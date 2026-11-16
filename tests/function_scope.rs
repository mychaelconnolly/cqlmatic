use serde_json::Value;
use std::collections::BTreeSet;

fn scope() -> Value {
    serde_json::from_str(include_str!("../data/function-scope.json")).unwrap()
}

#[test]
fn official_function_scope_counts_pages_rather_than_implemented_contracts() {
    let scope = scope();
    assert_eq!(scope["reviewed_at"], "2026-11-14");
    assert_eq!(
        scope["source"]["index_url"],
        "https://library.humio.com/crowdstrike-query-language/functions.html"
    );
    let functions = scope["functions"].as_array().unwrap();
    let mut names = BTreeSet::new();
    let mut urls = BTreeSet::new();
    let mut catalog_ids = BTreeSet::new();
    let mut candidates = 0;
    let mut deprecated = 0;
    let mut excluded = BTreeSet::new();
    for function in functions {
        let name = function["canonical_name"].as_str().unwrap();
        assert!(names.insert(name), "duplicate canonical name: {name}");
        let catalog_id = function["catalog_id"].as_str().unwrap();
        if function["status"] == "query_candidate" {
            let contract = cqlmatic::catalog::function(name).unwrap();
            assert_eq!(catalog_id, contract.reference_id);
            assert!(
                cqlmatic::catalog::records()
                    .iter()
                    .any(|record| record.id == catalog_id)
            );
        } else {
            assert_eq!(catalog_id, format!("cql:function:{name}"));
        }
        assert!(
            catalog_ids.insert(catalog_id),
            "duplicate catalog identity: {catalog_id}"
        );
        let url = function["official_url"].as_str().unwrap();
        assert!(urls.insert(url), "duplicate official page: {url}");
        assert_eq!(
            url,
            format!(
                "https://library.humio.com/crowdstrike-query-language/functions-{}.html",
                name.to_ascii_lowercase().replace(':', "-")
            )
        );
        assert_eq!(function["reviewed_at"], scope["reviewed_at"]);
        assert!(!function["reason"].as_str().unwrap().is_empty());
        assert!(function["conditions"].is_array());
        assert!(function["forms"].is_array());
        match function["status"].as_str().unwrap() {
            "query_candidate" => {
                candidates += 1;
                deprecated += usize::from(function["lifecycle"] == "deprecated");
                assert_eq!(function["query_applicability"], "current");
                let forms = function["forms"].as_array().unwrap();
                assert!(!forms.is_empty(), "missing form assessment: {name}");
                assert!(forms.iter().any(|form| {
                    form["id"] == "documented_signature"
                        && !form["signature"].as_str().unwrap().is_empty()
                }));
                let mut form_ids = BTreeSet::new();
                for form in forms {
                    assert!(form_ids.insert(form["id"].as_str().unwrap()));
                    assert!(
                        matches!(
                            form["assessment_status"].as_str(),
                            Some(
                                "offline_tested"
                                    | "runtime_pending"
                                    | "unsupported"
                                    | "unqualified"
                            )
                        ),
                        "{name}"
                    );
                    assert!(!form["reason"].as_str().unwrap().is_empty());
                    assert!(!form["qualification"].as_str().unwrap().is_empty());
                    assert!(!form["assessment_evidence"].as_array().unwrap().is_empty());
                    if let Some(signature) = form["signature"].as_str() {
                        assert!(
                            signature.starts_with(name) || signature == "fieldName =~ join(...)"
                        );
                        assert!(signature.ends_with(')'), "{name}: {signature}");
                        assert!(!signature.contains("Input Data"), "{name}: {signature}");
                        assert!(!signature.contains("Output Data"), "{name}: {signature}");
                        assert!(!signature.contains("Function Type"), "{name}: {signature}");
                    }
                }
            }
            "excluded" => {
                excluded.insert(name);
                assert_ne!(function["query_applicability"], "current");
            }
            status => panic!("unknown scope status: {status}"),
        }
    }
    assert_eq!(functions.len(), 196);
    assert_eq!(candidates, 194);
    assert_eq!(deprecated, 1);
    assert_eq!(excluded.len(), 2);
    assert_eq!(scope["counts"]["linked_function_pages"], 196);
    assert_eq!(scope["counts"]["current_query_candidates"], 193);
    assert_eq!(scope["counts"]["query_candidates"], 194);
    assert_eq!(scope["counts"]["deprecated_query_candidates"], 1);
    assert_eq!(scope["counts"]["excluded"], 2);
    assert_eq!(
        excluded,
        BTreeSet::from(["copyEvent", "math:spherical2cartesian"])
    );
    assert_eq!(scope["source"]["index_table_rows"], 189);
    let omitted = scope["source"]["linked_functions_outside_index_table"]
        .as_array()
        .unwrap();
    assert_eq!(omitted.len(), 7);
    for name in omitted {
        assert!(names.contains(name.as_str().unwrap()));
    }
}

#[test]
fn scope_retains_exclusions_and_conditional_forms() {
    let scope = scope();
    let functions = scope["functions"].as_array().unwrap();
    let function = |name: &str| {
        functions
            .iter()
            .find(|entry| entry["canonical_name"] == name)
            .unwrap()
    };
    for (name, applicability) in [
        ("copyEvent", "parser_only"),
        ("math:spherical2cartesian", "not_yet_available"),
    ] {
        assert_eq!(function(name)["query_applicability"], applicability);
    }
    assert_eq!(function("lowercase")["replacement"], "lower");
    assert_eq!(function("lowercase")["status"], "query_candidate");
    assert_eq!(function("lowercase")["query_applicability"], "current");
    assert_eq!(function("lowercase")["lifecycle"], "deprecated");
    assert_ne!(
        function("lowercase")["catalog_id"],
        function("lower")["catalog_id"]
    );
    assert_eq!(function("lower")["status"], "query_candidate");
    let has_condition = |entry: &Value, kind: &str, value: &str| {
        entry["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|condition| condition["kind"] == kind && condition["value"] == value)
    };
    for name in [
        "array:intersection",
        "array:reduceColumn",
        "array:reduceRow",
        "array:union",
    ] {
        assert!(has_condition(
            function(name),
            "feature_flag",
            "ArrayFunctions"
        ));
    }
    for (name, kind, value) in [
        ("beta:repeating", "feature_flag", "RepeatingQueries"),
        ("remoteTable", "product", "Falcon NG-SIEM"),
        ("remoteTable", "resource", "federated_connection"),
        (
            "readPersistedAggregation",
            "resource",
            "persisted_aggregation",
        ),
        ("explain:asTable", "lifecycle", "experimental"),
        ("explain:asTable", "usage", "ad_hoc_only"),
        ("ioc:lookup", "resource", "enabled_ioc_service"),
        ("asn", "data_freshness", "unexpired_maxmind_database"),
        ("ipLocation", "data_freshness", "unexpired_maxmind_database"),
    ] {
        assert!(
            has_condition(function(name), kind, value),
            "{name}: {value}"
        );
    }
    for (name, form, version) in [
        ("readPersistedAggregation", "stored_results", "1.252.0"),
        ("readPersistedAggregation", "rollup", "1.263.0"),
        ("wildcard", "expression", "1.265.0"),
    ] {
        let entry = function(name)["forms"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == form)
            .unwrap();
        assert!(has_condition(entry, "minimum_version", version));
    }
}

#[test]
fn scope_qualifies_alternative_resource_and_live_forms() {
    let scope = scope();
    let functions = scope["functions"].as_array().unwrap();
    let function = |name: &str| {
        functions
            .iter()
            .find(|entry| entry["canonical_name"] == name)
            .unwrap()
    };
    let form = |name: &str, id: &str| {
        function(name)["forms"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == id)
            .unwrap()
    };
    let has_condition = |entry: &Value, kind: &str, value: &str| {
        entry["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|condition| condition["kind"] == kind && condition["value"] == value)
    };
    for (id, signature, connection_type) in [
        (
            "cql_connection",
            "remoteTable(connection, [end], include, name, query, [start])",
            "CQL",
        ),
        (
            "other_connection",
            "remoteTable(connection, name, query)",
            "non_CQL",
        ),
    ] {
        let entry = form("remoteTable", id);
        assert_eq!(entry["signature"], signature);
        assert!(has_condition(entry, "connection_type", connection_type));
        assert!(has_condition(entry, "resource", "federated_connection"));
        assert!(!entry["constraints"].as_array().unwrap().is_empty());
    }
    assert_eq!(
        form("cidr", "inline_subnets")["signature"],
        "cidr(field, subnet)"
    );
    assert_eq!(
        form("cidr", "lookup_subnets")["signature"],
        "cidr(field, file, column)"
    );
    assert!(has_condition(
        form("cidr", "lookup_subnets"),
        "resource",
        "lookup_file"
    ));
    for name in ["match", "matchAsArray", "readFile"] {
        assert!(has_condition(
            form(name, "lookup_file"),
            "resource",
            "lookup_file"
        ));
        assert!(has_condition(
            form(name, "ad_hoc_table"),
            "resource",
            "ad_hoc_table"
        ));
    }
    assert!(has_condition(
        form("join", "live_repeated"),
        "query_mode",
        "live_repeated"
    ));
    assert!(has_condition(
        form("defineTable", "live_snapshot"),
        "query_mode",
        "live_snapshot"
    ));
    for name in [
        "accumulate",
        "neighbor",
        "partition",
        "slidingTimeWindow",
        "slidingWindow",
    ] {
        assert!(has_condition(
            function(name),
            "placement",
            "ordered_sequence_after_aggregate"
        ));
        assert!(has_condition(
            function(name),
            "compatibility",
            "no_multirow_stats_siblings"
        ));
    }
    for name in ["selfJoin", "selfJoinFilter"] {
        assert!(has_condition(function(name), "usage", "not_native_live"));
        assert!(has_condition(
            form(name, "live_repeating"),
            "feature_flag",
            "RepeatingQueries"
        ));
        assert!(has_condition(
            form(name, "live_repeating"),
            "lifecycle",
            "beta"
        ));
    }
    assert!(
        form("ioc:lookup", "negated")["constraints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|constraint| constraint.as_str().unwrap().contains("strict=true"))
    );
}

#[test]
fn parameter_facts_cover_candidates_without_promoting_open_choices_or_dynamic_limits() {
    let scope = scope();
    let facts: Value =
        serde_json::from_str(include_str!("../data/function-parameters.json")).unwrap();
    assert_eq!(facts["schema_version"], 1);
    let entries = facts["functions"].as_array().unwrap();
    assert_eq!(entries.len(), 194);
    let expected = scope["functions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["status"] == "query_candidate")
        .map(|entry| entry["canonical_name"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    let mut names = BTreeSet::new();
    for entry in entries {
        let name = entry["name"].as_str().unwrap();
        assert!(names.insert(name), "duplicate parameter function: {name}");
        assert_eq!(
            entry["official_url"],
            format!(
                "https://library.humio.com/crowdstrike-query-language/functions-{}.html",
                name.to_ascii_lowercase().replace(':', "-")
            )
        );
        let mut parameter_names = BTreeSet::new();
        for parameter in entry["parameters"].as_array().unwrap() {
            let parameter_name = parameter["name"].as_str().unwrap();
            assert!(!parameter_name.is_empty());
            assert!(
                parameter_names.insert(parameter_name),
                "duplicate {name} parameter: {parameter_name}"
            );
            assert_eq!(
                parameter
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([
                    "name",
                    "choices",
                    "minimum",
                    "maximum",
                    "default",
                    "omittable_name",
                ])
            );
            let mut choices = BTreeSet::new();
            for choice in parameter["choices"].as_array().unwrap() {
                let choice = choice.as_str().unwrap();
                assert!(!choice.is_empty());
                assert!(choices.insert(choice));
            }
            for bound in ["minimum", "maximum"] {
                assert!(parameter[bound].is_null() || parameter[bound].is_number());
            }
            if let (Some(minimum), Some(maximum)) =
                (parameter["minimum"].as_f64(), parameter["maximum"].as_f64())
            {
                assert!(minimum <= maximum, "{name}.{parameter_name}");
            }
            assert!(parameter["default"].is_null() || parameter["default"].is_string());
            assert!(parameter["omittable_name"].is_boolean());
        }
    }
    assert_eq!(names, expected);
    let parameter = |name: &str, parameter: &str| {
        entries.iter().find(|entry| entry["name"] == name).unwrap()["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == parameter)
            .unwrap()
    };
    assert_eq!(parameter("remoteTable", "query")["name"], "query");
    assert_eq!(
        parameter("text:editDistance", "maxDistance")["maximum"],
        100
    );
    for name in ["groupBy", "head", "tail", "sort", "table"] {
        assert!(parameter(name, "limit")["maximum"].is_null());
    }
    for (name, argument) in [
        ("regex", "flags"),
        ("replace", "flags"),
        ("array:regex", "flags"),
        ("parseTimestamp", "format"),
        ("sample", "percentage"),
        ("array:sort", "order"),
    ] {
        assert!(
            parameter(name, argument)["choices"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    assert!(parameter("matchAsArray", "asArray")["default"].is_null());
    assert!(parameter("text:editDistanceAsArray", "references")["maximum"].is_null());
    assert!(parameter("bucket", "buckets")["minimum"].is_null());
    assert_eq!(
        parameter("sort", "limit")["choices"],
        serde_json::json!(["max"])
    );
}

#[test]
fn parameter_name_omission_facts_agree_with_scope_annotations() {
    for function in cqlmatic::catalog::FUNCTIONS {
        let entry = cqlmatic::catalog::scope_entry(function.name).unwrap();
        let mut annotated = BTreeSet::new();
        for form in entry["forms"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|form| form["id"] == "omitted_argument_name")
        {
            let names = form["constraints"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|constraint| {
                    constraint
                        .as_str()
                        .unwrap()
                        .strip_prefix("The documented argument name may be omitted: ")
                        .and_then(|text| text.strip_suffix('.'))
                })
                .flat_map(|names| names.split(',').map(str::trim))
                .collect::<BTreeSet<_>>();
            assert!(
                !names.is_empty(),
                "{} has no omitted-name annotation",
                function.name
            );
            annotated.extend(names);
        }
        let facts = cqlmatic::catalog::parameter_rules(function.name)
            .iter()
            .filter(|rule| rule.omittable_name)
            .map(|rule| rule.name.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(facts, annotated, "{}", function.name);
    }
}

#[test]
fn form_dispositions_resolve_to_exact_evidence_and_preserve_runtime_limits() {
    let scope = scope();
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/function-coverage.json")).unwrap();
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for function in scope["functions"].as_array().unwrap() {
        let name = function["canonical_name"].as_str().unwrap();
        for form in function["forms"].as_array().unwrap() {
            let id = form["id"].as_str().unwrap();
            let status = form["assessment_status"].as_str().unwrap();
            *counts.entry(status).or_default() += 1;
            let runtime_mode = form["conditions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|condition| condition["kind"] == "query_mode");
            if runtime_mode {
                assert_eq!(
                    status, "runtime_pending",
                    "{name}/{id}: mode behavior needs execution"
                );
            }
            let mut positive = false;
            let mut refused = false;
            let mut reference = false;
            let evidence = form["assessment_evidence"].as_array().unwrap();
            assert!(!evidence.is_empty(), "{name}/{id}");
            for item in evidence {
                match item["kind"].as_str().unwrap() {
                    "fixture" => {
                        assert_eq!(item["file"], "tests/fixtures/function-coverage.json");
                        assert_eq!(
                            item["test"],
                            "qualified_synthetic_forms_check_and_round_trip_through_public_typed_lowering"
                        );
                        let case = fixture["cases"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|case| case["id"] == item["case_id"])
                            .unwrap_or_else(|| {
                                panic!("{name}/{id}: unresolved fixture {}", item["case_id"])
                            });
                        assert_eq!(case["function"], name);
                        assert!(
                            case["forms"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .any(|form| form == id)
                        );
                        positive = true;
                    }
                    "control" => {
                        assert_eq!(item["file"], "tests/fixtures/function-coverage.json");
                        assert_eq!(
                            item["test"],
                            "invalid_categories_and_unresolved_forms_keep_their_distinct_outcomes"
                        );
                        let control = fixture["controls"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|control| control["id"] == item["case_id"])
                            .unwrap_or_else(|| {
                                panic!("{name}/{id}: unresolved control {}", item["case_id"])
                            });
                        assert_eq!(control["function"], name);
                        assert!(matches!(
                            control["outcome"].as_str(),
                            Some("unsupported" | "invalid")
                        ));
                        assert!(
                            control["forms"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .any(|form| form == id)
                        );
                        refused = true;
                    }
                    "test" => {
                        assert_eq!(id, "output_assignment");
                        let file = item["file"].as_str().unwrap();
                        let test = item["test"].as_str().unwrap();
                        let source = match (file, test) {
                            (
                                "tests/math_functions.rs",
                                "every_math_function_has_lookup_and_checks_named_positional_scalar_and_output_forms"
                                | "typed_generation_uses_the_same_contract_and_rejects_predicate_context",
                            ) => {
                                assert!(
                                    name.starts_with("math:") && name != "math:spherical2cartesian"
                                );
                                include_str!("math_functions.rs")
                            }
                            (
                                "tests/text_time_functions.rs",
                                "calendar_functions_cover_defaults_options_and_scalar_assignments",
                            ) => {
                                assert!(name.starts_with("time:"));
                                include_str!("text_time_functions.rs")
                            }
                            (
                                "tests/text_time_functions.rs",
                                "text_computations_share_typed_raw_and_nested_contexts",
                            ) => {
                                assert!(matches!(
                                    name,
                                    "text:editDistance"
                                        | "text:positionOf"
                                        | "text:substring"
                                        | "text:trim"
                                ));
                                include_str!("text_time_functions.rs")
                            }
                            _ => panic!("{name}/{id}: unqualified test reference {file}::{test}"),
                        };
                        assert!(
                            source.contains(&format!("fn {test}(")),
                            "missing evidence test {file}::{test}"
                        );
                        positive = true;
                    }
                    "reference" => {
                        assert_eq!(item["url"], function["official_url"]);
                        assert!(!item["fact"].as_str().unwrap().is_empty());
                        reference = true;
                    }
                    kind => panic!("{name}/{id}: unknown evidence kind {kind}"),
                }
            }
            match status {
                "offline_tested" => assert!(positive, "{name}/{id}: no positive static evidence"),
                "runtime_pending" => assert!(
                    runtime_mode && positive,
                    "{name}/{id}: mode and static evidence are required"
                ),
                "unsupported" => assert!(
                    refused && reference && !positive,
                    "{name}/{id}: needs a documented valid shape and explicit refusal"
                ),
                "unqualified" => assert!(
                    reference && !positive && !refused,
                    "{name}/{id}: unresolved semantics require specific documentation evidence"
                ),
                _ => panic!("{name}/{id}: unresolved status {status}"),
            }
        }
    }
    assert_eq!(counts.values().sum::<usize>(), 519);
    assert_eq!(scope["assessment_summary"]["forms"], 519);
    for (status, count) in counts {
        assert_eq!(scope["assessment_summary"][status], count, "{status}");
    }
    assert!(
        scope["qualification"]
            .as_str()
            .unwrap()
            .contains("bounded offline")
    );
}
