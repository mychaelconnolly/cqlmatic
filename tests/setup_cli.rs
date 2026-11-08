use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-setup-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn state(&self) -> PathBuf {
        self.0.join("private/state.json")
    }
    fn run(&self, request: Option<Value>) -> Output {
        self.run_mode(request, true)
    }
    fn run_mode(&self, request: Option<Value>, json: bool) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cqlmatic"));
        command
            .args(["setup", "--state"])
            .arg(self.state())
            .stdin(Stdio::null());
        if json {
            command.arg("--json");
        }
        // Isolate native config and executable discovery. No real client config is read.
        command
            .env("PATH", &self.0)
            .env("CODEX_HOME", self.0.join("synthetic-codex"))
            .env("APPDATA", self.0.join("synthetic-appdata"))
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("TERM", "xterm-256color")
            .env_remove("LOCALAPPDATA");
        if let Some(request) = request {
            let path = self.0.join("request.json");
            fs::write(&path, serde_json::to_vec(&request).unwrap()).unwrap();
            command.arg("--request").arg(path);
        }
        command.output().unwrap()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn noninteractive_fresh_status_never_prompts_or_creates_state() {
    let temp = Temp::new();
    let output = temp.run(None);
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["state"]["phase"], "new");
    assert_eq!(result["state"]["configured"], false);
    assert!(result["state"]["selection"].is_null());
    assert!(!temp.state().exists());
    assert!(!temp.0.join("private").exists());
}
#[test]
fn redirected_and_request_status_remain_banner_free() {
    let temp = Temp::new();
    for request in [
        None,
        Some(json!({"schema_version":1,"action":{"type":"status"}})),
    ] {
        let output = temp.run_mode(request, false);
        assert_eq!(output.status.code(), Some(1));
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.starts_with("CQLmatic setup: New\n"), "{text}");
        assert!(!text.contains("/ ____/"));
        assert!(!text.contains("CQLmatic v"));
        assert!(!text.contains('\x1b'));
        assert!(!text.contains("Choose:"));
    }
}
#[test]
fn setup_help_exposes_structured_and_resume_paths() {
    let output = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args(["setup", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    for flag in ["--request", "--state", "--json"] {
        assert!(help.contains(flag));
    }
    assert!(!help.contains("/ ____/"));
    assert!(!help.contains("CQLmatic v"));
    assert!(!help.contains('\x1b'));
}
#[test]
fn version_output_remains_one_plain_line() {
    let output = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("cqlmatic {}\n", env!("CARGO_PKG_VERSION"))
    );
}
#[test]
fn secret_or_unknown_fields_rejected_before_state_creation() {
    let temp = Temp::new();
    for request in [
        json!({"schema_version":1,"action":{"type":"detect","executable":"arbitrary.exe"}}),
        json!({"schema_version":1,"action":{"type":"status"},"client_secret":"synthetic-never-print"}),
        json!({"schema_version":99,"action":{"type":"status"}}),
    ] {
        let output = temp.run(Some(request));
        assert_eq!(output.status.code(), Some(2));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-never-print"));
        assert!(!temp.state().exists());
    }
}
#[test]
fn selection_resumes_without_installing_or_reading_credential_reference() {
    let temp = Temp::new();
    let missing_credentials = temp.0.join("never-created-secret-file.json");
    let output = temp.run(Some(json!({"schema_version":1,"id":"synthetic-selection","action":{"type":"select","selection":{"client":"vs-code","cloud":"eu1","server_name":"synthetic-mcp","registration":{"mode":"api-client","credentials_file":missing_credentials}}}})));
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["id"], "synthetic-selection");
    assert_eq!(result["state"]["phase"], "advanced-pending");
    assert!(!missing_credentials.exists());
    let resumed = temp.run(None);
    assert_eq!(
        resumed.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let resumed: Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(resumed["state"]["selection"]["cloud"], "eu1");
    assert_eq!(resumed["state"]["phase"], "advanced-pending");
    assert!(
        resumed["handoff"]
            .as_str()
            .unwrap()
            .contains("cqlmatic env inventory")
    );
}
