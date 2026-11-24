use cqlmatic::{
    contracts::{EnvironmentId, Scope},
    environment::{Pack, PackData},
    inventory::{InventoryState, MAX_STATE_BYTES, PackReference},
    storage::Writer,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{
        Barrier,
        atomic::{AtomicU64, Ordering},
    },
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
            .env("HOME", &self.0)
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
    /// Asks for the run's probe without supplying an answer.
    fn next_probe(&self) -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
            .env("HOME", &self.0)
            .args(["env", "inventory", "--json", "--next-probe", "--state"])
            .arg(self.state())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["action"].take()
    }
    /// Prepares a Quick run over two sources and reserves its first probe.
    fn probing(&self) -> Value {
        for action in [
            json!({"kind":"set-discovery-mode","mode":"quick","quick_depth":"sources-only"}),
            json!({"kind":"confirm-target","environment":environment(),"scope":"selected-sources","context":"Synthetic capture target","expected_sources":[{"type":"synthetic-a"},{"type":"synthetic-b"}]}),
            json!({"kind":"prepare-discovery","start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z","max_queries":4,"max_seconds":120}),
        ] {
            self.act_on(&self.state(), action);
        }
        let first = self.next_probe();
        assert_eq!(first["dispatched"], true, "{first}");
        first
    }
    /// Runs the hook command on one payload; it must stay silent and succeed whatever happens.
    fn capture(&self, payload: Value) {
        self.capture_as("claude-code", SERVER, payload);
    }
    fn capture_as(&self, client: &str, server: &str, payload: Value) {
        self.capture_into(&self.state(), client, server, payload);
    }
    fn capture_into(&self, state: &Path, client: &str, server: &str, payload: Value) {
        self.capture_bytes(state, client, server, &payload.to_string());
    }
    fn capture_bytes(&self, state: &Path, client: &str, server: &str, payload: &str) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
            .env("HOME", &self.0)
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
            .write_all(payload.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{payload}");
        assert!(
            output.stdout.is_empty() && output.stderr.is_empty(),
            "{output:?}"
        );
    }
}

fn check_numeric_capture_reuse(deliver: impl Fn(&Temp, &Value, &str)) {
    for number in ["0.123456789012345678", "9007199254740993.0"] {
        for identity in [r#""trace_id":"call","#, ""] {
            for capture_first in [false, true] {
                let home = Temp::new();
                let first = home.probing();
                let job = if identity.is_empty() {
                    r#""job_id":"job","#
                } else {
                    ""
                };
                let body = format!(
                    r#"{{{identity}"metadata":{{{job}"timing":{number}}},"result":[{{"events":23}}]}}"#
                );
                let file = home.0.join("answer.json");
                fs::write(&file, &body).unwrap();
                let submit = |probe: &Value| {
                    home.act_on(
                        &home.state(),
                        json!({"kind":"discovery-step","answers":[
                            {"probe_id":probe["probe_id"],"result_file":file}
                        ]}),
                    );
                };
                if capture_first {
                    deliver(&home, &first["probe"], &body);
                } else {
                    submit(&first["probe"]);
                }
                let second = home.next_probe();
                assert_ne!(second["probe"]["probe_id"], first["probe"]["probe_id"]);
                let checkpoint = home.saved()["discovery_runs"][0]["state_path"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let before = fs::read(&checkpoint).unwrap();
                if capture_first {
                    submit(&second["probe"]);
                } else {
                    deliver(&home, &second["probe"], &body);
                }
                let after = home.next_probe();
                assert_eq!(
                    after["diagnostics"][0]["code"], "MCP_RESULT_REUSED",
                    "{number}, {identity}, capture_first={capture_first}: {after}"
                );
                assert_eq!(fs::read(&checkpoint).unwrap(), before);

                let other = if number.starts_with("0.") {
                    "0.123456789012345679"
                } else {
                    "9007199254740992.0"
                };
                fs::write(&file, body.replace(number, other)).unwrap();
                submit(&second["probe"]);
                assert_eq!(home.next_probe()["state"], "complete");
            }
        }
    }
}

#[test]
fn capture_keeps_exact_numbers_for_reuse_in_both_delivery_orders() {
    check_numeric_capture_reuse(|home, probe, body| {
        let hook = probe_search(probe, json!("synthetic-placeholder")).to_string();
        home.capture_bytes(
            &home.state(),
            "generic",
            SERVER,
            &hook.replace(r#""synthetic-placeholder""#, body),
        );
    });
}

#[cfg(unix)]
#[test]
fn pass_through_keeps_exact_numbers_for_reuse_in_both_delivery_orders() {
    check_numeric_capture_reuse(|home, probe, body| {
        let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
            "name":probe["tool"],"arguments":probe["arguments"]
        }});
        let body = body.strip_suffix('}').unwrap();
        let response = format!(r#"{{"jsonrpc":"2.0","id":1,"result":{body},"_meta":{{}}}}}}"#);
        let input = format!("{request}\n{response}\n");
        let mut child = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
            .env("HOME", &home.0)
            .args(["env", "pass-through", "--state"])
            .arg(home.state())
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
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, input.as_bytes());
    });
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

/// The search a probe asks for, as Claude Code reports it after calling the tool directly.
fn probe_search(probe: &Value, response: Value) -> Value {
    json!({
        "hook_event_name": "PostToolUse",
        "tool_name": format!("mcp__{SERVER}__{}", probe["tool"].as_str().unwrap()),
        "tool_input": probe["arguments"],
        "tool_response": response,
    })
}

fn body(rows: Value) -> String {
    body_with_trace(rows, "00000000-0000-0000-0000-000000000000")
}

fn body_with_trace(rows: Value, trace_id: &str) -> String {
    json!({"result": rows, "trace_id": trace_id}).to_string()
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
    #[cfg(windows)]
    command.env("LOCALAPPDATA", home.join("data"));
    #[cfg(not(windows))]
    command.env_remove("LOCALAPPDATA");
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_DATA_HOME", home.join("data"))
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("COPILOT_HOME")
}

fn install(home: &Path, client: &str) -> Command {
    install_for(home, client, SERVER)
}

fn install_for(home: &Path, client: &str, server: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cqlmatic"));
    isolated(&mut command, home).args([
        "env",
        "capture",
        "--client",
        client,
        "--server",
        server,
        "--install",
    ]);
    command
}

fn report(command: &mut Command) -> Value {
    install_report(&command.output().unwrap())
}

fn install_report(output: &Output) -> Value {
    assert!(
        output.status.success() && output.stderr.is_empty(),
        "{output:?}"
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn install_error(output: &Output) -> Value {
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    serde_json::from_slice(&output.stderr).unwrap()
}

fn assert_manual(report: &Value, reason: &str) {
    assert_eq!(report["installed"], false, "{report}");
    assert_eq!(report["changed"], false, "{report}");
    assert!(report["entry"]["hooks"].is_object(), "{report}");
    assert!(
        report["next_actions"][0].as_str().unwrap().contains(reason),
        "{report}"
    );
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
        for field in [
            "path",
            "events",
            "command",
            "entry",
            "hooks",
            "next_actions",
        ] {
            assert_eq!(fresh[field], preview[field], "{client}, {field}");
        }
        // The Copilot clients share one entry.
        let registered = if client.starts_with("copilot-") {
            "copilot-cli"
        } else {
            client
        };
        // Claude Code and Codex run the command through a wrapper that ends every failure with
        // exit status 0 and no output; the command inside it is quoted once more for that.
        // Codex starts hooks with cmd.exe on Windows, which quotes with double quotes.
        let (before, quote, after) = match (client, cfg!(windows)) {
            ("claude-code" | "codex", false) => (
                r#"/bin/sh -c 'if out=$(/bin/sh -c "$1") && [ -n "$out" ]; then printf '\''%s\n'\'' "$out"; fi; exit 0' sh '"#,
                r"'\''",
                "' 2>/dev/null; exit 0",
            ),
            ("claude-code", true) => (
                "try { Invoke-Expression '",
                "''",
                "' 2>$null } catch {}; exit 0",
            ),
            ("codex", true) => ("", "\"", " 2>nul & exit 0"),
            _ => ("", "'", ""),
        };
        // Codex writes a server's hyphens as underscores, and the entry uses that spelling.
        let server = if client == "codex" {
            SERVER.replace('-', "_")
        } else {
            SERVER.into()
        };
        let check_command = |value: &Value| {
            let value = value.as_str().unwrap();
            assert!(value.starts_with(&format!("{before}launcher --label capture env capture --client {registered} --server {quote}{server}{quote} --state ")), "{value}");
            assert!(value.ends_with(after), "{value}");
            assert!(value.contains("inventory"), "{value}");
            assert!(value.contains("state.json"), "{value}");
        };
        let expected = if client.starts_with("copilot-") {
            assert_eq!(fresh["events"], json!(["postToolUse"]));
            assert_eq!(fresh["hooks"], json!({"capture": "added"}));
            check_command(&fresh["command"]["bash"]);
            check_command(&fresh["command"]["powershell"]);
            json!({"version": 1, "hooks": {"postToolUse": [{
                "type": "command", "bash": fresh["command"]["bash"],
                "powershell": fresh["command"]["powershell"],
            }]}})
        } else {
            assert_eq!(fresh["events"], json!(["PostToolUse", "PreToolUse"]));
            assert_eq!(
                fresh["hooks"],
                json!({"capture": "added", "guard": "added"})
            );
            check_command(&fresh["command"]);
            let mut capture =
                json!({"type": "command", "command": fresh["command"], "async": true});
            let mut guard = json!({"type": "command", "command": fresh["command"], "timeout": 5});
            if client == "claude-code" {
                guard["onFailure"] = json!("continue");
            }
            if cfg!(windows) && client == "claude-code" {
                capture["shell"] = json!("powershell");
                guard["shell"] = json!("powershell");
            }
            let matcher = format!("mcp__{server}__.*");
            json!({"hooks": {
                "PostToolUse": [{"matcher": matcher, "hooks": [capture]}],
                "PreToolUse": [{"matcher": matcher, "hooks": [guard]}],
            }})
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
        assert_eq!(entries(file.parent().unwrap()), with_lock(&file));
    }
}

/// Every name in a folder, hidden ones included.
fn entries(directory: &Path) -> Vec<String> {
    let mut names = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    names
}

/// What a settings folder holds after an installation: the lock file and the settings.
fn with_lock(settings: &Path) -> Vec<String> {
    let name = settings.file_name().unwrap().to_str().unwrap();
    vec![format!(".{name}.cqlmatic.lock"), name.to_owned()]
}

#[test]
fn capture_install_preserves_existing_settings_and_recognizes_only_exact_hooks() {
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
        command.args(["--hooks", "capture"]);
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

        let exact = serde_json::to_vec(&expected).unwrap();
        fs::write(&file, &exact).unwrap();
        let repeat = report(&mut command);
        assert_eq!(repeat["installed"], true, "{client}");
        assert_eq!(repeat["changed"], false, "{client}");
        assert_eq!(repeat["hooks"]["capture"], "present", "{client}");
        assert_eq!(fs::read(&file).unwrap(), exact, "{client}");

        // Only the whole entry is this installation's. One someone changed is reported, and
        // nothing that would run the command a second time is added beside it.
        let hook = &mut expected["hooks"][event][1];
        hook["extra-setting"] = json!(true);
        let changed = serde_json::to_vec(&expected).unwrap();
        fs::write(&file, &changed).unwrap();
        let repeat = report(&mut command);
        assert_manual(&repeat, "different hook entry");
        assert_eq!(repeat["hooks"]["capture"], "manual", "{client}");
        assert_eq!(fs::read(&file).unwrap(), changed, "{client}");

        let hook = &expected["hooks"][event][1];
        let mut customized = vec![hook.clone()];
        if copilot {
            customized[0]["powershell"] = json!("another command");
            let mut other_shell = hook.clone();
            other_shell["bash"] = json!("another command");
            customized.push(other_shell);
        } else {
            customized[0]["matcher"] = json!("alternate matcher");
        }
        for custom in customized {
            expected["hooks"][event][1] = custom;
            let wanted = serde_json::to_vec(&expected).unwrap();
            fs::write(&file, &wanted).unwrap();
            for dry_run in [false, true] {
                let mut command = install(&home.0, client);
                command.args(["--hooks", "capture"]);
                if dry_run {
                    command.arg("--dry-run");
                }
                let repeat = report(&mut command);
                assert_manual(&repeat, "different hook entry");
                assert_eq!(fs::read(&file).unwrap(), wanted, "{client}");
            }
        }
    }
}

#[test]
fn capture_install_shares_one_entry_among_the_copilot_clients() {
    let home = Temp::new();
    let file = home.0.join(".copilot/hooks/cqlmatic-capture.json");
    let first = report(&mut install(&home.0, "copilot-cli"));
    assert_eq!(first["changed"], true);
    let written = fs::read(&file).unwrap();
    // Every Copilot client reads this file, so another client adds nothing.
    for client in ["copilot-app", "copilot-vscode"] {
        let repeat = report(&mut install(&home.0, client));
        assert_eq!(repeat["installed"], true, "{client}");
        assert_eq!(repeat["changed"], false, "{client}");
        assert_eq!(repeat["entry"], first["entry"], "{client}");
    }
    assert_eq!(fs::read(&file).unwrap(), written);
}

#[test]
fn capture_install_reports_a_byte_order_mark_for_manual_installation() {
    // Clients do not read a settings file that starts with the mark.
    let home = Temp::new();
    let file = home.0.join(".codex/hooks.json");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let original = "\u{feff}{}";
    fs::write(&file, original).unwrap();
    let output = report(&mut install(&home.0, "codex"));
    assert_manual(&output, "byte order mark");
    assert_eq!(fs::read_to_string(&file).unwrap(), original);
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
    // Settings the installer cannot merge into stay as they are, and the entry is reported.
    for (bytes, reason) in [
        (&b"{"[..], "strict JSON"),
        (b"{} {}", "strict JSON"),
        (b"// note\n{}", "strict JSON"),
        (b"[]", "a JSON object"),
        (b"null", "a JSON object"),
        (br#"{"duplicate":1,"duplicate":2}"#, "unique keys"),
        (br#"{"hooks":null}"#, "hooks to be a JSON object"),
        (br#"{"hooks":[]}"#, "hooks to be a JSON object"),
        (br#"{"hooks":{"PostToolUse":{}}}"#, "to be a JSON array"),
        (br#"{"hooks":{"PostToolUse":null}}"#, "to be a JSON array"),
        (br#"{"number": 1e400}"#, "number out of range"),
    ] {
        fs::write(&file, bytes).unwrap();
        for dry_run in [true, false] {
            let mut command = command();
            if dry_run {
                command.arg("--dry-run");
            }
            assert_manual(&report(&mut command), reason);
            assert_eq!(fs::read(&file).unwrap(), bytes);
            assert_eq!(entries(&root), ["hooks.json"]);
        }
    }
    let assert_refused = |code| {
        let output = command().output().unwrap();
        let d = install_error(&output);
        assert_eq!(d["code"], code);
        if code == "LOCAL_FILE_TYPE" {
            assert_manual(
                &serde_json::from_slice(&output.stdout).unwrap(),
                "regular file",
            );
        } else {
            assert!(output.stdout.is_empty());
        }
        assert!(
            d["message"]
                .as_str()
                .unwrap()
                .contains(file.to_str().unwrap()),
            "{d}"
        );
        assert_eq!(entries(&root), ["hooks.json"]);
    };
    let oversized = vec![b' '; 8 * 1024 * 1024 + 1];
    fs::write(&file, &oversized).unwrap();
    assert_refused("LOCAL_SIZE");
    assert_eq!(fs::read(&file).unwrap(), oversized);
    fs::remove_file(&file).unwrap();
    fs::create_dir(&file).unwrap();
    assert_refused("LOCAL_FILE_TYPE");
    assert!(file.is_dir());
}

#[test]
fn capture_install_reports_read_only_settings_for_manual_installation() {
    let home = Temp::new();
    let file = home.0.join(".codex/hooks.json");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "{}").unwrap();
    let mut permissions = fs::metadata(&file).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&file, permissions.clone()).unwrap();
    for dry_run in [true, false] {
        let mut command = install(&home.0, "codex");
        if dry_run {
            command.arg("--dry-run");
        }
        assert_manual(&report(&mut command), "read-only");
        assert_eq!(fs::read(&file).unwrap(), b"{}");
        assert_eq!(entries(file.parent().unwrap()), ["hooks.json"]);
    }
    // Let the temporary folder be removed on every platform.
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(&file, permissions).unwrap();
}

#[cfg(any(unix, windows))]
#[test]
fn capture_install_reports_a_link_it_cannot_follow_for_manual_installation() {
    // On Windows that is any linked settings file; elsewhere, a link whose target is missing.
    let home = Temp::new();
    let root = home.0.join(".claude");
    let file = root.join("settings.json");
    let target = home.0.join("settings.json");
    let preview = report(install(&home.0, "claude-code").arg("--dry-run"));
    fs::create_dir(&root).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &file).unwrap();
    #[cfg(windows)]
    let original = serde_json::to_vec(&preview["entry"]).unwrap();
    #[cfg(windows)]
    {
        fs::write(&target, &original).unwrap();
        match std::os::windows::fs::symlink_file(&target, &file) {
            Err(error) if error.raw_os_error() == Some(1314) => return, // Symlink privilege unavailable.
            result => result.unwrap(),
        }
    }
    for dry_run in [true, false] {
        let mut command = install(&home.0, "claude-code");
        if dry_run {
            command.arg("--dry-run");
        }
        let output = report(&mut command);
        assert_manual(&output, "symbolic link");
        assert_eq!(output["path"], json!(file));
        assert_eq!(output["entry"], preview["entry"]);
        assert_eq!(fs::read_link(&file).unwrap(), target);
        #[cfg(unix)]
        assert!(!target.exists());
        #[cfg(windows)]
        assert_eq!(fs::read(&target).unwrap(), original);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    }

    // A link that leads back to itself cannot be followed either.
    #[cfg(unix)]
    {
        let root = home.0.join(".codex");
        fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink("other.json", root.join("hooks.json")).unwrap();
        std::os::unix::fs::symlink("hooks.json", root.join("other.json")).unwrap();
        for dry_run in [true, false] {
            let mut command = install(&home.0, "codex");
            if dry_run {
                command.arg("--dry-run");
            }
            assert_manual(&report(&mut command), "cannot be followed");
            assert_eq!(entries(&root), ["hooks.json", "other.json"]);
        }
    }
}

#[cfg(unix)]
#[test]
fn capture_install_edits_a_linked_settings_file_where_it_really_is() {
    let home = Temp::new();
    let root = home.0.join(".claude");
    let file = root.join("settings.json");
    let store = home.0.join("dotfiles");
    let target = store.join("claude.json");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&store).unwrap();
    let original = "{\"existing\":true}\n";
    fs::write(&target, original).unwrap();
    // A relative link through a second link, as a dotfile manager may leave.
    std::os::unix::fs::symlink("claude.json", store.join("alias.json")).unwrap();
    let link = Path::new("../dotfiles/alias.json");
    std::os::unix::fs::symlink(link, &file).unwrap();

    let preview = report(install(&home.0, "claude-code").arg("--dry-run"));
    assert_eq!(preview["installed"], false);
    assert_eq!(preview["changed"], true);
    assert_eq!(fs::read_to_string(&target).unwrap(), original);
    assert_eq!(entries(&store), ["alias.json", "claude.json"]);

    let output = report(&mut install(&home.0, "claude-code"));
    assert_eq!(output["installed"], true);
    assert_eq!(output["changed"], true);
    assert_eq!(output["path"], json!(file));
    // Both links are untouched. The lock stays in the client's folder, so the folder that
    // holds the real file gains nothing.
    assert_eq!(fs::read_link(&file).unwrap(), link);
    assert_eq!(
        fs::read_link(store.join("alias.json")).unwrap(),
        Path::new("claude.json")
    );
    assert_eq!(entries(&root), with_lock(&file));
    assert_eq!(entries(&store), ["alias.json", "claude.json"]);
    let settings: Value = serde_json::from_slice(&fs::read(&target).unwrap()).unwrap();
    assert_eq!(settings["existing"], true);
    assert_eq!(settings["hooks"], output["entry"]["hooks"]);

    let repeat = report(&mut install(&home.0, "claude-code"));
    assert_eq!(repeat["installed"], true);
    assert_eq!(repeat["changed"], false);

    // A refusal about the real file names it, and the link that led to it.
    use std::os::unix::fs::PermissionsExt;
    fs::write(&target, "{}").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o444)).unwrap();
    let refused = report(&mut install(&home.0, "claude-code"));
    assert_manual(&refused, "read-only");
    let reason = refused["next_actions"][0].as_str().unwrap();
    let real = fs::canonicalize(&target).unwrap();
    assert!(reason.contains(real.to_str().unwrap()), "{reason}");
    assert!(reason.contains("linked from"), "{reason}");
    assert!(reason.contains(file.to_str().unwrap()), "{reason}");
    assert_eq!(fs::read(&target).unwrap(), b"{}");
}

#[cfg(unix)]
#[test]
fn capture_installs_through_different_links_to_one_file_take_turns() {
    // Two profiles keep one settings file. Their lock files differ, so they meet on the file.
    let home = Temp::new();
    let store = home.0.join("dotfiles");
    let target = store.join("claude.json");
    fs::create_dir(&store).unwrap();
    fs::write(&target, "{}").unwrap();
    let profiles = [home.0.join("profile-a"), home.0.join("profile-b")];
    for profile in &profiles {
        fs::create_dir(profile).unwrap();
        std::os::unix::fs::symlink(&target, profile.join("settings.json")).unwrap();
    }
    let install_in = |profile: &Path, server: &str| {
        let mut command = install_for(&home.0, "claude-code", server);
        command.env("CLAUDE_CONFIG_DIR", profile);
        command
    };

    // Another installation is in the middle of editing the real file.
    let held = fs::File::open(&target).unwrap();
    held.lock().unwrap();
    let busy = install_in(&profiles[0], "one").output().unwrap();
    assert_eq!(
        install_error(&busy)["message"],
        "Another hook installation is running. Retry."
    );
    assert!(busy.stdout.is_empty());
    assert_eq!(fs::read(&target).unwrap(), b"{}");
    drop(held);

    for (profile, server) in profiles.iter().zip(["one", "two"]) {
        let output = report(&mut install_in(profile, server));
        assert_eq!(output["installed"], true, "{server}");
        assert_eq!(output["changed"], true, "{server}");
    }
    let settings: Value = serde_json::from_slice(&fs::read(&target).unwrap()).unwrap();
    assert_eq!(
        settings["hooks"]["PostToolUse"].as_array().unwrap().len(),
        2
    );
    assert_eq!(entries(&store), ["claude.json"]);

    // A profile that holds its settings file directly meets other installers on it too.
    let direct = home.0.join("profile-c");
    fs::create_dir(&direct).unwrap();
    let own = direct.join("settings.json");
    fs::write(&own, "{}").unwrap();
    let held = fs::File::open(&own).unwrap();
    held.lock().unwrap();
    let busy = install_in(&direct, "three").output().unwrap();
    assert_eq!(
        install_error(&busy)["message"],
        "Another hook installation is running. Retry."
    );
    assert_eq!(fs::read(&own).unwrap(), b"{}");
    drop(held);
    let output = report(&mut install_in(&direct, "three"));
    assert_eq!(output["installed"], true);
}

#[test]
fn capture_install_reports_hard_linked_settings_for_manual_installation() {
    // Swapping in a new file would leave the second name pointing at the old one.
    let home = Temp::new();
    let file = home.0.join(".codex/hooks.json");
    let twin = home.0.join("hooks-copy.json");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "{}").unwrap();
    fs::hard_link(&file, &twin).unwrap();
    for dry_run in [true, false] {
        let mut command = install(&home.0, "codex");
        if dry_run {
            command.arg("--dry-run");
        }
        assert_manual(&report(&mut command), "more than one hard link");
        assert_eq!(fs::read(&file).unwrap(), b"{}");
        assert_eq!(fs::read(&twin).unwrap(), b"{}");
        assert_eq!(entries(file.parent().unwrap()), ["hooks.json"]);
    }
}

#[test]
fn capture_install_still_reports_the_entry_when_it_cannot_write() {
    let home = Temp::new();
    let root = home.0.join(".codex");
    let file = root.join("hooks.json");
    fs::create_dir_all(&root).unwrap();
    fs::write(&file, "{}").unwrap();
    // Something other than a file sits where the lock belongs.
    fs::create_dir(root.join(".hooks.json.cqlmatic.lock")).unwrap();
    let preview = report(install(&home.0, "codex").arg("--dry-run"));
    let output = install(&home.0, "codex").output().unwrap();
    let error = install_error(&output);
    assert_eq!(error["code"], "LOCAL_FILE_TYPE");
    let failed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(failed["installed"], false);
    assert_eq!(failed["changed"], false);
    assert_eq!(failed["entry"], preview["entry"]);
    assert!(
        failed["next_actions"][0]
            .as_str()
            .unwrap()
            .contains("merge the reported entry by hand")
    );
    assert_eq!(fs::read(&file).unwrap(), b"{}");
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
    assert_eq!(entries(&root), with_lock(&file));
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
    let file = root.join("hooks.json");
    let settings: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    assert_eq!(settings, output["entry"]);
    assert_eq!(
        fs::metadata(file).unwrap().permissions().mode() & 0o7777,
        0o600
    );
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o770
    );
}

#[cfg(unix)]
#[test]
fn capture_install_refuses_world_writable_settings_before_merge() {
    use std::os::unix::fs::PermissionsExt;

    let home = Temp::new();
    let root = home.0.join(".codex");
    let file = root.join("hooks.json");
    let entry = report(install(&home.0, "codex").arg("--dry-run"))["entry"].clone();
    let installed = serde_json::to_vec(&entry).unwrap();
    fs::create_dir(&root).unwrap();
    for (shared, shared_mode, private_mode, reason) in [
        (&file, 0o606, 0o600, "file is writable by everyone"),
        (&root, 0o707, 0o700, "directory is writable by everyone"),
    ] {
        for bytes in [installed.as_slice(), b"{"] {
            fs::write(&file, bytes).unwrap();
            fs::set_permissions(shared, fs::Permissions::from_mode(shared_mode)).unwrap();
            check_settings_refusal(&home, &file, bytes, &entry, reason);
        }
        fs::set_permissions(shared, fs::Permissions::from_mode(private_mode)).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn capture_install_refuses_foreign_owners_before_merge() {
    use std::os::unix::fs::{MetadataExt, chown};

    let home = Temp::new();
    let root = home.0.join(".codex");
    let file = root.join("hooks.json");
    let entry = report(install(&home.0, "codex").arg("--dry-run"))["entry"].clone();
    let bytes = serde_json::to_vec(&entry).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(&file, &bytes).unwrap();
    let user = fs::metadata(&home.0).unwrap().uid();
    for (path, reason) in [
        (&file, "file is owned by another user"),
        (&root, "directory is owned by another user"),
    ] {
        match chown(path, Some(user + 1), None) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("Foreign owner fixture requires permission to change its owner.");
                return;
            }
            Err(e) => panic!("Cannot change fixture owner. {e}"),
        }
        check_settings_refusal(&home, &file, &bytes, &entry, reason);
        chown(path, Some(user), None).unwrap();
    }
}

#[cfg(unix)]
fn check_settings_refusal(home: &Temp, path: &Path, bytes: &[u8], entry: &Value, reason: &str) {
    for dry_run in [true, false] {
        let mut command = install(&home.0, "codex");
        if dry_run {
            command.arg("--dry-run");
        }
        let report = report(&mut command);
        assert_manual(&report, reason);
        assert_eq!(&report["entry"], entry);
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }
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
        assert_eq!(
            output["events"],
            json!(event.into_iter().collect::<Vec<_>>())
        );
        assert_eq!(output["hooks"], json!({"capture": "manual"}));
        // The command names the state this installation resolved.
        let command = output["command"].as_str().unwrap();
        let (invocation, state) = command.split_once(" --state ").unwrap();
        assert_eq!(
            invocation,
            format!("launcher env capture --client {client} --server '{SERVER}'")
        );
        assert!(state.contains("state.json"), "{command}");
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
fn capture_install_rejects_an_empty_server_before_reading_settings() {
    let home = Temp::new();
    let output = install_for(&home.0, "codex", "").output().unwrap();
    let refused = install_error(&output);
    assert_eq!(refused["code"], "CLI_INPUT");
    assert_eq!(refused["message"], "--server must be non-empty.");
    assert!(output.stdout.is_empty());
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
    fs::set_permissions(&path, fs::Permissions::from_mode(0o7764)).unwrap();
    // The system can drop a special bit, for example in a folder of a group the user is not in.
    let mode = || fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
    let before = mode();
    assert_eq!(before & 0o777, 0o764);

    assert_eq!(report(&mut install(&home.0, "codex"))["changed"], true);
    assert_eq!(mode(), before);
}

#[cfg(windows)]
fn powershell() -> Command {
    let root = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
    let mut shell = Command::new(root.join("System32/WindowsPowerShell/v1.0/powershell.exe"));
    shell.args(["-NoProfile", "-NonInteractive", "-Command"]);
    shell
}

/// The built executable under a name and folder that need quoting.
fn runner_in(directory: &Path) -> PathBuf {
    let source = env!("CARGO_BIN_EXE_cqlmatic");
    let executable = directory.join(format!("capture's $runner{}", std::env::consts::EXE_SUFFIX));
    // Changing the link count on macOS can kill concurrent launches of the Cargo executable.
    if cfg!(target_os = "macos") || fs::hard_link(source, &executable).is_err() {
        fs::copy(source, &executable).unwrap();
        // Another test's child process, started during the copy, can hold the new file open
        // for writing for a moment, and Linux will not run a file in that state.
        #[cfg(not(target_os = "macos"))]
        while let Err(error) = Command::new(&executable).arg("--version").output() {
            assert_eq!(error.kind(), std::io::ErrorKind::ExecutableFileBusy);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    executable
}

#[test]
fn installed_hook_runs_the_native_command_and_captures_evidence() {
    let dir = Temp::new();
    let directory = dir.0.join("work folder's $data; settings");
    let state = directory.join("state.json");
    dir.confirm_at(&state);
    let executable = runner_in(&directory);
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
fn installed_codex_hook_runs_the_way_codex_starts_it() {
    let dir = Temp::new();
    let directory = dir.0.join("work & folder's $data; ^(settings)!");
    let state = directory.join("state.json");
    dir.confirm_at(&state);
    let executable = runner_in(&directory);
    // The name as configured; Codex spells it with underscores in tool names.
    let server = "synthetic-server";
    let mut installer = Command::new(&executable);
    isolated(&mut installer, &dir.0)
        .args([
            "env",
            "capture",
            "--client",
            "codex",
            "--server",
            server,
            "--install",
            "--state",
        ])
        .arg(&state);
    let installed = report(&mut installer);
    let settings: Value =
        serde_json::from_slice(&fs::read(dir.0.join(".codex/hooks.json")).unwrap()).unwrap();
    let command = settings["hooks"]["PostToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert_eq!(installed["command"], command);
    let response = body(json!([{"synthetic_field": "seen"}]));
    let mut payload = search(
        "#type=synthetic | head(5)",
        json!({
            "content": [{"type": "text", "text": response}], "isError": false,
        }),
    );
    payload["tool_name"] = json!("mcp__synthetic_server__execute_tool");
    assert_eq!(
        settings["hooks"]["PostToolUse"][0]["matcher"],
        "mcp__synthetic_server__.*"
    );
    // Installing under Codex's own spelling finds the same entry.
    let mut again = Command::new(&executable);
    isolated(&mut again, &dir.0)
        .args(["env", "capture", "--client", "codex", "--server"])
        .args(["synthetic_server", "--install", "--state"])
        .arg(&state);
    let again = report(&mut again);
    assert_eq!(again["installed"], true);
    assert_eq!(again["changed"], false);
    // Codex passes the command to the login shell, or to `cmd.exe /C` inside one pair of quotes.
    #[cfg(unix)]
    let mut shell = {
        let mut shell = Command::new("/bin/sh");
        shell.args(["-c", command]);
        shell
    };
    #[cfg(windows)]
    let mut shell = {
        use std::os::windows::process::CommandExt;
        let mut shell = Command::new(std::env::var_os("COMSPEC").unwrap());
        shell.arg("/C").raw_arg(format!("\"{command}\""));
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
    // The configured spelling, hyphens and all, names the same server.
    dir.capture_as(
        "codex",
        SERVER,
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

#[cfg(unix)]
#[test]
fn pass_through_answers_the_outstanding_probe_of_a_run() {
    let home = Temp::new();
    let first = home.probing();
    let input = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": first["probe"]["tool"], "arguments": first["probe"]["arguments"]
        }}),
        json!({"jsonrpc": "2.0", "id": 1, "result": {
            "content": [{"type": "text", "text": body(json!([{"events": 10}]))}],
            "isError": false
        }}),
    ]
    .map(|message| format!("{message}\n"))
    .concat();
    let mut child = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args(["env", "pass-through", "--state"])
        .arg(home.state())
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

    let second = home.next_probe();
    assert_eq!(second["dispatched"], true, "{second}");
    assert_ne!(second["probe"]["probe_id"], first["probe"]["probe_id"]);
    assert_eq!(home.tally(), json!({"probe-answered": 1}));
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
    // The path is the word after --state, in the quotes of the wrapper and of the command.
    let (_, state) = registered.split_once(" --state ").unwrap();
    let state = state.split_whitespace().next().unwrap();
    let state = Path::new(state.trim_matches(|c| matches!(c, '\'' | '"' | '\\')));
    assert!(state.is_absolute(), "{registered}");
    assert_eq!(state.file_name(), expected.file_name(), "{registered}");
    assert!(
        state.ends_with("project/relative/state.json"),
        "{registered}"
    );

    let mut command = install(&home.0, "codex");
    command.current_dir(&home.0).args(["--state", "project"]);
    let refused = install_error(&command.output().unwrap());
    assert_eq!(refused["code"], "CLI_INPUT");
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .contains("not a directory")
    );
}

#[test]
fn capture_install_refuses_a_held_lock_and_succeeds_after_release() {
    let home = Temp::new();
    let root = home.0.join(".codex");
    let file = root.join("hooks.json");
    fs::create_dir(&root).unwrap();
    let original = b"{\"existing\":true}\n";
    fs::write(&file, original).unwrap();
    // The lock stays beside the settings it guards.
    let lock_path = root.join(".hooks.json.cqlmatic.lock");
    let held = fs::File::create(&lock_path).unwrap();

    held.lock().unwrap();
    let busy = install(&home.0, "codex").output().unwrap();
    assert_eq!(
        install_error(&busy)["message"],
        "Another hook installation is running. Retry."
    );
    assert!(busy.stdout.is_empty());
    let preview = report(install(&home.0, "codex").arg("--dry-run"));
    assert_eq!(preview["installed"], false);
    assert_eq!(preview["changed"], true);
    assert_eq!(fs::read(&file).unwrap(), original);
    assert_eq!(entries(&root), with_lock(&file));

    drop(held);
    let updated = report(&mut install(&home.0, "codex"));
    assert_eq!(updated["installed"], true);
    assert_eq!(updated["changed"], true);
    let installed: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    assert_eq!(installed["existing"], true);
    assert_eq!(installed["hooks"], updated["entry"]["hooks"]);
    assert_eq!(fs::metadata(&lock_path).unwrap().len(), 0);
    assert_eq!(entries(&root), with_lock(&file));
}

#[test]
fn concurrent_capture_install_processes_keep_every_reported_hook() {
    let home = Temp::new();
    assert_eq!(
        report(&mut install(&home.0, "claude-code"))["installed"],
        true
    );
    let root = home.0.join(".codex");
    let file = root.join("hooks.json");
    fs::create_dir(&root).unwrap();
    let existing =
        r#"{"matcher":"keep","hooks":[{"type":"command","command":"keep quoted spelling"}]}"#;
    let original = format!(
        "{{\r\n  \"padding\" : \"{}\",\r\n  \"hooks\": {{\"OtherEvent\": [], \"PostToolUse\": [{existing}]}},\r\n  \"keep\" : 1e+01\r\n}}\r\n",
        "x".repeat(512 * 1024)
    );
    fs::write(&file, &original).unwrap();

    let start = Barrier::new(3);
    let run = |server| {
        let mut command = install_for(&home.0, "codex", server);
        command.args(["--hooks", "capture"]);
        start.wait();
        command.output().unwrap()
    };
    let results = std::thread::scope(|scope| {
        let a = scope.spawn(|| run("first"));
        let b = scope.spawn(|| run("second"));
        start.wait();
        [("first", a.join().unwrap()), ("second", b.join().unwrap())]
    });

    let saved = fs::read_to_string(&file).unwrap();
    let after: Value = serde_json::from_str(&saved).unwrap();
    let hooks = after["hooks"]["PostToolUse"].as_array().unwrap();
    let mut installed = 0;
    let mut refused = Vec::new();
    for (server, result) in &results {
        if result.status.success() {
            let report = install_report(result);
            assert_eq!(report["installed"], true, "{report}");
            assert_eq!(report["changed"], true, "{report}");
            let entry = &report["entry"]["hooks"]["PostToolUse"][0];
            assert_eq!(entry["matcher"], format!("mcp__{server}__.*"));
            assert_eq!(hooks.iter().filter(|hook| *hook == entry).count(), 1);
            installed += 1;
        } else {
            let error = install_error(result);
            assert!(
                matches!(
                    error["message"].as_str().unwrap(),
                    "Another hook installation is running. Retry."
                        | "Settings changed during installation. Retry."
                ),
                "{error}"
            );
            assert!(result.stdout.is_empty());
            refused.push(server);
        }
    }
    assert!(installed > 0);
    assert_eq!(hooks.len(), 1 + installed);
    let mut without_installed = after;
    without_installed["hooks"]["PostToolUse"]
        .as_array_mut()
        .unwrap()
        .truncate(1);
    assert_eq!(
        without_installed,
        serde_json::from_str::<Value>(&original).unwrap()
    );
    let insertion = original.find(existing).unwrap() + existing.len();
    assert!(saved.starts_with(&original[..insertion]));
    assert!(saved.ends_with(&original[insertion..]));
    for server in refused {
        assert_eq!(
            report(install_for(&home.0, "codex", server).args(["--hooks", "capture"]))["installed"],
            true
        );
    }
    let saved: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    let hooks = saved["hooks"]["PostToolUse"].as_array().unwrap();
    assert_eq!(hooks.len(), 3);
    for server in ["first", "second"] {
        assert_eq!(
            hooks
                .iter()
                .filter(|hook| hook["matcher"] == format!("mcp__{server}__.*"))
                .count(),
            1
        );
    }
}

#[test]
fn capture_answers_the_outstanding_probe_of_a_run() {
    let home = Temp::new();
    let first = home.probing();
    let counted = json!(body(json!([{"events": 10}])));
    home.capture(probe_search(&first["probe"], counted.clone()));

    let second = home.next_probe();
    assert_eq!(second["state"], "probe", "{second}");
    assert_eq!(second["dispatched"], true);
    assert_ne!(second["probe"]["probe_id"], first["probe"]["probe_id"]);
    assert_eq!(second["cost"]["searches"]["used"], 2);
    assert!(
        first["next_action"]
            .as_str()
            .unwrap()
            .ends_with("submit next-probe with run_index=0 and answer."),
        "{first}"
    );
    assert!(
        second["next_action"]
            .as_str()
            .unwrap()
            .contains("submit next-probe with run_index=0 and no answer"),
        "{second}"
    );
    assert_eq!(home.tally(), json!({"probe-answered": 1}));
    assert_eq!(
        names(&home.0.join("workflow/state.json.captures")),
        ["tally.json"]
    );

    // The hosted server's gateway carries the same arguments one level down.
    let second_counted = json!(body_with_trace(
        json!([{"events": 10}]),
        "00000000-0000-0000-0000-000000000002",
    ));
    let mut through_gateway = probe_search(&second["probe"], second_counted);
    through_gateway["tool_name"] = json!(format!("mcp__{SERVER}__execute_tool"));
    through_gateway["tool_input"] =
        json!({"name": second["probe"]["tool"], "arguments": second["probe"]["arguments"]});
    home.capture(through_gateway);
    let done = home.next_probe();
    assert_eq!(done["state"], "complete", "{done}");
    assert_eq!(done["cost"]["searches"]["used"], 2);
    assert_eq!(home.tally(), json!({"probe-answered": 2}));
    assert!(home.saved().get("captured_observations").is_none());
}

#[test]
fn capture_answers_a_probe_only_with_its_own_completed_search() {
    let home = Temp::new();
    let first = home.probing();
    let counted = json!(body(json!([{"events": 10}])));
    let still_outstanding = |after: &str| {
        let reread = home.next_probe();
        assert_eq!(reread["state"], "probe", "{after}: {reread}");
        assert_eq!(reread["dispatched"], false, "{after}");
        assert_eq!(reread["probe"], first["probe"], "{after}");
        assert_eq!(reread["cost"]["searches"]["used"], 1, "{after}");
    };

    let mut other_query = probe_search(&first["probe"], counted.clone());
    other_query["tool_input"]["query_string"] = json!("#type = \"synthetic-a\" | count()");
    home.capture(other_query);
    still_outstanding("a different query");

    let mut extra_argument = probe_search(&first["probe"], counted.clone());
    extra_argument["tool_input"]["limit"] = json!(10);
    home.capture(extra_argument);
    still_outstanding("an added argument");

    let mut beside_the_gateway = probe_search(&first["probe"], counted.clone());
    beside_the_gateway["tool_name"] = json!(format!("mcp__{SERVER}__execute_tool"));
    beside_the_gateway["tool_input"] = json!({
        "name": first["probe"]["tool"], "arguments": first["probe"]["arguments"], "timeout": 5
    });
    home.capture(beside_the_gateway);
    still_outstanding("a gateway call with something beside the arguments");

    let mut failed = probe_search(&first["probe"], Value::Null);
    failed["hook_event_name"] = json!("PostToolUseFailure");
    home.capture(failed);
    still_outstanding("a failed call");

    home.capture(probe_search(
        &first["probe"],
        json!("NGSIEM search timed out after 20 seconds"),
    ));
    still_outstanding("a search that timed out");
    assert!(home.tally().get("probe-answered").is_none());

    // A copied state does not own the run, so its hook leaves the run alone.
    let copy = home.0.join("copy/state.json");
    fs::create_dir(home.0.join("copy")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(home.0.join("copy"), fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::copy(home.state(), &copy).unwrap();
    home.capture_into(
        &copy,
        "claude-code",
        SERVER,
        probe_search(&first["probe"], counted.clone()),
    );
    still_outstanding("a copied state");

    home.capture(probe_search(&first["probe"], counted));
    assert_eq!(home.next_probe()["dispatched"], true);
    assert_eq!(home.tally()["probe-answered"], 1);
}

#[test]
fn capture_counts_a_probe_result_that_is_refused() {
    let home = Temp::new();
    let first = home.probing();
    home.capture(probe_search(
        &first["probe"],
        json!(body(json!([{"user": "ann"}]))),
    ));
    // The run treats the result as it would one the agent submitted.
    let after = home.next_probe();
    assert_eq!(after["state"], "paused", "{after}");
    assert_eq!(after["cost"]["searches"]["used"], 1);
    assert_eq!(home.tally(), json!({"probe-not-answered": 1}));
    assert_eq!(
        names(&home.0.join("workflow/state.json.captures")),
        ["tally.json"]
    );
}

#[test]
fn capture_answers_a_probe_despite_a_leftover_copy() {
    let home = Temp::new();
    let first = home.probing();
    let counted = body(json!([{"events": 10}]));
    home.capture(probe_search(
        &first["probe"],
        json!({"content": [{"type": "text", "text": counted}], "isError": false}),
    ));
    let second = home.next_probe();
    assert_eq!(second["dispatched"], true, "{second}");
    assert_ne!(second["probe"]["probe_id"], first["probe"]["probe_id"]);

    // A hook stopped after saving its copy must not block the same result later.
    let captures = home.0.join("workflow/state.json.captures");
    for entry in fs::read_dir(&captures).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        assert!(!name.starts_with("probe-"), "{name}");
    }
    let second_counted = body_with_trace(
        json!([{"events": 10}]),
        "00000000-0000-0000-0000-000000000002",
    );
    let leftover = captures.join(format!(
        "probe-{}.json",
        &cqlmatic::sha256_hex(second_counted.as_bytes())[..12]
    ));
    fs::write(&leftover, &second_counted).unwrap();
    home.capture(probe_search(&second["probe"], json!(second_counted)));
    assert_eq!(home.next_probe()["state"], "complete");
    assert_eq!(home.tally(), json!({"probe-answered": 2}));
}

#[test]
fn capture_answers_the_probe_of_a_resumed_earlier_run() {
    let home = Temp::new();
    let first = home.probing();
    let state = home.state();
    home.act_on(&state, json!({"kind":"pause-discovery","run_index":0}));
    // The pause asked for the outstanding search to finish; hand its result in.
    let counted = body(json!([{"events": 10}]));
    let saved = home.0.join("first.json");
    fs::write(&saved, &counted).unwrap();
    home.act_on(
        &state,
        json!({"kind":"discovery-step","run_index":0,"answers":[{"probe_id":first["probe"]["probe_id"],"result_file":saved}]}),
    );
    home.act_on(
        &state,
        json!({"kind":"prepare-discovery","start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z","max_queries":5,"max_seconds":120}),
    );
    home.act_on(&state, json!({"kind":"pause-discovery","run_index":1}));
    home.act_on(&state, json!({"kind":"resume-discovery","run_index":0}));
    let reserve = |extra: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
            .args([
                "env",
                "inventory",
                "--json",
                "--next-probe",
                "--run-index",
                "0",
            ])
            .args(extra)
            .arg("--state")
            .arg(&state)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["action"].take()
    };
    let second = reserve(&[]);
    assert_eq!(second["state"], "probe", "{second}");
    let second_counted = body_with_trace(
        json!([{"events": 10}]),
        "00000000-0000-0000-0000-000000000002",
    );
    home.capture(probe_search(&second["probe"], json!(second_counted)));
    let after = reserve(&[]);
    assert_eq!(after["state"], "complete", "{after}");
    assert_eq!(home.tally()["probe-answered"], 1);
}

const INVALID: &str = "#type=synthetic | join(query={#type=other | head(1)}, key=user_id)";
const REJECT_HEADING: &str = "CQLmatic check expects the search platform to reject this query, but did not block the search. Wait for the tool result. If the search failed, fix the errors below.";
const NOTE_HEADING: &str = "CQLmatic check found the issues below and did not block this search. Wait for the tool result.";

/// A search as Claude Code or Codex reports it before the call, to the search tool directly or
/// through the hosted server's gateway tool.
fn pre_search(client: &str, gateway: bool, repository: &str, query: &str) -> Value {
    let server = if client == "codex" {
        SERVER.replace('-', "_")
    } else {
        SERVER.into()
    };
    let arguments = json!({
        "repository": repository, "query_string": query, "start": hours_ago(2), "end": hours_ago(1),
    });
    let (tool, input) = if gateway {
        (
            "execute_tool",
            json!({"name": "falcon_search_ngsiem", "arguments": arguments}),
        )
    } else {
        ("falcon_search_ngsiem", arguments)
    };
    let mut payload = json!({
        "session_id": "00000000-0000-0000-0000-000000000000", "transcript_path": null,
        "cwd": "/", "permission_mode": "default", "hook_event_name": "PreToolUse",
        "tool_name": format!("mcp__{server}__{tool}"), "tool_input": input,
        "tool_use_id": "synthetic-call",
    });
    if client == "codex" {
        payload["turn_id"] = json!("synthetic-turn");
        payload["model"] = json!("synthetic-model");
    } else {
        payload["mcp_server"] = json!({"name": SERVER, "source": "dynamic"});
    }
    payload
}

impl Temp {
    /// Runs the hook command on one payload and returns its answer, or `None` when it printed
    /// nothing. Whatever happens, it exits 0 and writes nothing to stderr.
    fn guard(&self, client: &str, payload: &[u8]) -> Option<Value> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cqlmatic"));
        let mut child = isolated(&mut command, &self.0)
            .args([
                "env", "capture", "--client", client, "--server", SERVER, "--state",
            ])
            .arg(self.state())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // The hook may stop reading a payload it will not use.
        let _ = child.stdin.take().unwrap().write_all(payload);
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        assert!(output.stdout.is_ascii(), "{output:?}");
        (!output.stdout.is_empty()).then(|| serde_json::from_slice(&output.stdout).unwrap())
    }
    fn ask(&self, client: &str, gateway: bool, repository: &str, query: &str) -> Option<Value> {
        let payload = pre_search(client, gateway, repository, query);
        self.guard(client, payload.to_string().as_bytes())
    }
}

/// The context a note adds, after checking that it makes no permission decision.
fn note(answer: Option<Value>) -> String {
    let answer = answer.expect("a note");
    let output = answer["hookSpecificOutput"].as_object().unwrap();
    assert_eq!(
        output.keys().collect::<Vec<_>>(),
        ["additionalContext", "hookEventName"],
        "{answer}"
    );
    assert_eq!(output["hookEventName"], "PreToolUse");
    output["additionalContext"].as_str().unwrap().to_owned()
}

#[test]
fn guard_answers_each_client_and_call_shape_before_the_search() {
    let dir = Temp::new();
    for client in ["claude-code", "codex"] {
        for gateway in [false, true] {
            assert_eq!(
                note(dir.ask(client, gateway, "third-party", INVALID)),
                format!(
                    "{REJECT_HEADING}\n- CQL_ARGUMENT_REQUIRED at line 1, column 19: 'join' requires argument 'field'."
                )
            );
            assert_eq!(
                note(dir.ask(
                    client,
                    gateway,
                    "third-party",
                    "#type=synthetic | groupBy([alpha_field], limit=max) | sort(_count) | head(3)"
                )),
                format!(
                    "{NOTE_HEADING}\n- CQL_SORT_HEAD at line 1, column 70: head after sort can select a different set of rows from the first N in the sorted order. Use sort(..., limit=N) to select the first N rows in that order."
                )
            );
            assert_eq!(
                note(dir.ask(client, gateway, "third-party", "#type=synthetic | limit 20")),
                format!(
                    "{NOTE_HEADING}\n- CQL_BARE_COMMAND_WORD: limit is not a query-language command here and is searched as text. Use head() or tail() to limit rows, or quote it to search for the text."
                )
            );
            for informational_only in [
                "#type=synthetic | head(5)",
                "#type=synthetic | groupBy([alpha_field])",
            ] {
                assert_eq!(
                    dir.ask(client, gateway, "third-party", informational_only),
                    None,
                    "{informational_only}"
                );
            }
        }
    }
    assert!(!dir.0.join("workflow").exists());
}

#[test]
fn guard_never_blocks_and_lists_errors_first() {
    let dir = Temp::new();
    // Check expects the platform to reject these queries; each still runs, with check's errors.
    for (code, query) in [
        ("CQL_ARGUMENT_RANGE", "#type=synthetic | head(limit=0)"),
        ("CQL_ARGUMENT_REQUIRED", INVALID),
        ("CQL_ARGUMENT_TYPE", "#type=synthetic | head(limit=abc)"),
        ("CQL_ARGUMENT_UNKNOWN", "#type=synthetic | head(bogus=1)"),
        ("CQL_ASSIGN_ALIAS", "#type=synthetic | x := count(as=y)"),
        (
            "CQL_DUPLICATE_ARGUMENT",
            "#type=synthetic | head(limit=1, limit=2)",
        ),
        (
            "CQL_FUNCTION_FILTER_COMPOSITION",
            "#type=synthetic | user=a or regex(\"a\")",
        ),
        ("CQL_QUOTE", "#type=synthetic | user=\u{2018}abc\u{2019}"),
        ("CQL_RESERVED_WORD", "#type=synthetic | AND"),
        ("CQL_SYNTAX", "#type=synthetic | head(5"),
    ] {
        for client in ["claude-code", "codex"] {
            let context = note(dir.ask(client, true, "third-party", query));
            assert!(
                context.starts_with(&format!("{REJECT_HEADING}\n- {code}")),
                "{context}"
            );
        }
    }
    let context = note(dir.ask(
        "claude-code",
        false,
        "third-party",
        "#type=synthetic | x := \"v\" | series(x, memlimit=\"1kb\") | count()",
    ));
    let lines: Vec<_> = context.lines().collect();
    assert_eq!(lines[0], REJECT_HEADING, "{context}");
    assert!(lines[1].starts_with("- CQL_ARGUMENT_RANGE "), "{context}");
    assert!(lines[2].starts_with("- CQL_TEMPORAL_STREAM "), "{context}");
    let context = note(dir.ask(
        "codex",
        true,
        "third-party",
        "#type=synthetic | count() = 5",
    ));
    assert!(
        context.starts_with(&format!("{NOTE_HEADING}\n- CQL_UNSUPPORTED_SYNTAX ")),
        "{context}"
    );
    // Queries the platform runs pass silently or with a note.
    for (query, code) in [
        ("#type=synthetic | xml:prettyPrint()", None),
        ("#type=synthetic | time:dayOfWeek()", None),
        (
            "#type=synthetic | default(field=[\"#event.dataset\", alpha_field, beta_field], value=\"<missing>\") | groupBy([alpha_field, beta_field])",
            None,
        ),
        (
            "#type=synthetic | groupBy([alpha_field], limit=max) | sort(_count) | head(3)",
            Some("CQL_SORT_HEAD"),
        ),
    ] {
        let answer = dir.ask("claude-code", false, "third-party", query);
        match code {
            None => assert_eq!(answer, None, "{query}"),
            Some(code) => {
                let context = note(answer);
                assert!(
                    context.starts_with(&format!("{NOTE_HEADING}\n- {code} ")),
                    "{context}"
                );
            }
        }
    }
}

#[test]
fn guard_lets_what_it_cannot_judge_pass_silently() {
    let dir = Temp::new();
    let edited = |change: fn(&mut Value)| {
        let mut payload = pre_search("claude-code", true, "third-party", INVALID);
        change(&mut payload);
        payload.to_string().into_bytes()
    };
    let mut oversized = pre_search("claude-code", true, "third-party", INVALID);
    oversized["padding"] = json!("x".repeat(17 * 1024 * 1024));
    for payload in [
        b"invalid JSON".to_vec(),
        Vec::new(),
        oversized.to_string().into_bytes(),
        // Another server, another tool, and a gateway call to another tool.
        edited(|p| p["tool_name"] = json!("mcp__other-server__execute_tool")),
        edited(|p| p["tool_name"] = json!(format!("mcp__{SERVER}__list_repositories"))),
        edited(|p| p["tool_input"]["name"] = json!("list_repositories")),
        // A search without a query, or with a query that is not text.
        edited(|p| {
            p["tool_input"]["arguments"]
                .as_object_mut()
                .unwrap()
                .remove("query_string");
        }),
        edited(|p| p["tool_input"]["arguments"]["query_string"] = json!(5)),
    ] {
        assert_eq!(dir.guard("claude-code", &payload), None);
    }
    let payload = pre_search("claude-code", true, "third-party", INVALID).to_string();
    assert_eq!(dir.guard("qwen-code", payload.as_bytes()), None);
}

/// Gives the confirmed target a map with one source, `#type=synthetic`, that lists `user`.
fn map_at(state: &Path) {
    let environment: EnvironmentId = serde_json::from_value(environment()).unwrap();
    let source_id = cqlmatic::discovery::source_id(&Scope {
        type_: Some("synthetic".into()),
        ..Scope::default()
    });
    let pack = Pack::new(
        environment,
        "2026-01-01T00:00:00Z".into(),
        PackData {
            sources: vec![
                json!({"id": source_id, "kind": "source", "name": "synthetic",
                "inventory_status": "observed-only", "selectors": {"type": "synthetic"},
                "evidence": {"observed": true}, "provenance_ids": ["prov:synthetic"]}),
            ],
            fields: vec![
                json!({"id": "env:field:synthetic:user", "source_id": source_id,
                "path": "user", "data_type": "string", "is_tag": false,
                "evidence_kind": "observed", "coverage": {"observed": true},
                "provenance_ids": ["prov:synthetic"]}),
            ],
            provenance: vec![json!({"id": "prov:synthetic", "source_type": "ngsiem-api",
                "canonical_source": "Synthetic measurement",
                "retrieved_at": "2026-01-01T00:00:00Z", "redaction_status": "none",
                "coverage_status": "complete"})],
            ..PackData::default()
        },
    )
    .unwrap();
    let path = state.with_file_name("pack.json");
    Writer::acquire(&path).unwrap().commit(&pack).unwrap();
    let mut inventory: InventoryState = serde_json::from_slice(&fs::read(state).unwrap()).unwrap();
    let active = PackReference {
        pruned_at: None,
        cleanup_pending: false,
        path: path.to_string_lossy().into_owned(),
        content_digest: pack.content_digest.clone(),
        input_content_digest: None,
        imported_at: "2026-01-02T00:00:00Z".into(),
        discovery_run: None,
    };
    inventory.packs.push(active.clone());
    inventory.active_pack = Some(active);
    inventory.validate().unwrap();
    Writer::acquire(state)
        .unwrap()
        .commit_bytes(&serde_json::to_vec(&inventory).unwrap(), MAX_STATE_BYTES)
        .unwrap();
}

/// Every path under a folder with its bytes, so a test can tell that nothing was written.
fn snapshot(directory: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path.clone());
                found.push((path, Vec::new()));
            } else {
                found.push((path.clone(), fs::read(path).unwrap()));
            }
        }
    }
    found.sort();
    found
}

#[test]
fn guard_uses_the_map_only_for_its_own_view_and_writes_nothing() {
    let dir = Temp::new();
    dir.confirm();
    map_at(&dir.state());
    let before = snapshot(&dir.0);
    let unmapped = "#type=unmapped_type | count()";
    let context = note(dir.ask("claude-code", false, "third-party", unmapped));
    assert!(context.contains("- CQL_UNMAPPED_SOURCE: "), "{context}");
    // The map belongs to the view third-party; a search of another view, or of search-all when
    // the call names no repository, is checked without it.
    assert_eq!(dir.ask("codex", false, "other-view", unmapped), None);
    let mut unnamed = pre_search("codex", false, "", unmapped);
    unnamed["tool_input"]
        .as_object_mut()
        .unwrap()
        .remove("repository");
    assert_eq!(dir.guard("codex", unnamed.to_string().as_bytes()), None);
    assert!(note(dir.ask("claude-code", false, "third-party", INVALID)).contains("'field'"));
    assert_eq!(snapshot(&dir.0), before);

    // An inventory the guard cannot read leaves the check of the language.
    fs::write(dir.state(), b"{").unwrap();
    assert!(note(dir.ask("claude-code", false, "third-party", INVALID)).contains("'field'"));
    assert_eq!(dir.ask("claude-code", false, "third-party", unmapped), None);
}

#[test]
fn guard_answers_in_ascii_within_the_client_limits() {
    let dir = Temp::new();
    // The note quotes a curly quote, which goes out escaped.
    let context = note(dir.ask(
        "claude-code",
        false,
        "third-party",
        "#type=synthetic | user=\u{2018}abc\u{2019}",
    ));
    assert!(
        context.contains("'\u{2018}' is not a string delimiter"),
        "{context}"
    );

    // Past 200 lines or 8,000 characters the guard says how many lines it left out. Warnings
    // are left out first, so the error at the end of the query is still shown.
    let steps = (0..100)
        .map(|n| format!("groupBy([alpha_{n}])"))
        .collect::<Vec<_>>()
        .join(" | ");
    let context = note(dir.ask(
        "codex",
        false,
        "third-party",
        &format!("#type=synthetic | {steps} | head(limit=0)"),
    ));
    let lines: Vec<_> = context.lines().collect();
    assert!(lines.len() <= 200, "{context}");
    assert!(context.encode_utf16().count() <= 8_000, "{context}");
    assert_eq!(lines[0], REJECT_HEADING);
    assert!(lines[1].starts_with("- CQL_ARGUMENT_RANGE "), "{context}");
    let shown = lines.len() - 2;
    assert!(
        lines[2..=shown]
            .iter()
            .all(|line| line.starts_with("- CQL_DROPPED_FIELD ")),
        "{context}"
    );
    let left_out = lines[shown + 1]
        .strip_prefix("- and ")
        .and_then(|rest| rest.strip_suffix(" more. Run cqlmatic check on the query to see all."))
        .unwrap();
    assert!(left_out.parse::<usize>().unwrap() > 0, "{context}");
}

fn settings_file(home: &Path, client: &str) -> PathBuf {
    match client {
        "claude-code" => home.join(".claude/settings.json"),
        _ => home.join(".codex/hooks.json"),
    }
}

#[test]
fn guard_install_registers_both_events_with_one_fail_open_command() {
    for client in ["claude-code", "codex"] {
        let home = Temp::new();
        let output = report(&mut install(&home.0, client));
        assert_eq!(output["events"], json!(["PostToolUse", "PreToolUse"]));
        let settings: Value =
            serde_json::from_slice(&fs::read(settings_file(&home.0, client)).unwrap()).unwrap();
        let command = output["command"].as_str().unwrap();
        let [capture, guard] = ["PostToolUse", "PreToolUse"].map(|event| {
            let entries = settings["hooks"][event].as_array().unwrap();
            assert_eq!(entries.len(), 1, "{settings}");
            entries[0].clone()
        });
        assert_eq!(capture["hooks"][0]["command"], command);
        assert_eq!(guard["matcher"], capture["matcher"]);
        // Before the call the client waits for the answer, up to the timeout, and a guard that
        // fails lets the call run.
        let mut handler = json!({"type": "command", "command": command, "timeout": 5});
        if client == "claude-code" {
            handler["onFailure"] = json!("continue");
        }
        if cfg!(windows) && client == "claude-code" {
            handler["shell"] = json!("powershell");
        }
        assert_eq!(guard["hooks"], json!([handler]));
        // The command passes only arguments that earlier releases accept, so an older copy at
        // the same path runs capture and prints nothing.
        let flags: Vec<_> = command
            .split_whitespace()
            .filter(|word| word.starts_with("--"))
            .collect();
        assert_eq!(flags, ["--client", "--server", "--state"], "{command}");
    }
}

#[test]
fn guard_install_switches_each_hook_and_keeps_every_other_byte() {
    for client in ["claude-code", "codex"] {
        let home = Temp::new();
        let file = settings_file(&home.0, client);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let original = "{\n  \"hooks\": {\n    \"PreToolUse\": [\n      {\"matcher\": \"Bash\", \"hooks\": [{\"type\": \"command\", \"command\": \"echo before\"}]}\n    ],\n    \"PostToolUse\": [\n      {\"matcher\": \"Bash\", \"hooks\": [{\"type\": \"command\", \"command\": \"echo after\"}]}\n    ]\n  },\n  \"theme\": \"synthetic\"\n}\n";
        fs::write(&file, original).unwrap();
        let run = |hooks: &str, dry_run: bool| {
            let mut command = install(&home.0, client);
            if !hooks.is_empty() {
                command.args(["--hooks", hooks]);
            }
            if dry_run {
                command.arg("--dry-run");
            }
            report(&mut command)
        };
        let preview = run("", true);
        assert_eq!(
            preview["hooks"],
            json!({"capture": "added", "guard": "added"})
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), original);

        let both = run("", false);
        assert_eq!(both["hooks"], preview["hooks"]);
        assert_eq!(
            (&both["installed"], &both["changed"]),
            (&json!(true), &json!(true))
        );
        let installed = fs::read_to_string(&file).unwrap();
        let mut expected: Value = serde_json::from_str(original).unwrap();
        for event in ["PostToolUse", "PreToolUse"] {
            expected["hooks"][event]
                .as_array_mut()
                .unwrap()
                .push(both["entry"]["hooks"][event][0].clone());
        }
        assert_eq!(serde_json::from_str::<Value>(&installed).unwrap(), expected);
        let repeat = run("capture,guard", false);
        assert_eq!(
            repeat["hooks"],
            json!({"capture": "present", "guard": "present"})
        );
        assert_eq!(repeat["changed"], false);
        assert_eq!(fs::read_to_string(&file).unwrap(), installed);

        let capture = run("capture", false);
        assert_eq!(
            capture["hooks"],
            json!({"capture": "present", "guard": "removed"})
        );
        assert_eq!(capture["events"], json!(["PostToolUse"]));
        expected["hooks"]["PreToolUse"]
            .as_array_mut()
            .unwrap()
            .pop();
        let switched = fs::read_to_string(&file).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&switched).unwrap(), expected);
        let none = run("none", false);
        assert_eq!(
            none["hooks"],
            json!({"capture": "removed", "guard": "absent"})
        );
        assert_eq!(none["events"], json!([]));
        assert_eq!(fs::read_to_string(&file).unwrap(), original);
        let again = run("none", false);
        assert_eq!(
            again["hooks"],
            json!({"capture": "absent", "guard": "absent"})
        );
        assert_eq!(
            (&again["installed"], &again["changed"]),
            (&json!(true), &json!(false))
        );
        let guard = run("guard", false);
        assert_eq!(
            guard["hooks"],
            json!({"capture": "absent", "guard": "added"})
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(&file).unwrap()).unwrap()["hooks"]
                ["PostToolUse"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}

/// The capture entry 0.1.10 wrote for `launcher` and the state, on this platform.
fn earlier_capture(client: &str, state: &Path) -> Value {
    let state = state.to_str().unwrap();
    let mut handler = json!({"type": "command", "async": true});
    handler["command"] = json!(match (client, cfg!(windows)) {
        ("codex", true) => format!(
            "launcher env capture --client codex --server \"synthetic_server\" --state \"{state}\""
        ),
        ("codex", false) => format!(
            "launcher env capture --client codex --server 'synthetic_server' --state '{state}'"
        ),
        _ => format!(
            "launcher env capture --client claude-code --server '{SERVER}' --state '{state}'"
        ),
    });
    if cfg!(windows) && client == "claude-code" {
        handler["shell"] = json!("powershell");
    }
    let server = if client == "codex" {
        SERVER.replace('-', "_")
    } else {
        SERVER.into()
    };
    json!({"matcher": format!("mcp__{server}__.*"), "hooks": [handler]})
}

#[test]
fn guard_install_replaces_the_earlier_capture_entry_and_reports_entries_it_did_not_write() {
    for client in ["claude-code", "codex"] {
        let home = Temp::new();
        let file = settings_file(&home.0, client);
        let state = home.0.join("state.json");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let install = || {
            let mut command = install(&home.0, client);
            command
                .args(["--program", "launcher", "--state"])
                .arg(&state);
            command
        };
        let earlier = earlier_capture(client, &state);
        fs::write(
            &file,
            json!({"hooks": {"PostToolUse": [&earlier]}}).to_string(),
        )
        .unwrap();
        let upgraded = report(&mut install());
        assert_eq!(
            upgraded["hooks"],
            json!({"capture": "added", "guard": "added"})
        );
        let settings: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(settings, upgraded["entry"], "{client}");
        assert!(
            !upgraded["next_actions"]
                .to_string()
                .contains("different hook entry")
        );

        // An entry someone changed, and one an older copy elsewhere wrote, are reported with
        // their commands and left in place.
        let mut changed = upgraded["entry"]["hooks"]["PreToolUse"][0].clone();
        changed["note"] = json!("kept by its owner");
        let older = upgraded["command"]
            .as_str()
            .unwrap()
            .replace("launcher", "/older/copy/cqlmatic");
        let mut copy = changed.clone();
        copy.as_object_mut().unwrap().remove("note");
        copy["hooks"][0]["command"] = json!(older);
        let written = json!({"hooks": {
            "PostToolUse": upgraded["entry"]["hooks"]["PostToolUse"],
            "PreToolUse": [&changed, &copy],
        }})
        .to_string();
        fs::write(&file, &written).unwrap();
        let mut command = install();
        let kept = report(command.args(["--hooks", "capture"]));
        assert_eq!(
            kept["hooks"],
            json!({"capture": "present", "guard": "manual"})
        );
        assert_eq!(kept["installed"], false);
        assert_eq!(kept["changed"], false);
        let actions = kept["next_actions"].to_string();
        assert!(
            actions.contains("hooks.PreToolUse holds a different hook entry"),
            "{actions}"
        );
        assert!(actions.contains("/older/copy/cqlmatic"), "{actions}");
        // Each names the settings file as the report spells it on this platform.
        let path = kept["path"].as_str().unwrap();
        let different: Vec<_> = kept["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .filter(|action| action.contains("different hook entry"))
            .collect();
        assert_eq!(different.len(), 2, "{actions}");
        assert!(
            different
                .iter()
                .all(|action| action.ends_with(&format!(" ({path})."))),
            "{actions}"
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), written);
    }
}

/// An installation that registers `program` for `server` and `state`.
fn install_copy(home: &Path, client: &str, server: &str, program: &str, state: &Path) -> Command {
    let mut command = install_for(home, client, server);
    command.args(["--program", program, "--state"]).arg(state);
    command
}

#[test]
fn guard_install_finds_another_copy_by_its_complete_server_and_state() {
    for client in ["claude-code", "codex"] {
        let home = Temp::new();
        let file = settings_file(&home.0, client);
        // An apostrophe in the state path is written escaped in the command.
        let state = home.0.join("O'Brien").join("state.json");
        let first = report(&mut install_copy(
            &home.0,
            client,
            SERVER,
            "/first/copy/cqlmatic",
            &state,
        ));
        assert_eq!(
            first["hooks"],
            json!({"capture": "added", "guard": "added"})
        );
        let written = fs::read_to_string(&file).unwrap();
        let mut second = install_copy(&home.0, client, SERVER, "/second/copy/cqlmatic", &state);
        let none = report(second.args(["--hooks", "none"]));
        assert_eq!(
            none["hooks"],
            json!({"capture": "manual", "guard": "manual"}),
            "{none}"
        );
        assert_eq!(none["installed"], false);
        let actions = none["next_actions"].to_string();
        assert_eq!(
            actions.matches("holds a different hook entry").count(),
            2,
            "{actions}"
        );
        assert!(actions.contains("/first/copy/cqlmatic"), "{actions}");
        assert_eq!(fs::read_to_string(&file).unwrap(), written);

        // A server whose name starts with another's is a different server.
        let home = Temp::new();
        report(&mut install_copy(
            &home.0,
            client,
            "falcon-dev",
            "/first/copy/cqlmatic",
            &state,
        ));
        let written = fs::read_to_string(settings_file(&home.0, client)).unwrap();
        let mut other = install_copy(&home.0, client, "falcon", "/second/copy/cqlmatic", &state);
        let none = report(other.args(["--hooks", "none"]));
        assert_eq!(
            none["hooks"],
            json!({"capture": "absent", "guard": "absent"}),
            "{none}"
        );
        assert!(
            !none["next_actions"]
                .to_string()
                .contains("different hook entry"),
            "{none}"
        );
        assert_eq!(
            fs::read_to_string(settings_file(&home.0, client)).unwrap(),
            written
        );
    }
}

#[test]
fn guard_install_keeps_one_of_several_identical_entries() {
    for client in ["claude-code", "codex"] {
        let home = Temp::new();
        let file = settings_file(&home.0, client);
        let installed = report(&mut install(&home.0, client));
        let entry = &installed["entry"]["hooks"];
        let doubled = json!({"hooks": {
            "PostToolUse": [&entry["PostToolUse"][0], &entry["PostToolUse"][0]],
            "PreToolUse": [&entry["PreToolUse"][0], &entry["PreToolUse"][0], &entry["PreToolUse"][0]],
        }});
        fs::write(&file, serde_json::to_vec_pretty(&doubled).unwrap()).unwrap();
        let again = report(&mut install(&home.0, client));
        assert_eq!(
            again["hooks"],
            json!({"capture": "present", "guard": "present"})
        );
        assert_eq!(again["changed"], true);
        let settings: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(settings, installed["entry"], "{client}");
    }
}

#[test]
fn guard_install_refuses_hook_lists_a_client_cannot_take() {
    let home = Temp::new();
    for (client, hooks, message) in [
        (
            "copilot-cli",
            "guard",
            "Only claude-code and codex have the guard hook.",
        ),
        (
            "codex",
            "none,capture",
            "--hooks none cannot be combined with another hook.",
        ),
        (
            "cursor",
            "none",
            "This client's hook is added by hand, so --hooks must include capture.",
        ),
    ] {
        let output = install(&home.0, client)
            .args(["--hooks", hooks])
            .output()
            .unwrap();
        let refused = install_error(&output);
        assert_eq!(refused["code"], "CLI_INPUT");
        assert_eq!(refused["message"], message);
        assert!(output.stdout.is_empty());
    }
    assert_eq!(fs::read_dir(&home.0).unwrap().count(), 0);
    report(&mut install(&home.0, "copilot-cli"));
    let removed = report(install(&home.0, "copilot-cli").args(["--hooks", "none"]));
    assert_eq!(removed["hooks"], json!({"capture": "removed"}));
    let settings: Value = serde_json::from_slice(
        &fs::read(home.0.join(".copilot/hooks/cqlmatic-capture.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        settings,
        json!({"version": 1, "hooks": {"postToolUse": []}})
    );
}

/// Runs a registered command the way `client` starts it, with `payload` on stdin.
fn launch(client: &str, command: &str, payload: &[u8], home: &Path) -> Vec<Output> {
    #[cfg(unix)]
    let shells: Vec<_> = {
        let _ = client;
        // Claude Code runs a command with the user's shell, Codex with the login shell.
        let mut forms = vec![("/bin/sh", "-c"), ("/bin/bash", "-c"), ("/bin/bash", "-lc")];
        if Path::new("/bin/zsh").exists() {
            forms.extend([("/bin/zsh", "-c"), ("/bin/zsh", "-lc")]);
        }
        forms
            .into_iter()
            .map(|(shell, flag)| {
                let mut shell = Command::new(shell);
                shell
                    .args([flag, command])
                    .env_remove("ZDOTDIR")
                    .env_remove("BASH_ENV")
                    .env_remove("ENV");
                shell
            })
            .collect()
    };
    #[cfg(windows)]
    let shells = vec![if client == "codex" {
        use std::os::windows::process::CommandExt;
        let mut shell = Command::new(std::env::var_os("COMSPEC").unwrap());
        shell.arg("/C").raw_arg(format!("\"{command}\""));
        shell
    } else {
        let mut shell = powershell();
        shell
            .arg("& ([ScriptBlock]::Create($env:CQLMATIC_TEST_HOOK))")
            .env("CQLMATIC_TEST_HOOK", command);
        shell
    }];
    shells
        .into_iter()
        .map(|mut shell| {
            let mut child = isolated(&mut shell, home)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let _ = child.stdin.take().unwrap().write_all(payload);
            child.wait_with_output().unwrap()
        })
        .collect()
}

#[test]
fn installed_guard_runs_the_way_each_client_starts_it_and_fails_open() {
    let dir = Temp::new();
    // Startup files that make a shell exit as soon as a command fails.
    fs::write(dir.0.join(".zshenv"), "setopt ERR_EXIT\n").unwrap();
    fs::write(dir.0.join(".bash_profile"), "set -e\n").unwrap();
    let directory = dir.0.join("guard folder's $data; (x)");
    fs::create_dir(&directory).unwrap();
    let state = directory.join("state.json");
    let executable = runner_in(&directory);
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(fs::metadata(&executable).unwrap().nlink(), 1);
    }
    for client in ["claude-code", "codex"] {
        let mut installer = Command::new(&executable);
        isolated(&mut installer, &dir.0)
            .args(["env", "capture", "--client", client, "--server", SERVER])
            .args(["--install", "--hooks", "guard", "--state"])
            .arg(&state);
        let installed = report(&mut installer);
        let settings: Value =
            serde_json::from_slice(&fs::read(settings_file(&dir.0, client)).unwrap()).unwrap();
        let command = settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(installed["command"], command);
        let ask = |command: &str, query: &str| {
            let payload = pre_search(client, true, "third-party", query).to_string();
            launch(client, command, payload.as_bytes(), &dir.0)
        };
        for output in ask(command, INVALID) {
            assert!(
                output.status.success() && output.stderr.is_empty(),
                "{output:?}"
            );
            let answer: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(note(Some(answer)).contains("CQL_ARGUMENT_REQUIRED"));
        }
        for output in ask(command, "#type=synthetic | head(5)") {
            assert!(
                output.status.success() && output.stdout.is_empty() && output.stderr.is_empty(),
                "{output:?}"
            );
        }
        // A missing program, a program that rejects the arguments, a prefix the shell cannot
        // parse, and a prefix that prints a line before its program fails to start all end
        // with exit status 0 and no output, so the search runs.
        let path = executable.to_str().unwrap();
        let quoted = if cfg!(windows) && client == "codex" {
            format!("\"{path}\"")
        } else if cfg!(windows) {
            format!("& '{}'", path.replace('\'', "''"))
        } else {
            format!("'{}'", path.replace('\'', r"'\''"))
        };
        let missing = quoted.replace("runner", "missing runner");
        let mut programs = vec![
            missing.clone(),
            format!("{quoted} --no-such-option"),
            format!("{quoted}'"),
        ];
        if cfg!(unix) {
            programs.push(format!(
                "/bin/sh -c 'echo \"> synthetic@1.0.0 guard\"; exec \"$0\" \"$@\"' {missing}"
            ));
        }
        for program in programs {
            let mut installer = Command::new(&executable);
            isolated(&mut installer, &dir.0)
                .args(["env", "capture", "--client", client, "--server", SERVER])
                .args(["--install", "--dry-run", "--program", &program, "--state"])
                .arg(&state);
            let preview = report(&mut installer);
            for output in ask(preview["command"].as_str().unwrap(), INVALID) {
                assert!(
                    output.status.success() && output.stdout.is_empty() && output.stderr.is_empty(),
                    "{program}: {output:?}"
                );
            }
        }
    }
}
