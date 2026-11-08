use schemars::JsonSchema;
use std::path::Path;

#[derive(JsonSchema)]
#[allow(dead_code)]
struct AssessContract {
    request: crate::assess::AssessRequest,
    result: crate::assess::AssessResult,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct SetupContract {
    request: crate::setup::SetupRequest,
    result: crate::setup::SetupResult,
    state: crate::setup::SetupState,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct InventoryContract {
    request: crate::inventory::InventoryRequest,
    state: crate::inventory::InventoryState,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct StatusContract {
    request: crate::inventory::StatusRequest,
    result: crate::inventory::InventoryStatus,
}

fn schema<T: JsonSchema>(name: &str) {
    let data = serde_json::to_string_pretty(&schemars::schema_for!(T)).unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("schemas")
        .join(format!("{name}.json"));
    if std::env::var_os("UPDATE_SCHEMAS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &data).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        data,
        "Schema drift; regenerate explicitly with UPDATE_SCHEMAS=1."
    );
}
#[test]
fn contracts_match_bundled_schemas() {
    schema::<crate::contracts::GenerateRequest>("generate");
    schema::<crate::contracts::CheckRequest>("check");
    schema::<crate::contracts::Analysis>("result");
    schema::<crate::environment::Pack>("pack");
    schema::<crate::discovery::DiscoverRequest>("discovery");
    schema::<AssessContract>("assess");
    schema::<SetupContract>("setup");
    schema::<InventoryContract>("inventory");
    schema::<StatusContract>("status");
}
