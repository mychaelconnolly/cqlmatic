// Regression: a pack written by discovery must verify when read back. The pack digest is computed over the
// re-serialized content, so fractional coverage rates must survive a JSON round trip bit for bit. Without
// serde_json's float_roundtrip feature some f64 values parse back one ULP off and the pack fails PACK_DIGEST
use cqlmatic::{
    contracts::{Cloud, EnvironmentId},
    environment::{Pack, PackData},
    storage,
};
use serde_json::json;
use std::path::PathBuf;

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cqlmatic-pack-roundtrip-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn fractional_coverage_rates_survive_write_and_read() {
    let mut fields = Vec::new();
    for total in 2..400u64 {
        let mut seen = std::collections::BTreeSet::new();
        for n in [1, total / 3, total / 2, total - 1] {
            if n == 0 || n >= total || !seen.insert(n) {
                continue;
            }
            fields.push(json!({
                "id": format!("env:field:example_repo:f{total}_{n}"),
                "source_id": "env:repo:example_repo",
                "path": format!("f{total}_{n}"),
                "data_type": "unknown",
                "is_tag": false,
                "is_array": null,
                "evidence_kind": "observed",
                "observation_window": "2026-01-01T00:45:00Z/2026-01-01T01:00:00Z",
                "coverage": {"declared": null, "observed": true, "rate": n as f64 / total as f64},
                "provenance_ids": ["prov:synthetic"]
            }));
        }
    }
    let pack = Pack::new(
        EnvironmentId {
            cloud: Cloud::Us1,
            tenant: "0".repeat(32),
            view: "search-all".into(),
        },
        "2026-01-01T01:00:00Z".into(),
        PackData {
            sources: vec![json!({
                "id": "env:repo:example_repo",
                "kind": "repo",
                "name": "example_repo",
                "inventory_status": "observed-only",
                "selectors": {"repo": "example_repo"},
                "provenance_ids": ["prov:synthetic"]
            })],
            fields,
            provenance: vec![json!({
                "id": "prov:synthetic",
                "source_type": "mcp-observation",
                "canonical_source": "synthetic round-trip test",
                "retrieved_at": "2026-01-01T01:00:00Z",
                "redaction_status": "none",
                "coverage_status": "observed-only"
            })],
            ..PackData::default()
        },
    )
    .unwrap_or_else(|d| panic!("{}: {}", d.code, d.message));

    let dir = temp_dir();
    let path = dir.join("pack.json");
    let writer = storage::Writer::acquire(&path).unwrap();
    writer.commit(&pack).unwrap();
    drop(writer);

    let read = storage::read_pack(&path).unwrap_or_else(|d| panic!("{}: {}", d.code, d.message));
    assert_eq!(read.content_digest, pack.content_digest);
    std::fs::remove_dir_all(dir).unwrap();
}
