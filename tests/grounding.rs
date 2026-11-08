use cqlmatic::{
    app,
    contracts::*,
    environment::{Pack, PackData},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

fn provenance(id: &str, source_type: &str, coverage: &str) -> Value {
    json!({
        "id": id,
        "source_type": source_type,
        "canonical_source": "synthetic grounding test",
        "retrieved_at": "2000-01-01T00:00:00Z",
        "redaction_status": "none",
        "coverage_status": coverage
    })
}

fn data() -> PackData {
    PackData {
        sources: vec![json!({
            "id": "env:repo:example",
            "kind": "repo",
            "name": "example",
            "inventory_status": "observed-only",
            "selectors": {"repo": "example"},
            "evidence": {"observed": true},
            "provenance_ids": ["prov:observation"]
        })],
        fields: vec![json!({
            "id": "env:field:example:user",
            "source_id": "env:repo:example",
            "path": "user",
            "data_type": "string",
            "is_tag": false,
            "evidence_kind": "observed",
            "coverage": {"observed": true},
            "provenance_ids": ["prov:observation"]
        })],
        provenance: vec![provenance("prov:observation", "ngsiem-api", "complete")],
        ..PackData::default()
    }
}

fn pack(data: PackData) -> Pack {
    Pack::new(
        EnvironmentId {
            cloud: Cloud::Us1,
            tenant: "0".repeat(32),
            view: "search-all".into(),
        },
        "2026-10-29T00:00:00Z".into(),
        data,
    )
    .unwrap()
}

fn request(require_observed: bool) -> GenerateRequest {
    GenerateRequest {
        schema_version: 1,
        id: None,
        input: GenerateInput::Plan {
            plan: Plan {
                source: Scope {
                    repo: Some("example".into()),
                    ..Scope::default()
                },
                filters: vec![Predicate::Exists {
                    field: "user".into(),
                    present: true,
                }],
                steps: vec![Stage::Count {
                    field: None,
                    distinct: false,
                    accuracy: Accuracy::Exact,
                    as_: "_count".into(),
                }],
            },
        },
        context: Context {
            require_observed,
            confirmed_fields: vec!["user".into()],
            ..Context::default()
        },
    }
}

fn analyze(data: PackData, require_observed: bool) -> Analysis {
    app::generate(request(require_observed), Some(&pack(data)))
}

#[test]
fn explicit_source_states_block_strict_observation() {
    for status in [
        "stale",
        "unavailable",
        "incomplete",
        "declared-only",
        "not-applicable",
    ] {
        let mut d = data();
        d.sources[0]["inventory_status"] = json!(status);
        let result = analyze(d, true);
        assert_eq!(result.outcome, Outcome::NeedsContext, "{status}");
        assert_eq!(result.validation.status, Acceptance::Accepted);
        assert!(result.query.is_none());
        if status == "stale" {
            assert!(!result.grounding.stale.is_empty());
        } else {
            assert!(!result.grounding.missing.is_empty());
        }
    }
}

#[test]
fn explicit_nonobservation_overrides_observed_labels() {
    let mut d = data();
    d.sources[0]["evidence"]["observed"] = json!(false);
    assert_eq!(analyze(d, true).outcome, Outcome::NeedsContext);

    let mut d = data();
    d.fields[0]["coverage"]["observed"] = json!(false);
    assert_eq!(analyze(d, true).outcome, Outcome::NeedsContext);
}

#[test]
fn stale_only_source_or_field_provenance_blocks_strict_observation() {
    for source_only in [false, true] {
        let mut d = data();
        d.provenance
            .push(provenance("prov:old", "ngsiem-api", "stale"));
        if source_only {
            d.sources[0]["provenance_ids"] = json!(["prov:old"]);
        } else {
            d.fields[0]["provenance_ids"] = json!(["prov:old"]);
        }
        let result = analyze(d, true);
        assert_eq!(result.outcome, Outcome::NeedsContext);
        assert!(!result.grounding.stale.is_empty());
        assert!(result.query.is_none());
    }
}

#[test]
fn incomplete_or_error_only_provenance_is_not_observation() {
    for coverage in [
        "incomplete",
        "error",
        "unavailable",
        "declared-only",
        "partial",
    ] {
        let mut d = data();
        d.provenance[0]["coverage_status"] = json!(coverage);
        let result = analyze(d, true);
        assert_eq!(result.outcome, Outcome::NeedsContext, "{coverage}");
        assert!(!result.grounding.missing.is_empty());
        assert!(result.query.is_none());
    }
}

#[test]
fn documented_or_declared_provenance_cannot_qualify_observed_flags() {
    for source_type in ["official-docs", "manual-curation"] {
        let mut d = data();
        d.provenance[0]["source_type"] = json!(source_type);
        let result = analyze(d, true);
        assert_eq!(result.outcome, Outcome::NeedsContext, "{source_type}");
        assert!(!result.grounding.missing.is_empty());
    }
}

#[test]
fn qualifying_observations_survive_retained_old_provenance_in_either_order() {
    for reverse in [false, true] {
        let mut d = data();
        for (id, kind, coverage) in [
            ("prov:old", "ngsiem-api", "stale"),
            ("prov:partial", "mcp-observation", "incomplete"),
            ("prov:failed", "ngsiem-api", "error"),
            ("prov:declaration", "manual-curation", "declared-only"),
        ] {
            d.provenance.push(provenance(id, kind, coverage));
            d.sources[0]["provenance_ids"]
                .as_array_mut()
                .unwrap()
                .push(json!(id));
            d.fields[0]["provenance_ids"]
                .as_array_mut()
                .unwrap()
                .push(json!(id));
        }
        if reverse {
            d.provenance.reverse();
        }
        let result = analyze(d, true);
        assert_eq!(result.outcome, Outcome::Ready);
        assert!(result.grounding.stale.is_empty());
        assert!(result.grounding.missing.is_empty());
        assert!(result.query.is_some());
    }
}

#[test]
fn historical_field_is_not_made_current_by_qualifying_provenance() {
    let mut d = data();
    d.fields[0]["refresh_status"] = json!("not-reobserved");
    let result = analyze(d, true);
    assert_eq!(result.outcome, Outcome::NeedsContext);
    assert!(!result.grounding.stale.is_empty());
    assert!(result.query.is_none());
}

#[test]
fn deployment_observation_types_qualify_without_a_clock_cutoff() {
    for source_type in ["ngsiem-api", "mcp-observation", "console-export"] {
        for coverage in ["complete", "observed-only", "reconciled"] {
            let mut d = data();
            d.provenance[0]["source_type"] = json!(source_type);
            d.provenance[0]["coverage_status"] = json!(coverage);
            assert_eq!(analyze(d, true).outcome, Outcome::Ready);
        }
    }
}

#[test]
fn strict_observation_is_opt_in_but_stale_basis_is_visible() {
    let mut d = data();
    d.sources[0]["inventory_status"] = json!("stale");
    let result = analyze(d, false);
    assert_eq!(result.outcome, Outcome::Ready);
    assert_eq!(
        result.grounding.basis["example:*:user"],
        "source marked stale"
    );
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-grounding-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn cli(command: &str, request: &Value, pack: &Path, json_output: bool) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cqlmatic"));
    child.args([command, "--request", "-", "--pack"]).arg(pack);
    if json_output {
        child.arg("--json");
    }
    let mut child = child
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(request).unwrap())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn missing_provenance_names_field_source_or_both_in_core_and_cli() {
    let dir = Temp::new();
    let path = dir.0.join("pack.json");
    let key = "example:*:user";
    for (source_type, coverage) in [
        ("manual-curation", "declared-only"),
        ("ngsiem-api", "partial"),
    ] {
        for (field_missing, source_missing, affected) in [
            (true, false, "field"),
            (false, true, "source"),
            (true, true, "field and source"),
        ] {
            let mut d = data();
            d.provenance
                .push(provenance("prov:unqualified", source_type, coverage));
            if field_missing {
                d.fields[0]["provenance_ids"] = json!(["prov:unqualified"]);
            }
            if source_missing {
                d.sources[0]["provenance_ids"] = json!(["prov:unqualified"]);
            }
            let pack = pack(d);
            let expected = format!(
                "no qualifying deployment observation provenance for {affected}; requires source_type ngsiem-api, mcp-observation, or console-export and coverage_status complete, observed-only, or reconciled"
            );
            let ordinary = app::generate(request(false), Some(&pack));
            assert_eq!(ordinary.outcome, Outcome::Ready);
            assert!(ordinary.grounding.missing.is_empty());
            assert_eq!(ordinary.grounding.basis[key], expected);
            let check_request = CheckRequest {
                schema_version: 1,
                id: None,
                query: ordinary.query.unwrap(),
                context: Context {
                    require_observed: true,
                    ..Context::default()
                },
            };
            for result in [
                app::generate(request(true), Some(&pack)),
                app::check(check_request.clone(), Some(&pack)),
            ] {
                assert_eq!(result.outcome, Outcome::NeedsContext);
                assert_eq!(result.validation.status, Acceptance::Accepted);
                assert!(result.query.is_none());
                assert_eq!(result.grounding.missing, [key]);
                assert!(result.grounding.stale.is_empty());
                assert_eq!(result.grounding.basis[key], expected);
                assert!(
                    result
                        .diagnostics
                        .iter()
                        .any(|d| d.code == "CQL_CONTEXT_REQUIRED")
                );
            }
            fs::write(&path, serde_json::to_vec(&pack).unwrap()).unwrap();
            for (command, payload) in [
                ("generate", serde_json::to_value(request(true)).unwrap()),
                ("check", serde_json::to_value(check_request).unwrap()),
            ] {
                for json_output in [false, true] {
                    let out = cli(command, &payload, &path, json_output);
                    assert_eq!(out.status.code(), Some(1));
                    if json_output {
                        let result: Analysis = serde_json::from_slice(&out.stdout).unwrap();
                        assert_eq!(result.outcome, Outcome::NeedsContext);
                        assert_eq!(result.validation.status, Acceptance::Accepted);
                        assert!(result.query.is_none());
                        assert_eq!(result.grounding.missing, [key]);
                        assert_eq!(result.grounding.basis[key], expected);
                    } else {
                        let stderr = String::from_utf8(out.stderr).unwrap();
                        assert!(stderr.contains("CQL_CONTEXT_REQUIRED:"));
                        assert!(stderr.contains(&format!("missing context: {key}: {expected}")));
                        if command == "generate" {
                            assert!(out.stdout.is_empty());
                        } else {
                            assert!(
                                String::from_utf8(out.stdout)
                                    .unwrap()
                                    .starts_with("needs context")
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn source_selector_built_in_does_not_ground_unobserved_deployment_fields() {
    let pack = pack(data());
    for deployment_field in [false, true] {
        let mut request = request(true);
        let GenerateInput::Plan { plan } = &mut request.input else {
            unreachable!()
        };
        plan.source.repo = Some("absent-example".into());
        if !deployment_field {
            plan.filters.clear();
        }
        let mut ordinary = request.clone();
        ordinary.context.require_observed = false;
        let query = app::generate(ordinary, Some(&pack)).query.unwrap();
        let generated = app::generate(request, Some(&pack));
        assert_eq!(generated.query.is_none(), deployment_field);
        let checked = app::check(
            CheckRequest {
                schema_version: 1,
                id: None,
                query,
                context: Context {
                    require_observed: true,
                    ..Context::default()
                },
            },
            Some(&pack),
        );
        for result in [generated, checked] {
            assert_eq!(result.validation.status, Acceptance::Accepted);
            assert_eq!(
                result.grounding.basis["absent-example:*:#repo"],
                "language-built-in"
            );
            if deployment_field {
                assert_eq!(result.outcome, Outcome::NeedsContext);
                assert_eq!(result.grounding.missing, ["absent-example:*:user"]);
            } else {
                assert_eq!(result.outcome, Outcome::Ready);
                assert!(result.grounding.missing.is_empty());
            }
        }
    }
}

#[test]
fn human_grounding_details_escape_terminal_controls_for_missing_and_stale_fields() {
    let dir = Temp::new();
    let path = dir.0.join("pack.json");
    let field = "user\u{1b}[2J\u{7}\n\r\t";
    let request = json!({
        "schema_version": 1,
        "query": format!(
            "#repo=\"example\" | {}=* | count(as=\"_count\")",
            cqlmatic::language::ast::field(field)
        ),
        "context": {"require_observed": true}
    });
    for (coverage, label, basis) in [
        (
            "partial",
            "missing context",
            "no qualifying deployment observation provenance for field and source",
        ),
        (
            "stale",
            "historical evidence",
            "observation evidence marked stale",
        ),
    ] {
        let mut d = data();
        d.fields[0]["path"] = json!(field);
        d.provenance[0]["coverage_status"] = json!(coverage);
        fs::write(&path, serde_json::to_vec(&pack(d)).unwrap()).unwrap();
        let out = cli("check", &request, &path, false);
        assert_eq!(out.status.code(), Some(1));
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert!(stderr.chars().all(|c| !c.is_control() || c == '\n'));
        assert!(stderr.contains(&format!(
            "{label}: example:*:user\\u{{1b}}[2J\\u{{7}}\\n\\r\\t: {basis}"
        )));
    }
}
