use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-observation-{}-{}",
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

fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cqlmatic"));
    command
        .env_remove("FALCON_CLIENT_ID")
        .env_remove("FALCON_CLIENT_SECRET");
    command
}

#[test]
fn checked_in_observation_imports_inspects_and_grounds_without_changing_inputs() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let legacy = root.join("examples/legacy-observation");
    let environment = root.join("examples/environment.json");
    let inputs = [
        legacy.join("sources.jsonl"),
        legacy.join("fields.jsonl"),
        legacy.join("provenance.jsonl"),
        environment.clone(),
    ];
    let original = inputs.each_ref().map(|path| fs::read(path).unwrap());
    let dir = Temp::new();
    let pack = dir.0.join("imported.json");
    let imported = cli()
        .args(["env", "import", "--from"])
        .arg(&legacy)
        .arg("--environment")
        .arg(&environment)
        .arg("--out")
        .arg(&pack)
        .output()
        .unwrap();
    assert!(imported.status.success(), "{:?}", imported);
    let result: Value = serde_json::from_slice(&imported.stdout).unwrap();
    assert_eq!(result["status"], "imported");
    assert_eq!(result["source_files_modified"], false);

    let inspected = cli()
        .args(["env", "inspect"])
        .arg(&pack)
        .args(["--source", "env:repo:example_repo", "--field", "user"])
        .output()
        .unwrap();
    assert!(inspected.status.success(), "{:?}", inspected);
    let result: Value = serde_json::from_slice(&inspected.stdout).unwrap();
    assert_eq!(result["fields"].as_array().unwrap().len(), 1);
    assert_eq!(
        result["fields"][0],
        serde_json::from_slice::<Value>(&original[1]).unwrap()
    );

    let checked = cli()
        .args(["check", "#repo=example_repo | user=* | count()", "--pack"])
        .arg(&pack)
        .args(["--require-observed", "--json"])
        .output()
        .unwrap();
    assert!(checked.status.success(), "{:?}", checked);
    let result: Value = serde_json::from_slice(&checked.stdout).unwrap();
    assert_eq!(result["outcome"], "ready");
    assert_eq!(result["validation"]["execution"], "not-run");
    assert_eq!(
        result["grounding"]["basis"]["example_repo:*:user"],
        "observed in a bounded sample"
    );
    for (path, bytes) in inputs.iter().zip(original) {
        assert_eq!(fs::read(path).unwrap(), bytes, "{}", path.display());
    }
}

#[test]
fn legacy_import_requires_an_explicit_environment_before_creating_output() {
    let legacy = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/legacy-observation");
    let dir = Temp::new();
    let pack = dir.0.join("packs/imported.json");
    let output = cli()
        .args(["env", "import", "--from"])
        .arg(legacy)
        .arg("--out")
        .arg(&pack)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["code"], "CLI_INPUT");
    assert!(error["message"].as_str().unwrap().contains("--environment"));
    assert!(!pack.parent().unwrap().exists());
}
