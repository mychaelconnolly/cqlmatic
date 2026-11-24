//! Every file under examples/ parses as the contract its path names. With the schema drift test
//! this ties each example to its Rust type and bundled schema; an unmapped file fails here.
use cqlmatic::{
    assess::AssessRequest,
    contracts::{CheckRequest, EnvironmentId, GenerateRequest, Plan, Recipe},
    discovery::DiscoverRequest,
    environment::{LegacyField, LegacyProvenance, LegacySource},
    inventory::{InventoryRequest, ParserPage, RawSnapshot, parse_catalog_page, parse_parser},
    mcp,
    setup::{HostToolDiscovery, SetupRequest},
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn strict<T: DeserializeOwned>(text: &str) -> Result<(), String> {
    serde_json::from_str::<T>(text)
        .map(drop)
        .map_err(|cause| cause.to_string())
}
fn lines<T: DeserializeOwned>(text: &str) -> Result<(), String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .try_for_each(strict::<T>)
}
fn snapshot(text: &str) -> Result<RawSnapshot, String> {
    RawSnapshot::new(text.as_bytes(), "2026-01-01T00:00:00Z").map_err(|d| d.message)
}
fn check(relative: &str, text: &str) -> Result<(), String> {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    match relative {
        // Prose, CQL text and plugin source carry no JSON contract.
        _ if [".md", ".cql", ".js"].iter().any(|ext| name.ends_with(ext)) => Ok(()),
        _ if relative.starts_with("setup/") && name.ends_with(".request.json") => {
            strict::<SetupRequest>(text)
        }
        _ if relative.starts_with("setup/tool-discovery") => strict::<HostToolDiscovery>(text),
        _ if relative.starts_with("inventory/") && name.ends_with(".request.json") => {
            strict::<InventoryRequest>(text)
        }
        "inventory/parser-page.synthetic.json" => parse_catalog_page(&ParserPage {
            offset: 0,
            limit: 100,
            snapshot: snapshot(text)?,
        })
        .map(drop)
        .map_err(|d| d.message),
        "inventory/parser.synthetic.json" => parse_parser("synthetic-parser-id", snapshot(text)?)
            .map(drop)
            .map_err(|d| d.message),
        "discover.request.json" | "mcp/discover.request.json" => strict::<DiscoverRequest>(text),
        _ if relative == "inventory/scout.synthetic.json" || relative.starts_with("mcp/") => {
            mcp::normalize(text.as_bytes())
                .map(drop)
                .map_err(|d| d.message)
        }
        "assess.request.json" => strict::<AssessRequest>(text),
        "environment.json" => strict::<EnvironmentId>(text),
        "checks.jsonl" => lines::<CheckRequest>(text),
        "requests.jsonl" | "query-reference/recipes.jsonl" => lines::<GenerateRequest>(text),
        "legacy-observation/sources.jsonl" => lines::<LegacySource>(text),
        "legacy-observation/fields.jsonl" => lines::<LegacyField>(text),
        "legacy-observation/provenance.jsonl" => lines::<LegacyProvenance>(text),
        _ if name.ends_with(".plan.json") => strict::<Plan>(text),
        _ if name.ends_with(".params.json") => {
            let params: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
            let recipe = name.trim_end_matches(".params.json");
            serde_json::from_value::<Recipe>(json!({"recipe": recipe, "params": params}))
                .map(drop)
                .map_err(|e| e.to_string())
        }
        _ if name.ends_with(".request.json") => strict::<GenerateRequest>(text),
        _ => Err("no contract mapping; add one to tests/examples.rs".into()),
    }
}
fn files(directory: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(&path, found);
        } else {
            found.push(path);
        }
    }
}
#[test]
fn every_example_parses_as_the_contract_its_name_maps_to() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut found = Vec::new();
    files(&root, &mut found);
    found.sort();
    let failures: Vec<_> = found
        .iter()
        .filter_map(|path| {
            let relative = path
                .strip_prefix(&root)
                .unwrap()
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let text = std::fs::read_to_string(path).unwrap();
            check(&relative, &text)
                .err()
                .map(|cause| format!("{relative}: {cause}"))
        })
        .collect();
    assert!(found.len() > 40, "examples were not found: {}", found.len());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
#[test]
fn unmapped_example_names_fail() {
    assert!(check("setup/new-shape.json", "{}").is_err());
    assert!(check("notes.txt", "").is_err());
}
#[test]
fn setup_schema_exposes_the_saved_host_report() {
    let schema: Value = serde_json::from_str(include_str!("../schemas/setup.json")).unwrap();
    assert_eq!(
        schema["properties"]["host_tool_discovery"]["$ref"],
        "#/$defs/HostToolDiscovery"
    );
    let report = &schema["$defs"]["HostToolDiscovery"];
    assert_eq!(report["additionalProperties"], false);
    for field in [
        "schema_version",
        "client",
        "server_name",
        "endpoint",
        "authenticated",
        "tools",
    ] {
        assert!(
            report["required"]
                .as_array()
                .unwrap()
                .contains(&json!(field)),
            "{field}"
        );
    }
}
