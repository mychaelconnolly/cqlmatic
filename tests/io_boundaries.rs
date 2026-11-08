use cqlmatic::storage;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "cqlmatic-io-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn request(dir: &Temp) -> PathBuf {
    let p = dir.0.join("request.json");
    fs::write(&p,json!({"schema_version":1,"environment":{"cloud":"us1","tenant":"00000000000000000000000000000000","view":"search-all"},"start":"2026-01-01T00:00:00Z","end":"2026-01-01T00:01:00Z","census":true}).to_string()).unwrap();
    p
}
#[test]
fn receipt_reservation_is_exclusive_and_can_be_finished() {
    let dir = Temp::new();
    let path = dir.0.join("receipts").join("run.json");
    let file = storage::reserve_receipt(&path).unwrap();
    assert!(fs::read(&path).unwrap().is_empty());
    let duplicate = storage::reserve_receipt(&path).unwrap_err();
    assert_eq!(duplicate.code, "LOCAL_IO");
    assert!(fs::read(&path).unwrap().is_empty());
    let receipt = json!({"status": "complete", "rows": 2});
    storage::finish_receipt(file, &receipt).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap(),
        receipt
    );
}

#[test]
fn protected_state_is_create_new_then_atomically_replaced_under_one_lock() {
    let dir = Temp::new();
    let state = dir.0.join("state.json");
    let writer = storage::Writer::acquire(&state).unwrap();
    writer.create_new_bytes(b"{}", 1024).unwrap();
    assert_eq!(storage::read_bytes(&state, 1024).unwrap(), b"{}\n");
    assert_eq!(
        writer.create_new_bytes(b"{}", 1024).unwrap_err().code,
        "LOCAL_IO"
    );
    writer
        .commit_bytes(br#"{"status":"updated"}"#, 1024)
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&storage::read_bytes(&state, 1024).unwrap()).unwrap(),
        json!({"status":"updated"})
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&state).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn discovery_output_conflicts_fail_before_credentials_or_network() {
    let dir = Temp::new();
    let request = request(&dir);
    let pack = dir.0.join("active.json");
    let blocked = dir.0.join("blocked");
    fs::write(&blocked, "file").unwrap();
    for receipt in [
        pack.clone(),
        dir.0.join("./active.json"),
        dir.0.join(".active.json.lock"),
        blocked.join("receipt.json"),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
            .args(["env", "discover", "--execute", "--request"])
            .arg(&request)
            .arg("--pack")
            .arg(&pack)
            .arg("--receipt")
            .arg(&receipt)
            .arg("--credentials-file")
            .arg(dir.0.join("missing-credentials"))
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        let error: Value = serde_json::from_slice(&out.stderr).unwrap();
        assert!(
            !error["code"].as_str().unwrap().starts_with("AUTH"),
            "{error}"
        );
        assert!(!pack.exists());
    }
}
#[test]
fn human_diagnostics_cannot_execute_terminal_controls() {
    let payload = json!({"schema_version":1,"input":{"mode":"plan","plan":{"source":{},"steps":[{"op":"call","function":"bad\u{1b}[2J\u{7}","args":[]}]}}});
    let mut child = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args(["generate", "--request", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(!out.stderr.contains(&0x1b));
    assert!(!out.stderr.contains(&7));
    assert!(String::from_utf8(out.stderr).unwrap().contains("\\u{1b}"));
}
#[test]
fn closed_output_streams_return_errors_without_panicking() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args(["generate", "--request", "-", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"schema_version":1,"input":{"mode":"plan","plan":{"source":{}}}}"#)
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("panicked"));
}
