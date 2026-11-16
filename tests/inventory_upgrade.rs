#![cfg(unix)]

use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

#[path = "support/inventory_release.rs"]
mod released;

const PREVIOUS_PRODUCER: &str = "0.1.6";

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-upgrade-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn version_parts(version: &str) -> Option<[u64; 3]> {
    version
        .split('.')
        .map(str::parse)
        .collect::<Result<Vec<_>, _>>()
        .ok()?
        .try_into()
        .ok()
}

fn reader_version_for(package: &str, previous: &str) -> Option<String> {
    let current = version_parts(package)?;
    let mut prior = version_parts(previous)?;
    match current.cmp(&prior) {
        std::cmp::Ordering::Less => None,
        std::cmp::Ordering::Greater => Some(package.into()),
        std::cmp::Ordering::Equal => {
            prior[2] = prior[2].checked_add(1)?;
            Some(format!("{}.{}.{}", prior[0], prior[1], prior[2]))
        }
    }
}

fn source_fingerprint(source: &Path) -> String {
    fn collect(path: &Path, files: &mut Vec<PathBuf>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        if metadata.is_dir() {
            for child in fs::read_dir(path).unwrap() {
                collect(&child.unwrap().path(), files);
            }
        } else {
            assert!(metadata.is_file(), "Build inputs must be ordinary files");
            files.push(path.into());
        }
    }
    let mut files = Vec::new();
    for input in [
        "Cargo.toml",
        "Cargo.lock",
        "build.rs",
        "dev",
        "src",
        "data",
        "schemas",
        "docs",
        ".cargo",
        "rust-toolchain.toml",
    ] {
        let path = source.join(input);
        if path.exists() {
            collect(&path, &mut files);
        }
    }
    files.sort();
    let mut bytes = Vec::new();
    for file in files {
        let content = fs::read(&file).unwrap();
        bytes.extend_from_slice(
            file.strip_prefix(source)
                .unwrap()
                .as_os_str()
                .as_encoded_bytes(),
        );
        bytes.push(0);
        bytes.extend_from_slice(&(content.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&content);
    }
    cqlmatic::sha256_hex(&bytes)
}

fn build_directory(source: &Path, version: &str, _label: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut identity = fs::canonicalize(source.join("Cargo.toml"))
        .unwrap()
        .as_os_str()
        .as_encoded_bytes()
        .to_vec();
    identity.push(0);
    identity.extend_from_slice(version.as_bytes());
    root.join("target/producer-compatibility/artifacts")
        .join(cqlmatic::sha256_hex(&identity))
}

#[test]
fn build_cache_tracks_source_identity() {
    let first = Temp::new();
    let second = Temp::new();
    for directory in [&first.0, &second.0] {
        fs::write(
            directory.join("Cargo.toml"),
            "[package]\nname='synthetic'\nversion='0.1.6'\n",
        )
        .unwrap();
    }
    let initial = build_directory(&first.0, "0.1.6", "writer");
    assert_ne!(
        initial,
        build_directory(&second.0, "0.1.6", "writer"),
        "Different source trees must never share a cached executable"
    );
    assert_ne!(initial, build_directory(&first.0, "0.1.7", "writer"));
    let fingerprint = source_fingerprint(&first.0);
    fs::write(
        first.0.join("Cargo.toml"),
        "[package]\nname='changed'\nversion='0.1.6'\n",
    )
    .unwrap();
    assert_ne!(fingerprint, source_fingerprint(&first.0));
}

#[test]
fn reader_selection_survives_the_package_version_bump() {
    assert_eq!(
        reader_version_for("0.1.6", "0.1.6").as_deref(),
        Some("0.1.7")
    );
    assert_eq!(
        reader_version_for("0.1.7", "0.1.6").as_deref(),
        Some("0.1.7")
    );
    assert_eq!(
        reader_version_for("0.1.8", "0.1.6").as_deref(),
        Some("0.1.8")
    );
    assert_eq!(reader_version_for("0.1.5", "0.1.6"), None);
}

fn manifest_version(source: &Path) -> String {
    let manifest = fs::read_to_string(source.join("Cargo.toml")).unwrap();
    let package = manifest
        .split_once("[package]")
        .expect("Writer needs an explicit package")
        .1;
    package
        .lines()
        .take_while(|line| !line.trim().starts_with('['))
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "version").then(|| value.trim().trim_matches(['\"', '\'']).to_string())
        })
        .expect("Writer needs an explicit package version")
}

fn verify_binary(binary: &Path, version: &str) {
    let output = Command::new(binary).arg("--version").output().unwrap();
    assert!(
        output.status.success(),
        "{} --version failed: {output:?}",
        binary.display()
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!("cqlmatic {version}")
    );
}

fn verify_current_profile(binary: &Path) {
    let output = Command::new(binary)
        .args(["catalog", "--profile"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let profile: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(profile["profile_id"], cqlmatic::contracts::PROFILE);
    assert_eq!(
        profile["knowledge_revision"],
        cqlmatic::environment::KNOWLEDGE_REVISION
    );
}

fn snapshot_binary(
    binary: &Path,
    source: &Path,
    directory: &Path,
    version: &str,
    fingerprint: &str,
    label: &str,
    input_sha256: Option<&str>,
) -> PathBuf {
    fs::create_dir_all(directory).unwrap();
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    let published = directory.join("cqlmatic");
    let stage = directory.join(format!("{}-{label}.stage", std::process::id()));
    let existing = published.is_file();
    let candidate = if existing {
        &published
    } else {
        if stage.exists() {
            fs::remove_file(&stage).unwrap();
        }
        fs::copy(binary, &stage).unwrap();
        fs::set_permissions(&stage, fs::Permissions::from_mode(0o700)).unwrap();
        &stage
    };
    if let Some(expected) = input_sha256 {
        assert_eq!(
            cqlmatic::sha256_hex(&fs::read(candidate).unwrap()),
            expected,
            "Cargo executable changed during snapshot"
        );
        assert_eq!(
            cqlmatic::sha256_hex(&fs::read(binary).unwrap()),
            expected,
            "Cargo executable changed during snapshot"
        );
    }
    verify_binary(candidate, version);
    if fs::canonicalize(source).unwrap() == fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap() {
        verify_current_profile(candidate);
    }
    if source_fingerprint(source) != fingerprint {
        if !existing {
            fs::remove_file(&stage).unwrap();
        }
        panic!("Source changed during compilation or snapshot; discard this run");
    }
    if !existing {
        fs::rename(&stage, &published).unwrap();
    }
    published
}

fn cargo_command(source: &Path, target: &Path) -> Command {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut command = Command::new(root.join("dev"));
    command
        .current_dir(source)
        .env("CARGO_TARGET_DIR", target)
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER");
    command
}

fn compilation_lock() -> (PathBuf, fs::File) {
    let shared =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target/producer-compatibility/compilation");
    fs::create_dir_all(&shared).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(shared.join("build.lock"))
        .unwrap();
    lock.lock().unwrap();
    (shared, lock)
}

fn build(source: &Path, version: &str, label: &str) -> PathBuf {
    let source = fs::canonicalize(source).unwrap();
    let (shared, _lock) = compilation_lock();
    let fingerprint = source_fingerprint(&source);
    let directory = build_directory(&source, version, label).join(&fingerprint);
    let artifact = directory.join("cqlmatic");
    if !artifact.is_file() {
        fs::create_dir_all(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        // Evict this package, retaining dependency compilation across source switches.
        let output = cargo_command(&source, &shared)
            .args([
                "clean",
                "--offline",
                "--locked",
                "-p",
                "cqlmatic",
                "--manifest-path",
            ])
            .arg(source.join("Cargo.toml"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut command = cargo_command(&source, &shared);
        command
            .args([
                "build",
                "--offline",
                "--locked",
                "--bin",
                "cqlmatic",
                "--manifest-path",
            ])
            .arg(source.join("Cargo.toml"));
        if manifest_version(&source) != version {
            let [major, minor, patch] =
                version_parts(version).expect("Simulation needs a stable numeric version");
            let compiler = directory.join("compiler");
            fs::write(&compiler, format!("#!/bin/sh\nif [ \"${{CARGO_PKG_NAME:-}}\" = cqlmatic ]; then\nexport CARGO_PKG_VERSION={version} CARGO_PKG_VERSION_MAJOR={major} CARGO_PKG_VERSION_MINOR={minor} CARGO_PKG_VERSION_PATCH={patch} CARGO_PKG_VERSION_PRE=\nfi\nexec \"$@\"\n")).unwrap();
            fs::set_permissions(&compiler, fs::Permissions::from_mode(0o700)).unwrap();
            command.env("RUSTC_WRAPPER", compiler);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return snapshot_binary(
            &shared.join("debug/cqlmatic"),
            &source,
            &directory,
            version,
            &fingerprint,
            label,
            None,
        );
    }
    snapshot_binary(
        &artifact,
        &source,
        &directory,
        version,
        &fingerprint,
        label,
        None,
    )
}

fn current_binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let (_shared, _lock) = compilation_lock();
        let before = source_fingerprint(root);
        let binary = Path::new(env!("CARGO_BIN_EXE_cqlmatic"));
        let binary_sha256 = cqlmatic::sha256_hex(&fs::read(binary).unwrap());
        let directory = build_directory(root, env!("CARGO_PKG_VERSION"), "current")
            .join(format!("{before}-{binary_sha256}"));
        snapshot_binary(
            binary,
            root,
            &directory,
            env!("CARGO_PKG_VERSION"),
            &before,
            "current",
            Some(&binary_sha256),
        )
    })
}

// The default uses this checkout only; preserved-release testing is an explicit ignored case.
fn old() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    assert!(
        std::env::var_os("CQLMATIC_PREVIOUS_WRITER_MANIFEST").is_none(),
        "Run genuine_previous_release with --ignored instead of the relabelled cases"
    );
    BINARY.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        if env!("CARGO_PKG_VERSION") == PREVIOUS_PRODUCER {
            current_binary().into()
        } else {
            build(root, PREVIOUS_PRODUCER, "writer")
        }
    })
}
fn upgraded() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        let version = reader_version_for(env!("CARGO_PKG_VERSION"), PREVIOUS_PRODUCER)
            .expect("Update previous-producer fixtures before testing an older package");
        if version == env!("CARGO_PKG_VERSION") {
            current_binary().into()
        } else {
            build(Path::new(env!("CARGO_MANIFEST_DIR")), &version, "reader")
        }
    })
}

fn released_writer() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        let manifest = fs::canonicalize(
            std::env::var_os("CQLMATIC_PREVIOUS_WRITER_MANIFEST").expect(
                "Set CQLMATIC_PREVIOUS_WRITER_MANIFEST to the preserved release Cargo.toml",
            ),
        )
        .unwrap();
        let source = manifest.parent().unwrap();
        assert_eq!(
            manifest_version(source),
            PREVIOUS_PRODUCER,
            "Preserved writer must be the maintained previous release"
        );
        build(source, PREVIOUS_PRODUCER, "release")
    })
}
fn environment() -> Value {
    json!({"cloud":"us1","tenant":"00000000000000000000000000000000","view":"search-all"})
}
fn state(dir: &Path) -> PathBuf {
    dir.join("workflow/state.json")
}
fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn write(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}
fn command(binary: &Path, dir: &Path, action: Value) -> Output {
    command_with_flag(binary, dir, action, None)
}
fn command_with_flag(binary: &Path, dir: &Path, action: Value, flag: Option<&str>) -> Output {
    let request = dir.join("request.json");
    write(&request, &json!({"schema_version":1,"action":action}));
    let mut command = Command::new(binary);
    command
        .args(["env", "inventory", "--json", "--state"])
        .arg(state(dir))
        .arg("--request")
        .arg(request);
    if let Some(flag) = flag {
        command.arg(flag);
    }
    command.output().unwrap()
}
fn call(binary: &Path, dir: &Path, action: Value) -> Value {
    let output = command(binary, dir, action);
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}
fn setup(dir: &Path) {
    call(
        old(),
        dir,
        json!({"kind":"set-discovery-mode","mode":"quick","quick_depth":"sources-only"}),
    );
    call(
        old(),
        dir,
        json!({"kind":"confirm-target","environment":environment(),"scope":"selected-sources","context":"Synthetic discovery target","expected_sources":[{"type":"synthetic"}]}),
    );
}
fn prepare(binary: &Path, dir: &Path, queries: u32) -> Value {
    call(
        binary,
        dir,
        json!({"kind":"prepare-discovery","start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z","max_queries":queries,"max_seconds":120}),
    )
}
fn checkpoint(dir: &Path, index: usize) -> PathBuf {
    PathBuf::from(
        read(&state(dir))["discovery_runs"][index]["state_path"]
            .as_str()
            .unwrap(),
    )
}
fn answer(binary: &Path, dir: &Path, prepared: &Value, response: Value) -> Value {
    let index = prepared["action"]["run_index"].clone();
    let probe = prepared["action"]["relay"]["probes"][0]["probe_id"].clone();
    call(
        binary,
        dir,
        json!({"kind":"dispatch-discovery","run_index":index,"probe_id":probe}),
    );
    let file = dir.join("answer.json");
    write(&file, &response);
    call(
        binary,
        dir,
        json!({"kind":"discovery-step","run_index":index,"answers":[{"probe_id":probe,"result_file":file}]}),
    )
}
fn complete(binary: &Path, dir: &Path, prepared: &Value) {
    let result = answer(binary, dir, prepared, json!({"result":[{"events":10}]}));
    assert_eq!(result["action"]["relay"]["status"], "complete");
}
fn scout() -> Value {
    json!({"kind":"prepare-scout","environment":environment(),"start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z","max_seconds":60})
}
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut paths = vec![state(dir)];
    for run in read(&state(dir))["discovery_runs"].as_array().unwrap() {
        paths.push(PathBuf::from(run["state_path"].as_str().unwrap()));
    }
    paths
        .into_iter()
        .map(|path| {
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect()
}
fn unchanged(before: &[(PathBuf, Vec<u8>)]) {
    for (path, bytes) in before {
        assert!(
            fs::read(path).unwrap() == *bytes,
            "Changed protected bytes: {}",
            path.display()
        );
    }
}
fn reject(binary: &Path, dir: &Path, action: Value, code: &str) {
    let before = snapshot(dir);
    let output = command(binary, dir, action.clone());
    unchanged(&before);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["code"], code, "{action}: {diagnostic}");
}
fn remove_owner(path: &Path) {
    let mut checkpoint = read(path);
    checkpoint["session"]
        .as_object_mut()
        .unwrap()
        .remove("inventory_owner");
    let mut binding = serde_json::to_vec(&(
        &checkpoint["request_sha256"],
        &checkpoint["created_at"],
        &checkpoint["pack"],
        &checkpoint["receipt"],
        &checkpoint["session"]["transport"],
        &checkpoint["session"]["predecessor_sha256"],
    ))
    .unwrap();
    if checkpoint["session"]["sparse_fieldset_fallback"] == true {
        binding.extend_from_slice(b"\0sparse-fieldset-fallback-v1");
    }
    checkpoint["session"]["binding_sha256"] = json!(cqlmatic::sha256_hex(&binding));
    write(path, &checkpoint);
}

#[test]
fn bound_paused_previous_producer_allows_unrelated_scout() {
    let dir = Temp::new();
    setup(&dir.0);
    prepare(old(), &dir.0, 4);
    call(old(), &dir.0, json!({"kind":"pause-discovery"}));
    let path = checkpoint(&dir.0, 0);
    let retained = fs::read(&path).unwrap();
    call(old(), &dir.0, scout());
    call(upgraded(), &dir.0, scout());
    assert_eq!(fs::read(&path).unwrap(), retained);
}

#[test]
fn foreign_completed_step_preserves_version_three_inventory_and_every_checkpoint() {
    let dir = Temp::new();
    setup(&dir.0);
    let prepared = prepare(old(), &dir.0, 4);
    complete(old(), &dir.0, &prepared);
    prepare(old(), &dir.0, 5);
    let mut inventory = read(&state(&dir.0));
    inventory["schema_version"] = json!(3);
    inventory
        .as_object_mut()
        .unwrap()
        .remove("field_evidence_schema_version");
    write(&state(&dir.0), &inventory);
    reject(
        upgraded(),
        &dir.0,
        json!({"kind":"discovery-step","run_index":0,"answers":[]}),
        "MCP_STATE_PRODUCER",
    );
}

#[test]
fn foreign_completed_resume_does_not_associate_current_unowned_sibling() {
    let dir = Temp::new();
    setup(&dir.0);
    let prepared = prepare(old(), &dir.0, 4);
    complete(old(), &dir.0, &prepared);
    // Prepare the sibling before retaining the older completed checkpoint.
    let path = checkpoint(&dir.0, 0);
    let old_bytes = fs::read(&path).unwrap();
    let mut inventory = read(&state(&dir.0));
    let history = inventory["discovery_runs"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    write(&state(&dir.0), &inventory);
    prepare(upgraded(), &dir.0, 5);
    let sibling = checkpoint(&dir.0, 0);
    remove_owner(&sibling);
    let mut inventory = read(&state(&dir.0));
    inventory["discovery_runs"]
        .as_array_mut()
        .unwrap()
        .insert(0, history);
    write(&state(&dir.0), &inventory);
    fs::write(path, old_bytes).unwrap();
    reject(
        upgraded(),
        &dir.0,
        json!({"kind":"resume-discovery","run_index":0}),
        "MCP_STATE_PRODUCER",
    );
}

const PHASES: [&str; 8] = [
    "needs-probes",
    "paused",
    "cancel-requested",
    "cleanup-pending",
    "activation-blocked",
    "activating",
    "activated",
    "failed",
];
const ROUTES: [&str; 9] = [
    "prepare-discovery",
    "dispatch-discovery",
    "resume-discovery",
    "prepare-refresh",
    "dispatch-refresh",
    "prepare-scout",
    "prepare-capabilities",
    "dispatch-capability",
    "resume-capabilities",
];

fn retained_phase(dir: &Path, phase: &str) -> Value {
    setup(dir);
    let baseline = prepare(old(), dir, 4);
    complete(old(), dir, &baseline);
    let prepared = prepare(old(), dir, 5);
    let path = checkpoint(dir, 1);
    let before = fs::read(state(dir)).unwrap();
    match phase {
        "needs-probes" => {}
        "paused" => {
            call(old(), dir, json!({"kind":"pause-discovery","run_index":1}));
        }
        "cancel-requested" => {
            call(
                old(),
                dir,
                json!({"kind":"dispatch-discovery","run_index":1,"probe_id":prepared["action"]["relay"]["probes"][0]["probe_id"]}),
            );
            call(old(), dir, json!({"kind":"pause-discovery","run_index":1}));
        }
        "cleanup-pending" => {
            answer(
                old(),
                dir,
                &prepared,
                json!({"result":[],"error":"Synthetic timeout"}),
            );
        }
        "activation-blocked" => {
            let pack = PathBuf::from(read(&path)["pack"].as_str().unwrap());
            let guard = cqlmatic::storage::Writer::acquire(&pack).unwrap();
            let result = answer(old(), dir, &prepared, json!({"result":[{"events":10}]}));
            assert_eq!(result["action"]["relay"]["status"], "activation-blocked");
            drop(guard);
        }
        "activated" => complete(old(), dir, &prepared),
        "activating" => {
            complete(old(), dir, &prepared);
            let mut checkpoint = read(&path);
            // Reconstruct frozen replacement before receipt and inventory completion.
            checkpoint["status"] = json!("activating");
            checkpoint["pack_sha256"] = Value::Null;
            fs::remove_file(checkpoint["receipt"].as_str().unwrap()).unwrap();
            write(&path, &checkpoint);
            fs::write(state(dir), before).unwrap();
        }
        "failed" => {
            let receipt = PathBuf::from(read(&path)["receipt"].as_str().unwrap());
            write(&receipt, &json!({"synthetic":"occupied"}));
            fs::set_permissions(&receipt, fs::Permissions::from_mode(0o600)).unwrap();
            let result = answer(old(), dir, &prepared, json!({"result":[{"events":10}]}));
            assert_eq!(result["action"]["relay"]["status"], "failed");
        }
        _ => unreachable!(),
    }
    let raw = read(&path);
    assert_eq!(raw["producer_version"], PREVIOUS_PRODUCER);
    assert_eq!(raw["schema_version"], 3);
    assert_eq!(raw["status"], phase);
    assert_eq!(
        raw["session"]["inventory_owner"],
        json!(fs::canonicalize(state(dir)).unwrap())
    );
    read(&state(dir))["discovery_runs"][1].clone()
}
fn host(dir: &Path) -> PathBuf {
    let path = dir.join("host.json");
    write(
        &path,
        &json!({"schema_version":1,"environment":environment(),"session_id":"synthetic-session","verified_at":"2026-01-01T00:00:00Z","mechanism":"provider-enforced-search-lifetime","max_search_seconds":60,"capability_reference":"Synthetic provider capability","verification_evidence":"Synthetic session verification of provider search lifetime"}),
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    path
}
fn capability(dir: &Path) -> Value {
    json!({"kind":"prepare-capabilities","execution_context_id":"synthetic-auth","session_id":"synthetic-session","case_ids":["array-union"],"host_evidence_file":host(dir),"max_queries":2,"max_seconds":120})
}
fn route(binary: &Path, dir: &Path, kind: &str) -> Value {
    match kind {
        "prepare-discovery" => {
            json!({"kind":kind,"start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z","max_queries":7,"max_seconds":120})
        }
        "dispatch-discovery" | "resume-discovery" => {
            let prepared = prepare(binary, dir, 6);
            if kind == "resume-discovery" {
                call(binary, dir, json!({"kind":"pause-discovery","run_index":0}));
                json!({"kind":kind,"run_index":0})
            } else {
                json!({"kind":kind,"run_index":0,"probe_id":prepared["action"]["relay"]["probes"][0]["probe_id"]})
            }
        }
        "prepare-refresh" => {
            json!({"kind":kind,"session_id":"synthetic-session","host_evidence_file":host(dir),"automatic":false})
        }
        "dispatch-refresh" => {
            let prepared = call(
                binary,
                dir,
                json!({"kind":"prepare-refresh","session_id":"synthetic-session","host_evidence_file":host(dir),"automatic":false}),
            );
            json!({"kind":kind,"attempt_id":prepared["action"]["attempt_id"],"session_id":"synthetic-session"})
        }
        "prepare-scout" => scout(),
        "prepare-capabilities" => capability(dir),
        "dispatch-capability" | "resume-capabilities" => {
            let prepared = call(binary, dir, capability(dir));
            let run_id = prepared["action"]["capability"]["run_id"].clone();
            if kind == "resume-capabilities" {
                call(
                    binary,
                    dir,
                    json!({"kind":"pause-capabilities","run_id":run_id}),
                );
                json!({"kind":kind,"run_id":run_id,"execution_context_id":"synthetic-auth"})
            } else {
                json!({"kind":kind,"run_id":run_id,"case_id":"array-union","execution_context_id":"synthetic-auth","session_id":"synthetic-session"})
            }
        }
        _ => unreachable!(),
    }
}

#[test]
fn retained_history_covers_every_guarded_entrypoint() {
    for unowned_paused in [false, true] {
        for kind in ROUTES {
            let dir = Temp::new();
            let history = if unowned_paused {
                unowned_history(&dir.0, "paused")
            } else {
                retained_phase(&dir.0, "activated")
            };
            let mut inventory = read(&state(&dir.0));
            inventory["discovery_runs"] = json!([]);
            write(&state(&dir.0), &inventory);
            let action = route(upgraded(), &dir.0, kind);
            let mut inventory = read(&state(&dir.0));
            inventory["discovery_runs"]
                .as_array_mut()
                .unwrap()
                .push(history.clone());
            write(&state(&dir.0), &inventory);
            if unowned_paused {
                reject(upgraded(), &dir.0, action, "MCP_STATE_PRODUCER");
            } else {
                let path = Path::new(history["state_path"].as_str().unwrap());
                let before = fs::read(path).unwrap();
                call(upgraded(), &dir.0, action);
                assert!(fs::read(path).unwrap() == before);
            }
        }
    }
}

#[test]
#[ignore = "Exhaustive current-feature matrix; run exhaustive_policy with the previous-writer variable unset"]
fn exhaustive_policy_bound_lifecycle_matches_current_producer() {
    for phase in PHASES {
        for kind in ROUTES {
            for binary in [old(), upgraded()] {
                let dir = Temp::new();
                let history = retained_phase(&dir.0, phase);
                // Prepare selected work before retaining history that may block it.
                let mut inventory = read(&state(&dir.0));
                inventory["discovery_runs"] = json!([]);
                write(&state(&dir.0), &inventory);
                let action = route(binary, &dir.0, kind);
                let mut inventory = read(&state(&dir.0));
                inventory["discovery_runs"]
                    .as_array_mut()
                    .unwrap()
                    .push(history);
                write(&state(&dir.0), &inventory);
                let path = checkpoint(
                    &dir.0,
                    inventory["discovery_runs"].as_array().unwrap().len() - 1,
                );
                let retained = fs::read(&path).unwrap();
                let allowed = matches!(phase, "paused" | "activated" | "failed");
                if allowed {
                    call(binary, &dir.0, action);
                    assert!(fs::read(&path).unwrap() == retained, "{phase}: {kind}");
                } else {
                    reject(binary, &dir.0, action, "INVENTORY_INPUT");
                }
            }
        }
    }
}

#[test]
fn selected_previous_producer_actions_refuse_every_phase_without_writes() {
    for phase in PHASES {
        let dir = Temp::new();
        retained_phase(&dir.0, phase);
        for kind in [
            "dispatch-discovery",
            "resume-discovery",
            "discovery-step",
            "pause-discovery",
        ] {
            let mut action = json!({"kind":kind,"run_index":1});
            if kind == "dispatch-discovery" {
                action["probe_id"] = json!("p-0000000000000000");
            } else if kind == "discovery-step" {
                action["answers"] = json!([]);
            }
            reject(upgraded(), &dir.0, action, "MCP_STATE_PRODUCER");
        }
    }
}

#[test]
#[ignore = "Exhaustive current-feature matrix; run exhaustive_policy with the previous-writer variable unset"]
fn exhaustive_policy_unowned_lifecycle_guards_every_route() {
    for phase in PHASES {
        for kind in ROUTES {
            let dir = Temp::new();
            let history = unowned_history(&dir.0, phase);
            let mut inventory = read(&state(&dir.0));
            inventory["discovery_runs"] = json!([]);
            write(&state(&dir.0), &inventory);
            let action = route(upgraded(), &dir.0, kind);
            let mut inventory = read(&state(&dir.0));
            inventory["discovery_runs"]
                .as_array_mut()
                .unwrap()
                .push(history.clone());
            write(&state(&dir.0), &inventory);
            if matches!(phase, "activated" | "failed") {
                let path = PathBuf::from(history["state_path"].as_str().unwrap());
                let bytes = fs::read(&path).unwrap();
                call(upgraded(), &dir.0, action);
                assert!(fs::read(path).unwrap() == bytes);
            } else {
                reject(upgraded(), &dir.0, action, "MCP_STATE_PRODUCER");
            }
        }
    }
}

#[test]
fn identical_paused_request_preserves_allowance_across_producers() {
    for binary in [old(), upgraded()] {
        let dir = Temp::new();
        retained_phase(&dir.0, "paused");
        reject(
            binary,
            &dir.0,
            json!({"kind":"prepare-discovery","start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z","max_queries":5,"max_seconds":120}),
            "INVENTORY_INPUT",
        );
    }
}

#[test]
fn brief_selected_producer_refusals_keep_recovery_guidance_and_index() {
    let dir = Temp::new();
    retained_phase(&dir.0, "activated");
    for kind in [
        "dispatch-discovery",
        "resume-discovery",
        "discovery-step",
        "pause-discovery",
    ] {
        let mut action = json!({"kind":kind,"run_index":1});
        if kind == "dispatch-discovery" {
            action["probe_id"] = json!("p-0000000000000000");
        }
        if kind == "discovery-step" {
            action["answers"] = json!([]);
        }
        for flag in ["--summary", "--brief"] {
            let before = snapshot(&dir.0);
            let output = command_with_flag(upgraded(), &dir.0, action.clone(), Some(flag));
            unchanged(&before);
            assert_eq!(output.status.code(), Some(2), "{kind} {flag}: {output:?}");
            assert!(output.stdout.is_empty());
            let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(diagnostic["code"], "MCP_STATE_PRODUCER");
            assert_eq!(diagnostic["pointer"], "/discovery_runs/1");
            let message = diagnostic["message"].as_str().unwrap();
            assert!(message.contains("unowned"));
            assert!(message.contains("standalone"));
            assert!(message.contains("checkpoint"));
            assert!(message.contains("bound to inventory"));
            assert!(message.contains("outstanding inventory searches"));
            assert!(message.contains("concurrent inventory execution"));
            if flag == "--brief" {
                assert!(output.stderr.len() <= 4096);
                assert!(!String::from_utf8_lossy(&output.stderr).contains(dir.0.to_str().unwrap()));
                assert!(
                    diagnostic["message"]
                        .as_str()
                        .unwrap()
                        .contains("producing version")
                );
                assert!(!message.contains("--state"));
            } else {
                assert!(message.contains("env discover --mcp --state CHECKPOINT --resume"));
            }
        }
    }
}

#[test]
fn unsupported_producers_and_format_pairs_remain_rejected_without_writes() {
    let reader = reader_version_for(env!("CARGO_PKG_VERSION"), PREVIOUS_PRODUCER).unwrap();
    let future = reader_version_for(&reader, &reader).unwrap();
    for (producer, schema, expected) in [
        ("unknown".to_string(), 3, "MCP_STATE_TAMPERED"),
        (future, 3, "MCP_STATE_TAMPERED"),
        (
            format!("{PREVIOUS_PRODUCER}-invalid"),
            3,
            "MCP_STATE_TAMPERED",
        ),
        ("0.1.5".to_string(), 3, "MCP_STATE_TAMPERED"),
        (PREVIOUS_PRODUCER.to_string(), 4, "MCP_STATE_RESTART"),
    ] {
        let dir = Temp::new();
        retained_phase(&dir.0, "paused");
        let path = checkpoint(&dir.0, 1);
        let mut checkpoint = read(&path);
        checkpoint["producer_version"] = json!(producer);
        checkpoint["schema_version"] = json!(schema);
        write(&path, &checkpoint);
        reject(upgraded(), &dir.0, scout(), expected);
    }
}

#[test]
fn foreign_owner_must_still_match_locked_inventory() {
    let dir = Temp::new();
    retained_phase(&dir.0, "paused");
    let path = checkpoint(&dir.0, 1);
    let mut checkpoint = read(&path);
    let owner = dir.0.join("different-inventory.json");
    checkpoint["session"]["inventory_owner"] = json!(owner);
    let mut binding = serde_json::to_vec(&(
        &checkpoint["request_sha256"],
        &checkpoint["created_at"],
        &checkpoint["pack"],
        &checkpoint["receipt"],
        &checkpoint["session"]["transport"],
        &checkpoint["session"]["predecessor_sha256"],
    ))
    .unwrap();
    if checkpoint["session"]["sparse_fieldset_fallback"] == true {
        binding.extend_from_slice(b"\0sparse-fieldset-fallback-v1");
    }
    binding.extend_from_slice(b"\0inventory-owner-v1\0");
    binding
        .extend_from_slice(&serde_json::to_vec(&checkpoint["session"]["inventory_owner"]).unwrap());
    checkpoint["session"]["binding_sha256"] = json!(cqlmatic::sha256_hex(&binding));
    write(&path, &checkpoint);
    reject(upgraded(), &dir.0, scout(), "DISCOVERY_INVENTORY_OWNER");
}

fn standalone(path: &Path, args: &[String]) -> Value {
    let output = Command::new(old())
        .args(["env", "discover", "--mcp", "--state"])
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        matches!(output.status.code(), Some(0 | 2 | 3)) && !output.stdout.is_empty(),
        "{output:?}"
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn unowned_history(dir: &Path, phase: &str) -> Value {
    let mut history = retained_phase(dir, "paused");
    let request = read(&checkpoint(dir, 1))["request"].clone();
    let directory = dir.join("standalone");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.join("state.json");
    let pack = directory.join("pack.json");
    let receipt = directory.join("receipt.json");
    let request_path = directory.join("request.json");
    write(&request_path, &request);
    let prepared = standalone(
        &path,
        &[
            "--request".into(),
            request_path.to_str().unwrap().into(),
            "--pack".into(),
            pack.to_str().unwrap().into(),
            "--receipt".into(),
            receipt.to_str().unwrap().into(),
        ],
    );
    match phase {
        "needs-probes" => {}
        "paused" => {
            standalone(&path, &["--cancel".into()]);
        }
        "cancel-requested" => {
            let probe = prepared["probes"][0]["probe_id"].as_str().unwrap();
            standalone(&path, &["--dispatch".into(), probe.into()]);
            standalone(&path, &["--cancel".into()]);
        }
        "cleanup-pending" | "activation-blocked" | "activating" | "activated" | "failed" => {
            let probe = prepared["probes"][0]["probe_id"].as_str().unwrap();
            standalone(&path, &["--dispatch".into(), probe.into()]);
            let answer = directory.join("answer.json");
            write(
                &answer,
                &if phase == "cleanup-pending" {
                    json!({"result":[],"error":"Synthetic timeout"})
                } else {
                    json!({"result":[{"events":10}]})
                },
            );
            if phase == "failed" {
                write(&receipt, &json!({"synthetic":"occupied"}));
                fs::set_permissions(&receipt, fs::Permissions::from_mode(0o600)).unwrap();
            }
            let guard = (phase == "activation-blocked")
                .then(|| cqlmatic::storage::Writer::acquire(&pack).unwrap());
            let output = standalone(
                &path,
                &["--answer".into(), format!("{probe}={}", answer.display())],
            );
            assert_eq!(
                output["status"],
                if matches!(phase, "activating" | "activated") {
                    "complete"
                } else {
                    phase
                }
            );
            drop(guard);
            if phase == "activating" {
                let mut checkpoint = read(&path);
                checkpoint["status"] = json!(phase);
                checkpoint["pack_sha256"] = Value::Null;
                fs::remove_file(&receipt).unwrap();
                write(&path, &checkpoint);
            }
        }
        _ => unreachable!(),
    }
    let checkpoint = read(&path);
    assert_eq!(checkpoint["status"], phase);
    assert_eq!(checkpoint["producer_version"], PREVIOUS_PRODUCER);
    assert!(checkpoint["session"].get("inventory_owner").is_none());
    history["state_path"] = json!(path);
    history["pack_path"] = json!(pack);
    history["receipt_path"] = json!(receipt);
    history["status"] = json!(if phase == "activated" {
        "complete"
    } else {
        phase
    });
    history["answered"] = json!(checkpoint["answers"].as_object().unwrap().len());
    history
}

#[test]
fn selected_current_producer_preserves_dispatch_and_resume_transitions() {
    for phase in PHASES {
        for kind in ["dispatch-discovery", "resume-discovery"] {
            let dir = Temp::new();
            retained_phase(&dir.0, phase);
            if kind == "dispatch-discovery" {
                let raw = read(&checkpoint(&dir.0, 1));
                let request: cqlmatic::discovery::DiscoverRequest =
                    serde_json::from_value(raw["request"].clone()).unwrap();
                let probe = cqlmatic::discovery::frontier(&request, &Default::default())
                    .unwrap()
                    .remove(0);
                let action = json!({"kind":kind,"run_index":1,"probe_id":cqlmatic::discovery::probe_id(&request.environment.view, &probe)});
                if phase == "needs-probes" {
                    call(old(), &dir.0, action);
                    let raw = read(&checkpoint(&dir.0, 1));
                    assert_eq!(raw["session"]["attempts"].as_array().unwrap().len(), 1);
                    assert_eq!(raw["session"]["attempts"][0]["outcome"], "dispatched");
                } else {
                    let before = snapshot(&dir.0);
                    let output = command(old(), &dir.0, action);
                    unchanged(&before);
                    assert_eq!(output.status.code(), Some(2), "{phase}: {output:?}");
                }
            } else if matches!(phase, "cancel-requested" | "cleanup-pending") {
                reject(
                    old(),
                    &dir.0,
                    json!({"kind":kind,"run_index":1}),
                    "DISCOVERY_CLEANUP_PENDING",
                );
            } else {
                let result = call(old(), &dir.0, json!({"kind":kind,"run_index":1}));
                let expected = match phase {
                    "paused" => "needs-probes",
                    "activating" | "activated" => "complete",
                    _ => phase,
                };
                assert_eq!(result["action"]["relay"]["status"], expected, "{phase}");
            }
        }
    }
}

#[test]
fn standalone_controls_keep_producer_pin_and_inventory_ownership() {
    let dir = Temp::new();
    let history = unowned_history(&dir.0, "paused");
    let path = PathBuf::from(history["state_path"].as_str().unwrap());
    let mut inventory = read(&state(&dir.0));
    inventory["discovery_runs"][1] = history;
    write(&state(&dir.0), &inventory);
    reject(upgraded(), &dir.0, scout(), "MCP_STATE_PRODUCER");
    let before = snapshot(&dir.0);
    let output = Command::new(upgraded())
        .args(["env", "discover", "--mcp", "--state"])
        .arg(&path)
        .arg("--resume")
        .output()
        .unwrap();
    unchanged(&before);
    assert_eq!(output.status.code(), Some(2));
    let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["code"], "MCP_STATE_PRODUCER");
    let resumed = standalone(&path, &["--resume".into()]);
    assert_eq!(resumed["status"], "needs-probes");
    standalone(
        &path,
        &[
            "--dispatch".into(),
            resumed["probes"][0]["probe_id"].as_str().unwrap().into(),
        ],
    );
    assert_eq!(
        read(&path)["session"]["attempts"][0]["outcome"],
        "dispatched"
    );

    let owned = Temp::new();
    retained_phase(&owned.0, "paused");
    let before = snapshot(&owned.0);
    let output = Command::new(old())
        .args(["env", "discover", "--mcp", "--state"])
        .arg(checkpoint(&owned.0, 1))
        .arg("--resume")
        .output()
        .unwrap();
    unchanged(&before);
    assert_eq!(output.status.code(), Some(2));
    let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["code"], "DISCOVERY_INVENTORY_OWNER");
}
