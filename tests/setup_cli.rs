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
            .env("COPILOT_HOME", self.0.join("synthetic-copilot"))
            .env("CLAUDE_CONFIG_DIR", self.0.join("synthetic-claude"))
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
#[test]
fn host_managed_selections_resume_without_native_configuration() {
    let temp = Temp::new();
    let selection = json!({"client":"external","host_id":"synthetic-host","cloud":"us1","server_name":"cs-mcp","registration":{"mode":"host-managed"}});
    let selected = temp.run(Some(
        json!({"schema_version":1,"action":{"type":"select","selection":selection}}),
    ));
    assert_eq!(
        selected.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&selected.stderr)
    );
    let selected: Value = serde_json::from_slice(&selected.stdout).unwrap();
    assert_eq!(selected["state"]["phase"], "selected");
    assert!(
        !selected["next_actions"]
            .to_string()
            .contains("Preview the exact native-client")
    );
    let detected = temp.run(Some(json!({"schema_version":1,"action":{"type":"detect"}})));
    assert_eq!(detected.status.code(), Some(1));
    let detected: Value = serde_json::from_slice(&detected.stdout).unwrap();
    assert_eq!(detected["state"]["phase"], "selected");
    for action in [
        json!({"type":"preview","operation":"configure"}),
        json!({"type":"apply","confirmation_sha256":"a".repeat(64)}),
        json!({"type":"confirm-authentication"}),
    ] {
        let output = temp.run(Some(json!({"schema_version":1,"action":action})));
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("SETUP_HOST_MANAGED"));
    }
    let resumed = temp.run(None);
    let resumed: Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(resumed["state"]["phase"], "selected");
    assert!(
        !resumed["next_actions"]
            .to_string()
            .contains("Preview the exact native-client")
    );
    assert!(!temp.0.join("synthetic-codex/config.toml").exists());
    assert!(!temp.0.join("synthetic-appdata/Code/User/mcp.json").exists());
}
#[test]
fn copilot_cli_native_setup_resumes_and_requires_matching_discovery() {
    let temp = Temp::new();
    let selected = temp.run(Some(json!({"schema_version":1,"action":{"type":"select","selection":{"client":"copilot-cli","cloud":"us1","server_name":"cs-mcp","registration":{"mode":"manual","client_id":"synthetic-copilot-id","callback_url":"http://127.0.0.1:1234/callback"}}}})));
    assert_eq!(selected.status.code(), Some(1));
    let selected: Value = serde_json::from_slice(&selected.stdout).unwrap();
    assert_eq!(selected["state"]["phase"], "selected");
    let preview = temp.run(Some(
        json!({"schema_version":1,"action":{"type":"preview","operation":"configure"}}),
    ));
    assert_eq!(
        preview.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let preview: Value = serde_json::from_slice(&preview.stdout).unwrap();
    let hash = preview["state"]["plan"]["confirmation_sha256"]
        .as_str()
        .unwrap();
    assert!(
        preview["state"]["plan"]["commands"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let applied = temp.run(Some(
        json!({"schema_version":1,"action":{"type":"apply","confirmation_sha256":hash}}),
    ));
    assert_eq!(
        applied.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let applied: Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(applied["state"]["phase"], "authentication-required");
    let config: Value = serde_json::from_slice(
        &fs::read(temp.0.join("synthetic-copilot/mcp-config.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        config["mcpServers"]["cs-mcp"]["oauthClientId"],
        "synthetic-copilot-id"
    );
    let resumed = temp.run(None);
    let resumed: Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(resumed["state"]["phase"], "authentication-required");
    let report_path = temp.0.join("discovery.json");
    let mut report = json!({"schema_version":1,"client":"copilot-cli","server_name":"cs-mcp","endpoint":"https://mcp.crowdstrike.com/v1","authenticated":true,"effective_oauth_client_id":"wrong-id","effective_config_source":"user","tools":[{"name":"falcon_search_ngsiem","available":false}]});
    let import = || {
        temp.run(Some(
            json!({"schema_version":1,"action":{"type":"import-discovery","file":report_path}}),
        ))
    };
    fs::write(&report_path, serde_json::to_vec(&report).unwrap()).unwrap();
    let rejected = import();
    assert_eq!(rejected.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("SETUP_DISCOVERY_IDENTITY"));
    report["effective_oauth_client_id"] = json!("synthetic-copilot-id");
    fs::write(&report_path, serde_json::to_vec(&report).unwrap()).unwrap();
    let missing = import();
    assert_eq!(missing.status.code(), Some(1));
    let missing: Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(missing["state"]["phase"], "authenticated");
    report["tools"][0]["available"] = json!(true);
    fs::write(&report_path, serde_json::to_vec(&report).unwrap()).unwrap();
    let verified = import();
    assert_eq!(verified.status.code(), Some(0));
    let verified: Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(verified["state"]["phase"], "tool-discovery-verified");
}
#[test]
fn host_managed_import_requires_bound_report_and_exact_available_tool() {
    let temp = Temp::new();
    let selected = temp.run(Some(json!({"schema_version":1,"action":{"type":"select","selection":{"client":"external","host_id":"synthetic-host","cloud":"us1","server_name":"cs-mcp","registration":{"mode":"host-managed"}}}})));
    assert_eq!(selected.status.code(), Some(1));
    let report_path = temp.0.join("host-report.json");
    let mut report = json!({"schema_version":1,"client":"external","host_id":"wrong-host","server_name":"cs-mcp","endpoint":"https://mcp.crowdstrike.com/v1","authenticated":true,"tools":[{"name":"falcon_search_ngsiem","available":false}]});
    let import = || {
        temp.run(Some(
            json!({"schema_version":1,"action":{"type":"import-discovery","file":report_path}}),
        ))
    };
    fs::write(&report_path, serde_json::to_vec(&report).unwrap()).unwrap();
    let rejected = import();
    assert_eq!(rejected.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("SETUP_DISCOVERY_BINDING"));
    report["host_id"] = json!("synthetic-host");
    fs::write(&report_path, serde_json::to_vec(&report).unwrap()).unwrap();
    let incomplete = import();
    assert_eq!(incomplete.status.code(), Some(1));
    let incomplete: Value = serde_json::from_slice(&incomplete.stdout).unwrap();
    assert_eq!(incomplete["state"]["phase"], "authenticated");
    assert_eq!(incomplete["state"]["configured"], true);
    assert!(incomplete["state"]["configured_sha256"].is_null());
    report["tools"][0]["available"] = json!(true);
    fs::write(&report_path, serde_json::to_vec(&report).unwrap()).unwrap();
    let verified = import();
    assert_eq!(verified.status.code(), Some(0));
    let verified: Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(verified["state"]["phase"], "tool-discovery-verified");
    assert!(verified["handoff"].as_str().unwrap().contains("search-all"));
}
