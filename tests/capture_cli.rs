use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

const SERVER: &str = "synthetic-server";

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-capture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(path)
    }
    fn state(&self) -> PathBuf {
        self.0.join("workflow/state.json")
    }
    fn saved(&self) -> Value {
        serde_json::from_slice(&fs::read(self.state()).unwrap()).unwrap()
    }
    fn tally(&self) -> Value {
        serde_json::from_slice(
            &fs::read(self.0.join("workflow/state.json.captures/tally.json")).unwrap(),
        )
        .unwrap()
    }
    /// Confirms a selected-source target in view `third-party` with source `#type=synthetic`.
    fn confirm(&self) {
        self.confirm_at(&self.state());
    }
    fn confirm_at(&self, state: &Path) {
        self.act_on(
            state,
            json!({"kind":"confirm-target","environment":environment(),"scope":"selected-sources","context":"Synthetic capture target","expected_sources":[{"type":"synthetic"}]}),
        );
    }
    fn act_on(&self, state: &Path, action: Value) {
        let request = self.0.join("request.json");
        fs::write(
            &request,
            json!({"schema_version":1,"action":action}).to_string(),
        )
        .unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
            .args(["env", "inventory", "--json", "--state"])
            .arg(state)
            .arg("--request")
            .arg(request)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    /// Runs the hook command on one payload; it must stay silent and succeed whatever happens.
    fn capture(&self, payload: Value) {
        self.capture_as("claude-code", SERVER, payload);
    }
    fn capture_as(&self, client: &str, server: &str, payload: Value) {
        self.capture_into(&self.state(), client, server, payload);
    }
    fn capture_into(&self, state: &Path, client: &str, server: &str, payload: Value) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
            .args(["env", "capture", "--client", client, "--server"])
            .arg(server)
            .arg("--state")
            .arg(state)
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
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{payload}");
        assert!(
            output.stdout.is_empty() && output.stderr.is_empty(),
            "{output:?}"
        );
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn environment() -> Value {
    json!({"cloud":"us1","tenant":"00000000000000000000000000000000","view":"third-party"})
}

fn hours_ago(hours: i64) -> String {
    (time::OffsetDateTime::now_utc() - time::Duration::hours(hours))
        .replace_nanosecond(0)
        .unwrap()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

/// A search through the hosted server's gateway tool, as Claude Code reports it after the call.
fn search(query: &str, response: Value) -> Value {
    json!({
        "hook_event_name": "PostToolUse",
        "tool_name": format!("mcp__{SERVER}__execute_tool"),
        "tool_input": {"name": "falcon_search_ngsiem", "arguments": {
            "repository": "third-party", "query_string": query,
            "start": hours_ago(2), "end": hours_ago(1)
        }},
        "tool_response": response,
    })
}

fn body(rows: Value) -> String {
    json!({"result": rows, "trace_id": "00000000-0000-0000-0000-000000000000"}).to_string()
}

const WRITTEN_CLIENTS: [(&str, &str, &str, &str); 5] = [
    (
        "claude-code",
        "CLAUDE_CONFIG_DIR",
        ".claude",
        "settings.json",
    ),
    ("codex", "CODEX_HOME", ".codex", "hooks.json"),
    (
        "copilot-cli",
        "COPILOT_HOME",
        ".copilot",
        "hooks/cqlmatic-capture.json",
    ),
    (
        "copilot-app",
        "COPILOT_HOME",
        ".copilot",
        "hooks/cqlmatic-capture.json",
    ),
    (
        "copilot-vscode",
        "COPILOT_HOME",
        ".copilot",
        "hooks/cqlmatic-capture.json",
    ),
];

fn isolated<'a>(command: &'a mut Command, home: &Path) -> &'a mut Command {
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("COPILOT_HOME")
}

fn install(home: &Path, client: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cqlmatic"));
    isolated(&mut command, home).args([
        "env",
        "capture",
        "--client",
        client,
        "--server",
        SERVER,
        "--install",
    ]);
    command
}

fn report(command: &mut Command) -> Value {
    let output = command.output().unwrap();
    assert!(
        output.status.success() && output.stderr.is_empty(),
        "{output:?}"
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn capture_install_previews_writes_and_keeps_the_documented_hooks() {
    for (client, variable, default, file) in WRITTEN_CLIENTS {
        let home = Temp::new();
        let root = home.0.join("relocated client");
        let file = file
            .split('/')
            .fold(root.clone(), |path, part| path.join(part));
        let state = home.0.join("inventory's state.json");
        let mut command = install(&home.0, client);
        command
            .env(variable, root)
            .args(["--program", "launcher --label capture", "--state"])
            .arg(&state);
        let preview = report(command.arg("--dry-run"));
        assert_eq!(preview["path"], json!(file), "{client}");
        assert_eq!(preview["changed"], true);
        assert_eq!(preview["installed"], false);
        assert_eq!(preview["dry_run"], true);
        assert_eq!(fs::read_dir(&home.0).unwrap().count(), 0);
        let mut command = install(&home.0, client);
        command
            .env(variable, home.0.join("relocated client"))
            .args(["--program", "launcher --label capture", "--state"])
            .arg(&state);
        let fresh = report(&mut command);
        assert_eq!(fresh["changed"], true);
        assert_eq!(fresh["installed"], true);
        assert_eq!(fresh["dry_run"], false);
        for field in ["path", "event", "command", "entry", "next_actions"] {
            assert_eq!(fresh[field], preview[field], "{client}, {field}");
        }
        let check_command = |value: &Value| {
            let value = value.as_str().unwrap();
            assert!(value.starts_with(&format!("launcher --label capture env capture --client {client} --server '{SERVER}' --state ")));
            assert!(value.contains("inventory"), "{value}");
            assert!(value.contains("state.json"), "{value}");
        };
        let expected = if client.starts_with("copilot-") {
            assert_eq!(fresh["event"], "postToolUse");
            check_command(&fresh["command"]["bash"]);
            check_command(&fresh["command"]["powershell"]);
            json!({"version": 1, "hooks": {"postToolUse": [{
                "type": "command", "bash": fresh["command"]["bash"],
                "powershell": fresh["command"]["powershell"],
            }]}})
        } else {
            assert_eq!(fresh["event"], "PostToolUse");
            check_command(&fresh["command"]);
            json!({"hooks": {"PostToolUse": [{
                "matcher": "mcp__synthetic-server__.*",
                "hooks": [{"type": "command", "command": fresh["command"], "async": true}],
            }]}})
        };
        if client == "codex" {
            assert!(
                fresh["next_actions"][0]
                    .as_str()
                    .unwrap()
                    .contains("/hooks")
            );
        }
        let original = fs::read(&file).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&original).unwrap(),
            expected
        );
        let repeat = report(&mut command);
        assert_eq!(repeat["installed"], true);
        assert_eq!(repeat["changed"], false);
        assert_eq!(fs::read(&file).unwrap(), original);
        assert!(!home.0.join(default).exists());
        assert_eq!(fs::read_dir(file.parent().unwrap()).unwrap().count(), 1);
    }
}

#[test]
fn capture_install_preserves_existing_settings_and_deduplicates_by_command() {
    for (client, _, default, file) in WRITTEN_CLIENTS {
        let home = Temp::new();
        let file = home.0.join(default).join(file);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let copilot = client.starts_with("copilot-");
        let event = if copilot {
            "postToolUse"
        } else {
            "PostToolUse"
        };
        let version = if copilot { r#", "version": 1"# } else { "" };
        let old_hook = if copilot {
            r#"{"type":"command","bash":"another command"}"#
        } else {
            r#"{"matcher":"another","hooks":[{"type":"command","command":"another command"}]}"#
        };
        let original = format!(
            "{{\r\n  \"z-before\": {{\"z\": 1e+01, \"a\": \"\\u0061\"}},\r\n  \"hooks\": {{\"unrelated\": [], \"{event}\": [{old_hook}]}},\r\n  \"a-after\": true{version}\r\n}}\r\n"
        );
        fs::write(&file, &original).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
        }
        let mut command = install(&home.0, client);
        let fresh = report(&mut command);
        let after = fs::read_to_string(&file).unwrap();
        let inserted_at = original.find(old_hook).unwrap() + old_hook.len();
        assert!(after.starts_with(&original[..inserted_at]), "{after}");
        assert!(after.ends_with(&original[inserted_at..]), "{after}");
        let mut expected: Value = serde_json::from_str(&original).unwrap();
        expected["hooks"][event]
            .as_array_mut()
            .unwrap()
            .push(fresh["entry"]["hooks"][event][0].clone());
        assert_eq!(serde_json::from_str::<Value>(&after).unwrap(), expected);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o640
            );
        }

        let hook = &mut expected["hooks"][event][1];
        hook["extra-setting"] = json!(true);
        if copilot {
            hook["powershell"] = json!("another command");
        } else {
            hook["matcher"] = json!("alternate matcher");
        }
        let wanted = serde_json::to_vec(&expected).unwrap();
        fs::write(&file, &wanted).unwrap();
        let repeat = report(&mut command);
        assert_eq!(repeat["changed"], false, "{client}");
        assert_eq!(fs::read(&file).unwrap(), wanted, "{client}");
    }
}

#[test]
fn capture_install_adds_missing_containers() {
    let home = Temp::new();
    let file = home.0.join(".copilot/hooks/cqlmatic-capture.json");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    for original in [
        r#"{"unrelated":true}"#,
        r#"{"hooks":{}}"#,
        r#"{"hooks":{"postToolUse":[]}}"#,
    ] {
        fs::write(&file, original).unwrap();
        let fresh = report(&mut install(&home.0, "copilot-cli"));
        assert_eq!(fresh["changed"], true);
        let mut expected: Value = serde_json::from_str(original).unwrap();
        expected["version"] = json!(1);
        expected["hooks"] = fresh["entry"]["hooks"].clone();
        let after = fs::read(&file).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&after).unwrap(), expected);
        assert_eq!(
            report(&mut install(&home.0, "copilot-cli"))["changed"],
            false
        );
        assert_eq!(fs::read(&file).unwrap(), after);
    }
}

#[test]
fn capture_install_refuses_unsafe_or_invalid_settings() {
    let home = Temp::new();
    let root = home.0.join(".codex");
    let file = root.join("hooks.json");
    fs::create_dir(&root).unwrap();
    let command = || install(&home.0, "codex");
    let assert_refused = |code| {
        let output = command().output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stdout.is_empty());
        let d: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(d["code"], code);
        assert!(
            d["message"]
                .as_str()
                .unwrap()
                .contains(file.to_str().unwrap()),
            "{d}"
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    };
    for bytes in [
        &b"{"[..],
        b"{} {}",
        b"[]",
        b"null",
        br#"{"duplicate":1,"duplicate":2}"#,
        br#"{"hooks":null}"#,
        br#"{"hooks":[]}"#,
        br#"{"hooks":{"PostToolUse":{}}}"#,
        br#"{"hooks":{"PostToolUse":null}}"#,
    ] {
        fs::write(&file, bytes).unwrap();
        assert_refused("CLI_INPUT");
        assert_eq!(fs::read(&file).unwrap(), bytes);
    }
    let oversized = vec![b' '; 8 * 1024 * 1024 + 1];
    fs::write(&file, &oversized).unwrap();
    assert_refused("LOCAL_SIZE");
    assert_eq!(fs::read(&file).unwrap(), oversized);
    fs::remove_file(&file).unwrap();
    fs::create_dir(&file).unwrap();
    assert_refused("LOCAL_FILE_TYPE");
    assert!(file.is_dir());
}

#[cfg(any(unix, windows))]
#[test]
fn capture_install_reports_a_symbolic_link_for_manual_installation() {
    let home = Temp::new();
    let root = home.0.join(".claude");
    let file = root.join("settings.json");
    let target = home.0.join("settings.json");
    fs::create_dir(&root).unwrap();
    fs::write(&target, b"this file is never read or edited").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &file).unwrap();
    #[cfg(windows)]
    match std::os::windows::fs::symlink_file(&target, &file) {
        Err(error) if error.raw_os_error() == Some(1314) => return, // Symlink privilege unavailable.
        result => result.unwrap(),
    }
    let output = report(&mut install(&home.0, "claude-code"));
    assert_eq!(output["installed"], false);
    assert_eq!(output["changed"], false);
    assert_eq!(output["path"], json!(file));
    assert!(
        output["next_actions"][0]
            .as_str()
            .unwrap()
            .contains("linked")
    );
    assert_eq!(fs::read_link(&file).unwrap(), target);
    assert_eq!(
        fs::read(&target).unwrap(),
        b"this file is never read or edited"
    );
    assert_eq!(fs::read_dir(root).unwrap().count(), 1);
}

#[cfg(any(unix, windows))]
#[test]
fn capture_install_merges_settings_through_a_symlinked_directory() {
    let home = Temp::new();
    let root = home.0.join("settings");
    let linked = home.0.join(".claude");
    let file = root.join("settings.json");
    fs::create_dir(&root).unwrap();
    fs::write(&file, r#"{"existing":true}"#).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&root, &linked).unwrap();
    #[cfg(windows)]
    match std::os::windows::fs::symlink_dir(&root, &linked) {
        Err(error) if error.raw_os_error() == Some(1314) => return, // Symlink privilege unavailable.
        result => result.unwrap(),
    }

    let output = report(&mut install(&home.0, "claude-code"));
    assert_eq!(output["installed"], true);
    assert_eq!(output["changed"], true);
    assert_eq!(output["path"], json!(linked.join("settings.json")));
    let settings: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    assert_eq!(settings["existing"], true);
    assert_eq!(settings["hooks"], output["entry"]["hooks"]);
    assert_eq!(fs::read_link(linked).unwrap(), root);
    assert_eq!(fs::read_dir(root).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn capture_install_creates_settings_in_a_group_writable_directory() {
    use std::os::unix::fs::PermissionsExt;

    let home = Temp::new();
    let root = home.0.join(".codex");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o770)).unwrap();

    let output = report(&mut install(&home.0, "codex"));
    assert_eq!(output["installed"], true);
    assert_eq!(output["changed"], true);
    let settings: Value =
        serde_json::from_slice(&fs::read(root.join("hooks.json")).unwrap()).unwrap();
    assert_eq!(settings, output["entry"]);
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o770
    );
}

#[test]
fn capture_install_prints_documented_manual_entries() {
    let home = Temp::new();
    for (client, path, event) in [
        (
            "cline",
            Some("~/.cline/hooks/PostToolUse"),
            Some("PostToolUse"),
        ),
        (
            "cursor",
            Some("~/.cursor/hooks.json"),
            Some("afterMCPExecution"),
        ),
        (
            "gemini-cli",
            Some("~/.gemini/settings.json"),
            Some("AfterTool"),
        ),
        (
            "kiro",
            Some("~/.kiro/hooks/cqlmatic-capture.json"),
            Some("PostToolUse"),
        ),
        (
            "qwen-code",
            Some("~/.qwen/settings.json"),
            Some("PostToolUse"),
        ),
        ("generic", None, None),
    ] {
        let output = report(install(&home.0, client).args(["--program", "launcher"]));
        assert_eq!(output["installed"], false);
        assert_eq!(output["changed"], false);
        assert_eq!(output["path"], json!(path));
        assert_eq!(output["event"], json!(event));
        let command = format!("launcher env capture --client {client} --server '{SERVER}'");
        assert_eq!(output["command"], command);
        let expected = match client {
            "cline" => json!(format!("#!/bin/sh\n{command}\necho '{{}}'\n")),
            "cursor" => {
                json!({"version": 1, "hooks": {"afterMCPExecution": [{"command": command}]}})
            }
            "gemini-cli" => json!({"hooks": {"AfterTool": [{
                "matcher": "mcp_synthetic-server_.*", "hooks": [{"type":"command", "command": command}],
            }]}}),
            "kiro" => json!({"version": "v1", "hooks": [{
                "name": "cqlmatic-capture", "trigger": "PostToolUse", "matcher": "@synthetic-server",
                "action": {"type": "command", "command": command},
            }]}),
            "qwen-code" => json!({"hooks": {"PostToolUse": [{
                "matcher": "mcp__synthetic-server__.*", "hooks": [{"type":"command", "command": command, "async":true}],
            }]}}),
            _ => Value::Null,
        };
        assert_eq!(output["entry"], expected, "{client}");
        assert!(!output["next_actions"].as_array().unwrap().is_empty());
    }
    assert_eq!(fs::read_dir(&home.0).unwrap().count(), 0);
}

#[test]
fn capture_install_requires_valid_program_text() {
    let home = Temp::new();
    for program in ["", "  ", "launcher\nextra", "launcher\u{7f}"] {
        let output = install(&home.0, "codex")
            .args(["--program", program])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stderr).unwrap()["code"],
            "CLI_INPUT"
        );
    }
    assert_eq!(fs::read_dir(&home.0).unwrap().count(), 0);
}

#[test]
fn capture_without_install_is_silent_with_invalid_input() {
    let home = Temp::new();
    let mut command = Command::new(env!("CARGO_BIN_EXE_cqlmatic"));
    let mut child = isolated(&mut command, &home.0)
        .args([
            "env", "capture", "--client", "codex", "--server", SERVER, "--state",
        ])
        .arg(home.state())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"invalid JSON")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success() && output.stdout.is_empty() && output.stderr.is_empty());
    assert_eq!(fs::read_dir(&home.0).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn capture_install_preserves_existing_settings_permission_bits() {
    use std::os::unix::fs::PermissionsExt;

    let home = Temp::new();
    let path = home.0.join(".codex/hooks.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "{}").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o764)).unwrap();

    assert_eq!(report(&mut install(&home.0, "codex"))["changed"], true);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o764
    );
}

#[cfg(windows)]
fn powershell() -> Command {
    let root = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
    let mut shell = Command::new(root.join("System32/WindowsPowerShell/v1.0/powershell.exe"));
    shell.args(["-NoProfile", "-NonInteractive", "-Command"]);
    shell
}

#[test]
fn installed_hook_runs_the_native_command_and_captures_evidence() {
    let dir = Temp::new();
    let directory = dir.0.join("work folder's $data; settings");
    let state = directory.join("state.json");
    dir.confirm_at(&state);
    let executable = directory.join(format!("capture's $runner{}", std::env::consts::EXE_SUFFIX));
    if fs::hard_link(env!("CARGO_BIN_EXE_cqlmatic"), &executable).is_err() {
        fs::copy(env!("CARGO_BIN_EXE_cqlmatic"), &executable).unwrap();
    }
    let server = "synthetic $server's (name)";
    let mut installer = Command::new(&executable);
    isolated(&mut installer, &dir.0)
        .args([
            "env",
            "capture",
            "--client",
            "copilot-cli",
            "--server",
            server,
            "--install",
            "--state",
        ])
        .arg(&state);
    let installed = report(&mut installer);
    let file = dir.0.join(".copilot/hooks/cqlmatic-capture.json");
    let settings: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
    let hook = &settings["hooks"]["postToolUse"][0];
    let shell = if cfg!(windows) { "powershell" } else { "bash" };
    let command = hook[shell].as_str().unwrap();
    assert_eq!(installed["command"][shell], command);
    let response = body(json!([{"synthetic_field": "seen"}]));
    let sample = search("#type=synthetic | head(5)", json!(response));
    let payload = json!({
        "toolName": format!("{server}-falcon_search_ngsiem"),
        "toolArgs": sample["tool_input"]["arguments"],
        "toolResult": {"resultType": "success", "textResultForLlm": response},
    });
    #[cfg(unix)]
    let mut shell = {
        let mut shell = Command::new("/bin/bash");
        shell.args(["-c", command]);
        shell
    };
    #[cfg(windows)]
    let mut shell = {
        let mut shell = powershell();
        shell
            .arg("& ([ScriptBlock]::Create($env:CQLMATIC_TEST_HOOK))")
            .env("CQLMATIC_TEST_HOOK", command);
        shell
    };
    let mut child = isolated(&mut shell, &dir.0)
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
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success() && output.stdout.is_empty() && output.stderr.is_empty(),
        "{output:?}"
    );
    let state: Value = serde_json::from_slice(&fs::read(state).unwrap()).unwrap();
    assert_eq!(
        state["captured_observations"][0]["fields"][0]["field"],
        "synthetic_field"
    );
}

#[test]
fn capture_credits_a_plain_search_in_every_claude_code_result_form() {
    let dir = Temp::new();
    dir.confirm();
    let query = "#type=synthetic | head(5)";

    dir.capture(search(
        query,
        json!(body(json!([{"user": "ann", "bytes": "10"}]))),
    ));
    let mut listed = search(
        query,
        json!([{"type": "text", "text": body(json!([{"user": "bob", "empty": ""}]))}]),
    );
    // A direct search tool, and a call without `end`, which searched up to the moment it ran.
    listed["tool_name"] = json!(format!("mcp__{SERVER}__falcon_search_ngsiem"));
    listed["tool_input"] = listed["tool_input"]["arguments"].take();
    listed["tool_input"].as_object_mut().unwrap().remove("end");
    dir.capture(listed);

    let results = dir.0.join("session/tool-results");
    fs::create_dir_all(&results).unwrap();
    let saved = results.join("mcp-synthetic-server-execute_tool-1-a.txt");
    fs::write(&saved, body(json!([{"host": "synthetic-host"}]))).unwrap();
    let notice = |path: &Path| {
        let mut payload = search(
            query,
            json!(format!(
                "Error: result (91,246 characters) exceeds maximum allowed tokens. Output has been saved to {}.\nFormat: JSON with schema: {{result: [{{...}}], trace_id: string}}\n",
                path.display()
            )),
        );
        payload["transcript_path"] = json!(dir.0.join("session.jsonl"));
        payload
    };
    dir.capture(notice(&saved));
    // A notice may only name a file among the session's own saved results.
    let outside = dir.0.join("outside.txt");
    fs::write(&outside, body(json!([{"stray": "x"}]))).unwrap();
    dir.capture(notice(&outside));

    let state = dir.saved();
    let observations = state["captured_observations"].as_array().unwrap();
    let fields = |index: usize| {
        observations[index]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|field| field["field"].as_str().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(observations.len(), 3, "{state}");
    // Captures are kept apart from imported observations.
    assert_eq!(state["query_observations"], json!([]));
    assert_eq!(fields(0), ["bytes", "user"]);
    assert_eq!(fields(1), ["user"]);
    assert_eq!(fields(2), ["host"]);
    for observation in observations {
        assert_eq!(observation["caller_assertion"], "client-hook-capture");
        assert_eq!(observation["environment"]["view"], "third-party");
    }
    assert_eq!(dir.tally(), json!({"recorded": 3, "search-failed": 1}));

    // Each credited search keeps a private copy of its query and result.
    let captures = dir.0.join("workflow/state.json.captures");
    let kept = |extension: &str| {
        fs::read_dir(&captures)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|found| found == extension))
            .collect::<Vec<_>>()
    };
    assert_eq!(kept("cql").len(), 3);
    assert_eq!(kept("json").len(), 4, "three results and the tally");
    assert!(
        kept("cql")
            .iter()
            .all(|path| fs::read_to_string(path).unwrap() == query)
    );
    #[cfg(unix)]
    for path in kept("cql").into_iter().chain(kept("json")) {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o077,
            0,
            "{path:?}"
        );
    }
}

#[test]
fn capture_flags_an_unmapped_source_and_counts_what_it_skips() {
    let dir = Temp::new();
    // Without a confirmed inventory there is nothing to add to, and nothing is created.
    dir.capture(search("#type=synthetic", json!(body(json!([{"a": "1"}])))));
    assert!(!dir.0.join("workflow").exists());
    dir.confirm();
    let rows = || json!(body(json!([{"user": "ann"}])));

    let unmapped = "#type=unmapped user=ann | head(5)";
    dir.capture(search(unmapped, rows()));
    dir.capture(search(unmapped, rows()));
    let state = dir.saved();
    let pending = state["pending_inventory"].as_array().unwrap();
    assert_eq!(pending.len(), 1, "{state}");
    assert_eq!(pending[0]["source"], json!({"type": "unmapped"}));
    assert_eq!(pending[0]["new_source"], true);
    assert_eq!(pending[0]["fields"], json!(["user"]));

    // Mapped source, but an aggregate proves nothing about its fields.
    dir.capture(search("#type=synthetic | count()", rows()));
    dir.capture(search("user=ann | head(5)", rows()));
    dir.capture(search("#type=synthetic", json!(body(json!([])))));
    dir.capture(search(
        "#type=synthetic",
        json!(json!({"result": {"error": "Failed to start NGSIEM search"}}).to_string()),
    ));
    let mut failed = search("#type=synthetic", Value::Null);
    failed["hook_event_name"] = json!("PostToolUseFailure");
    failed.as_object_mut().unwrap().remove("tool_response");
    failed["error"] = json!("Search failed");
    dir.capture(failed);
    let mut elsewhere = search("#type=synthetic", rows());
    elsewhere["tool_input"]["arguments"]["repository"] = json!("search-all");
    dir.capture(elsewhere);

    // Other servers and other tools are not searches of this inventory's target.
    let mut other_server = search("#type=synthetic", rows());
    other_server["tool_name"] = json!("mcp__another-server__execute_tool");
    dir.capture(other_server);
    let mut other_tool = search("#type=synthetic", rows());
    other_tool["tool_input"]["name"] = json!("another_tool");
    dir.capture(other_tool);
    dir.capture(json!("not a hook payload"));

    assert_eq!(
        dir.tally(),
        json!({
            "new-source-lead": 1, "nothing-new": 1, "not-creditable": 1, "no-single-source": 1,
            "no-rows": 1, "search-failed": 2, "other-view": 1
        })
    );
    assert_eq!(dir.saved()["query_observations"], json!([]));
    assert!(dir.saved()["captured_observations"].is_null());
    let leftovers = fs::read_dir(dir.0.join("workflow/state.json.captures"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| !name.contains("tally.json"))
        .collect::<Vec<_>>();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn capture_reads_codex_and_generic_payloads() {
    let dir = Temp::new();
    dir.confirm();
    let query = "#type=synthetic | head(5)";
    // Codex hands over the MCP result object and spells the server name with underscores.
    let codex = |response: Value| {
        let mut payload = search(query, response);
        payload["tool_name"] = json!("mcp__synthetic_server__execute_tool");
        payload
    };
    let text = body(json!([{"user": "ann"}]));
    dir.capture_as(
        "codex",
        "synthetic_server",
        codex(json!({
            "content": [{"type": "text", "text": text}],
            "structuredContent": serde_json::from_str::<Value>(&text).unwrap()
        })),
    );
    // A structured copy that disagrees with the text is refused, as it is for an import.
    dir.capture_as(
        "codex",
        "synthetic_server",
        codex(json!({
            "content": [{"type": "text", "text": text}],
            "structuredContent": {"result": {"error": "Search failed"}}
        })),
    );
    dir.capture_as(
        "codex",
        "synthetic_server",
        codex(json!({"content": [{"type": "text", "text": "Search failed"}], "isError": true})),
    );
    // Only Claude Code is known to move a result into a file, so no other client's notice is read.
    let saved = dir.0.join("session/tool-results/saved.txt");
    fs::create_dir_all(saved.parent().unwrap()).unwrap();
    fs::write(&saved, body(json!([{"host": "synthetic-host"}]))).unwrap();
    let mut notice = codex(json!(format!(
        "Error: result exceeds maximum allowed tokens. Output has been saved to {}.",
        saved.display()
    )));
    notice["transcript_path"] = json!(dir.0.join("session.jsonl"));
    dir.capture_as("codex", "synthetic_server", notice);

    // The generic form needs only the tool name, its input and its result.
    dir.capture_as(
        "generic",
        SERVER,
        json!({
            "tool_name": format!("mcp__{SERVER}__falcon_search_ngsiem"),
            "tool_input": {
                "repository": "third-party", "query_string": query, "start": hours_ago(3)
            },
            "tool_response": body(json!([{"bytes": "10"}])),
        }),
    );

    let state = dir.saved();
    let fields = state["captured_observations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|observation| observation["fields"][0]["field"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(fields, ["user", "bytes"], "{state}");
    assert_eq!(dir.tally(), json!({"recorded": 2, "search-failed": 3}));
}

#[test]
fn capture_reads_copilot_cli_and_cursor_payloads() {
    let dir = Temp::new();
    dir.confirm();
    let query = "#type=synthetic | head(5)";
    let arguments = || {
        json!({
            "repository": "third-party", "query_string": query,
            "start": hours_ago(2), "end": hours_ago(1)
        })
    };

    // Copilot CLI joins server and tool with a hyphen and hands the result over as text.
    let copilot = |result: Value| {
        json!({
            "toolName": format!("{SERVER}-falcon_search_ngsiem"),
            "toolArgs": arguments(),
            "toolResult": result,
        })
    };
    let success = |text: String| json!({"resultType": "success", "textResultForLlm": text});
    dir.capture_as(
        "copilot-cli",
        SERVER,
        copilot(success(body(json!([{"user": "ann"}])))),
    );
    // A large result is named in a notice and read only from the temporary directory.
    let saved = std::env::temp_dir().join(format!(
        "{}-copilot-tool-output-synthetic.txt",
        std::process::id()
    ));
    fs::write(&saved, body(json!([{"host": "synthetic-host"}]))).unwrap();
    let notice = |path: &Path| {
        success(format!(
            "Output too large to read at once (92.9 KB). Saved to: {}\nConsider using tools like rg.\n\nPreview (first 500 chars):\n{{\"result\":[",
            path.display()
        ))
    };
    dir.capture_as("copilot-cli", SERVER, copilot(notice(&saved)));
    fs::remove_file(&saved).unwrap();
    let elsewhere = dir.0.join("1-copilot-tool-output-synthetic.txt");
    fs::write(&elsewhere, body(json!([{"stray": "x"}]))).unwrap();
    dir.capture_as("copilot-cli", SERVER, copilot(notice(&elsewhere)));
    let mut failed = copilot(Value::Null);
    failed.as_object_mut().unwrap().remove("toolResult");
    failed["error"] = json!("Search failed");
    dir.capture_as("copilot-cli", SERVER, failed);

    // Cursor names the server separately and hands arguments and result over as JSON text.
    let cursor = |server: &str, result: Value| {
        json!({
            "hook_event_name": "afterMCPExecution",
            "tool_name": "execute_tool",
            "mcp_server_name": server,
            "tool_input": json!({"name": "falcon_search_ngsiem", "arguments": arguments()})
                .to_string(),
            "result_json": result.to_string(),
        })
    };
    dir.capture_as(
        "cursor",
        SERVER,
        cursor(
            SERVER,
            json!({"content": [{"type": "text", "text": body(json!([{"bytes": "10"}]))}]}),
        ),
    );
    dir.capture_as(
        "cursor",
        SERVER,
        cursor(SERVER, json!({"result": [{"zone": "synthetic-zone"}]})),
    );
    dir.capture_as(
        "cursor",
        SERVER,
        cursor("another-server", json!({"result": [{"stray": "x"}]})),
    );

    let state = dir.saved();
    let fields = state["captured_observations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|observation| observation["fields"][0]["field"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(fields, ["user", "host", "bytes", "zone"], "{state}");
    assert_eq!(dir.tally(), json!({"recorded": 4, "search-failed": 2}));
}

#[test]
fn capture_reads_cline_gemini_kiro_and_qwen_payloads() {
    let dir = Temp::new();
    dir.confirm();
    let query = "#type=synthetic | head(5)";
    let arguments = || {
        json!({
            "repository": "third-party", "query_string": query,
            "start": hours_ago(2), "end": hours_ago(1)
        })
    };
    let result =
        |field: &str| json!({"content": [{"type": "text", "text": body(json!([{field: "x"}]))}]});

    // Cline: the command-line client's record, then the editor extension's text-only form.
    dir.capture_as(
        "cline",
        SERVER,
        json!({"hookName": "tool_result", "tool_result": {
            "name": format!("{SERVER}__falcon_search_ngsiem"),
            "input": arguments(), "output": result("a"), "error": null
        }}),
    );
    dir.capture_as(
        "cline",
        SERVER,
        json!({"postToolUse": {
            "toolName": format!("{SERVER}__falcon_search_ngsiem"),
            "parameters": arguments(), "result": result("b").to_string(), "success": true
        }}),
    );
    dir.capture_as(
        "cline",
        SERVER,
        json!({"tool_result": {
            "name": format!("{SERVER}__falcon_search_ngsiem"),
            "input": arguments(), "output": null, "error": "Search failed"
        }}),
    );

    // Gemini CLI names the server in `mcp_context` and wraps the text it gives the model.
    let gemini = |server: &str, response: Value| {
        json!({
            "hook_event_name": "AfterTool",
            "tool_name": "mcp_synthetic-server_falcon_search_ngsiem",
            "tool_input": arguments(),
            "tool_response": response,
            "mcp_context": {"server_name": server, "tool_name": "falcon_search_ngsiem"},
        })
    };
    let display = body(json!([{"c": "x"}]));
    let wrapped = format!("<untrusted_context>\n{display}\n</untrusted_context>");
    dir.capture_as(
        "gemini-cli",
        SERVER,
        gemini(
            SERVER,
            json!({"llmContent": [{"text": wrapped}], "returnDisplay": display}),
        ),
    );
    dir.capture_as(
        "gemini-cli",
        SERVER,
        gemini(
            SERVER,
            json!({
                "llmContent": "MCP tool reported tool error",
                "returnDisplay": "Error: MCP tool reported an error.",
                "error": {"message": "Search failed", "type": "mcp_tool_error"}
            }),
        ),
    );
    dir.capture_as(
        "gemini-cli",
        SERVER,
        gemini(
            "another-server",
            json!({"returnDisplay": body(json!([{"stray": "x"}]))}),
        ),
    );

    // Kiro reports `mcp_SERVER_TOOL`; its predecessor reported `@SERVER/TOOL`.
    for (name, field) in [
        (format!("mcp_{SERVER}_falcon_search_ngsiem"), "d"),
        (format!("@{SERVER}/falcon_search_ngsiem"), "e"),
    ] {
        dir.capture_as(
            "kiro",
            SERVER,
            json!({
                "hook_event_name": "PostToolUse",
                "tool_name": name,
                "tool_input": arguments(),
                "tool_response": {"success": true, "result": [result(field)]},
            }),
        );
    }
    dir.capture_as(
        "kiro",
        SERVER,
        json!({
            "tool_name": format!("mcp_{SERVER}_falcon_search_ngsiem"),
            "tool_input": arguments(),
            "tool_response": {"success": false, "error": ["Search failed"]},
        }),
    );

    dir.capture_as(
        "qwen-code",
        SERVER,
        json!({
            "hook_event_name": "PostToolUse",
            "tool_name": format!("mcp__{SERVER}__falcon_search_ngsiem"),
            "tool_input": arguments(),
            "tool_response": {
                "llmContent": [{"text": body(json!([{"f": "x"}]))}],
                "returnDisplay": body(json!([{"f": "x"}]))
            },
        }),
    );

    // The Copilot surfaces share one hook format.
    dir.capture_as(
        "copilot-vscode",
        SERVER,
        json!({
            "toolName": format!("{SERVER}-falcon_search_ngsiem"),
            "toolArgs": arguments(),
            "toolResult": {"resultType": "success", "textResultForLlm": body(json!([{"g": "x"}]))},
        }),
    );

    let state = dir.saved();
    let fields = state["captured_observations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|observation| observation["fields"][0]["field"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(fields, ["a", "b", "c", "d", "e", "f", "g"], "{state}");
    assert_eq!(dir.tally(), json!({"recorded": 7, "search-failed": 3}));
}

/// Makes a file look old enough for the tidy-up to consider it.
fn age(path: &Path) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
        .unwrap();
}

fn names(directory: &Path) -> Vec<String> {
    let mut names = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| !name.starts_with('.'))
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn capture_replaces_covered_captures_and_tidies_what_nothing_refers_to() {
    let dir = Temp::new();
    dir.confirm();
    let window = |start: i64, end: Option<i64>, rows: Value| {
        let mut payload = search("#type=synthetic | head(5)", json!(body(rows)));
        let arguments = &mut payload["tool_input"]["arguments"];
        arguments["start"] = json!(hours_ago(start));
        match end {
            Some(end) => arguments["end"] = json!(hours_ago(end)),
            None => drop(arguments.as_object_mut().unwrap().remove("end")),
        }
        payload
    };
    let captures = dir.0.join("workflow/state.json.captures");
    let audits = dir.0.join("workflow/query-observations");

    // A thirty-day search is credited, then replaced by a later-starting one showing more.
    dir.capture(window(24 * 30, Some(3), json!([{"a": "1", "b": "1"}])));
    let mut replaced = names(&captures);
    replaced.retain(|name| name != "tally.json");
    assert_eq!(replaced.len(), 2);
    let replaced_audit = names(&audits);
    // An import may reuse a capture's saved query with a result of its own. Its audit record
    // then names that query file, which has to outlive the capture.
    let reused = replaced
        .iter()
        .find(|name| name.ends_with(".cql"))
        .unwrap()
        .clone();
    let result = dir.0.join("workflow/imported-result.json");
    fs::write(&result, body(json!([{"z": "1"}]))).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&result, fs::Permissions::from_mode(0o600)).unwrap();
    }
    dir.act_on(
        &dir.state(),
        json!({
            "kind": "import-query-observations", "query_file": captures.join(&reused),
            "result_file": result, "environment": environment(), "source": {"type": "synthetic"},
            "start": "2026-01-01T00:00:00Z", "end": "2026-01-02T00:00:00Z",
            "retrieved_at": "2026-01-03T00:00:00Z",
            "caller_assertion": "exact-query-result-target-window"
        }),
    );
    dir.capture(window(2, Some(1), json!([{"a": "1", "b": "1", "c": "1"}])));
    // An earlier-starting search that shows nothing new writes nothing.
    dir.capture(window(5, Some(4), json!([{"a": "1"}])));
    let fields = |state: &Value| {
        state["captured_observations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|observation| observation["fields"].as_array().unwrap().len())
            .collect::<Vec<_>>()
    };
    assert_eq!(fields(&dir.saved()), [3]);
    assert_eq!(dir.tally(), json!({"recorded": 2, "already-covered": 1}));

    // Nothing recent is removed: both captures' copies and audit records are still here.
    assert_eq!(names(&captures).len(), 5, "{:?}", names(&captures));
    assert_eq!(names(&audits).len(), 3);
    for directory in [&captures, &audits] {
        for name in names(directory) {
            age(&directory.join(name));
        }
    }
    let plant = |path: PathBuf, text: &str, old: bool| {
        fs::write(&path, text).unwrap();
        if old {
            age(&path);
        }
    };
    plant(
        captures.join("20260101T000000Z-0123456789ab.json"),
        "{}",
        true,
    );
    plant(
        captures.join("20260101T000000Z-0123456789ab.cql"),
        "x",
        true,
    );
    plant(
        captures.join("20260103T000000Z-0123456789ab.cql"),
        "x",
        true,
    );
    plant(
        captures.join("20260102T000000Z-0123456789ab.json"),
        "{}",
        false,
    );
    plant(captures.join("notes.json"), "{}", true);
    plant(
        audits.join("imported.json"),
        r#"{"observation":{"caller_assertion":"exact-query-result-target-window"}}"#,
        true,
    );

    dir.capture(window(1, None, json!([{"d": "1"}])));
    assert_eq!(fields(&dir.saved()), [3, 1]);
    let kept = names(&captures);
    // The replaced capture's result is gone; its query stays because the import names it.
    assert_eq!(
        replaced
            .iter()
            .filter(|name| kept.contains(name))
            .collect::<Vec<_>>(),
        [&reused],
        "{kept:?}"
    );
    assert_eq!(
        kept.iter()
            .filter(|name| name.contains("0123456789ab") || *name == "notes.json")
            .collect::<Vec<_>>(),
        ["20260102T000000Z-0123456789ab.json", "notes.json"]
    );
    // The held capture's aged copies stay, beside the new capture's and the tally.
    assert_eq!(kept.len(), 1 + 2 + 2 + 2 + 1, "{kept:?}");
    let kept = names(&audits);
    assert!(!kept.contains(&replaced_audit[0]), "{kept:?}");
    assert!(kept.contains(&"imported.json".to_owned()));
    assert_eq!(kept.len(), 4, "{kept:?}");
    assert!(!audits.join(format!(".{}.lock", replaced_audit[0])).exists());
}

#[test]
fn capture_never_tidies_the_files_of_another_state_in_its_directory() {
    let dir = Temp::new();
    dir.confirm();
    let second = dir.0.join("workflow/second.json");
    dir.act_on(
        &second,
        json!({"kind":"confirm-target","environment":environment(),"scope":"selected-sources","context":"Second synthetic target","expected_sources":[{"type":"synthetic"}]}),
    );
    let rows = |field: &str| {
        search(
            "#type=synthetic | head(5)",
            json!(body(json!([{field: "1"}]))),
        )
    };
    dir.capture_into(&second, "claude-code", SERVER, rows("a"));
    let copies = dir.0.join("workflow/second.json.captures");
    let audits = dir.0.join("workflow/query-observations");
    let before = (names(&copies), names(&audits));
    assert_eq!((before.0.len(), before.1.len()), (3, 1));
    for (directory, files) in [(&copies, &before.0), (&audits, &before.1)] {
        for name in files {
            age(&directory.join(name));
        }
    }

    // The first state shares the directory and the audit folder, and tidies after recording.
    dir.capture(rows("b"));
    assert_eq!(dir.tally(), json!({"recorded": 1}));
    assert_eq!(names(&copies), before.0);
    assert!(before.1.iter().all(|name| names(&audits).contains(name)));
    let second: Value = serde_json::from_slice(&fs::read(second).unwrap()).unwrap();
    assert_eq!(second["captured_observations"].as_array().unwrap().len(), 1);
}

#[test]
fn a_state_holding_captures_among_its_imports_still_loads() {
    let dir = Temp::new();
    dir.confirm();
    let rows = |field: &str| {
        search(
            "#type=synthetic | head(5)",
            json!(body(json!([{field: "1"}]))),
        )
    };
    dir.capture(rows("a"));
    // An earlier layout kept captures in the import list.
    let mut state = dir.saved();
    let captures = state["captured_observations"].take();
    state
        .as_object_mut()
        .unwrap()
        .remove("captured_observations");
    state["query_observations"] = captures;
    fs::write(dir.state(), state.to_string()).unwrap();

    dir.capture(rows("b"));
    assert_eq!(dir.tally(), json!({"recorded": 2}));
    let state = dir.saved();
    assert_eq!(state["query_observations"].as_array().unwrap().len(), 1);
    assert_eq!(state["captured_observations"].as_array().unwrap().len(), 1);
}

/// `cat` stands in for a server: it answers with whatever arrives, so the answer is sent too.
#[cfg(unix)]
#[test]
fn pass_through_relays_every_message_unchanged_and_records_a_finished_search() {
    let dir = Temp::new();
    dir.confirm();
    let call = |id: u64, query: &str| {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {
            "name": "falcon_search_ngsiem",
            "arguments": {
                "repository": "third-party", "query_string": query,
                "start": hours_ago(2), "end": hours_ago(1)
            }
        }})
    };
    let answer = |id: u64, rows: Value| {
        json!({"jsonrpc": "2.0", "id": id, "result": {
            "content": [{"type": "text", "text": body(rows)}], "isError": false, "_meta": {}
        }})
    };
    let messages = [
        json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}}).to_string(),
        "not JSON at all".to_owned(),
        call(1, "#type=synthetic | head(5)").to_string(),
        call(2, "#type=synthetic | count()").to_string(),
        answer(1, json!([{"user": "ann"}])).to_string(),
        answer(2, json!([{"_count": "1"}])).to_string(),
        json!({"jsonrpc": "2.0", "id": 3, "error": {"code": -32601, "message": "x"}}).to_string(),
    ];
    let input = messages
        .iter()
        .map(|message| format!("{message}\n"))
        .collect::<String>();
    let mut child = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args(["env", "pass-through", "--state"])
        .arg(dir.state())
        .args(["--", "cat"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), input);

    let state = dir.saved();
    assert_eq!(state["captured_observations"].as_array().unwrap().len(), 1);
    assert_eq!(
        state["captured_observations"][0]["fields"][0]["field"],
        "user"
    );
    assert_eq!(dir.tally(), json!({"recorded": 1, "not-creditable": 1}));

    // A server command that cannot start is reported, not passed through.
    let missing = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args([
            "env",
            "pass-through",
            "--",
            "cqlmatic-no-such-server-command",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(2));
    assert!(missing.stdout.is_empty());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("PASS_THROUGH"));
}

#[test]
fn capture_install_registers_an_absolute_state_path() {
    // The hook runs from whatever directory the client was started in.
    let home = Temp::new();
    let project = home.0.join("project");
    fs::create_dir(&project).unwrap();
    let mut command = install(&home.0, "codex");
    command
        .current_dir(&project)
        .args(["--program", "launcher", "--state", "relative/state.json"]);
    let output = report(&mut command);
    let registered = output["command"].as_str().unwrap();
    let expected = fs::canonicalize(&project)
        .unwrap()
        .join("relative")
        .join("state.json");
    let (_, state) = registered.split_once(" --state ").unwrap();
    let state = Path::new(state.trim_matches(|c| c == '\'' || c == '"'));
    assert!(state.is_absolute(), "{registered}");
    assert_eq!(state.file_name(), expected.file_name(), "{registered}");
    assert!(
        state.ends_with("project/relative/state.json"),
        "{registered}"
    );
}

#[test]
fn capture_install_takes_over_an_abandoned_candidate_and_yields_to_a_live_one() {
    let home = Temp::new();
    let root = home.0.join(".codex");
    fs::create_dir(&root).unwrap();
    let candidate = root.join(".hooks.json.cqlmatic.tmp");
    // A candidate another installation holds locked is left alone.
    fs::write(&candidate, "left over").unwrap();
    let held = fs::File::options().write(true).open(&candidate).unwrap();
    held.lock().unwrap();
    let mut command = install(&home.0, "codex");
    let refused = command.args(["--program", "launcher"]).output().unwrap();
    assert!(!refused.status.success(), "{refused:?}");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("Another installation"));
    assert!(!root.join("hooks.json").exists());
    drop(held);
    assert_eq!(fs::read(&candidate).unwrap(), b"left over");
    // With nobody holding it, it is from an interrupted installation and is taken over.
    let mut command = install(&home.0, "codex");
    let output = report(command.args(["--program", "launcher"]));
    assert_eq!(output["installed"], true);
    assert_eq!(output["changed"], true);
    let installed: Value =
        serde_json::from_slice(&fs::read(root.join("hooks.json")).unwrap()).unwrap();
    assert_eq!(installed, output["entry"]);
    assert!(!candidate.exists());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
}
