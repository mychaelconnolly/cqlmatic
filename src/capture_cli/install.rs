//! Writes the user-level hook for clients with a supported settings file.
use super::{CaptureArgs, Client};
use clap::ValueEnum;
use cqlmatic::{contracts::Diagnostic, mcp};
use serde_json::{Value, json, value::RawValue};
use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const MAX_SETTINGS_BYTES: u64 = 8 * 1024 * 1024;
/// Age after which a leftover candidate file is taken to be from an interrupted installation.
const ABANDONED_CANDIDATE: std::time::Duration = std::time::Duration::from_secs(60);
const POST_TOOL_USE: &str = "PostToolUse";
const COPILOT_EVENT: &str = "postToolUse";

pub(super) fn run(args: &CaptureArgs) -> Result<i32, Diagnostic> {
    let client = args.client;
    let name = client
        .to_possible_value()
        .expect("A capture client has a name");
    let mut next_actions = Vec::new();
    let (path, event, command, entry, installed, changed) = if let Some(path) = config_path(client)?
    {
        let copilot = is_copilot(client);
        let (event, command, hook) = if copilot {
            let bash = command(args, name.get_name(), Shell::Bash)?;
            let powershell = command(args, name.get_name(), Shell::PowerShell)?;
            (
                COPILOT_EVENT,
                json!({"bash": bash, "powershell": powershell}),
                json!({"type": "command", "bash": bash, "powershell": powershell}),
            )
        } else {
            let command = command(args, name.get_name(), Shell::for_client(client))?;
            (
                POST_TOOL_USE,
                json!(command),
                matcher_hook(&matcher("mcp__", &args.server, "__.*"), &command, true),
            )
        };
        let mut entry = json!({"hooks": {event: [&hook]}});
        if copilot {
            entry["version"] = json!(1);
        }

        let destination = path.to_str().ok_or_else(|| {
            file_error(
                "LOCAL_PATH",
                &path,
                "Cannot report a non-Unicode settings path",
            )
        })?;
        let settings = read_settings(&path)?;
        let (installed, changed) = if matches!(settings, Settings::Link) {
            let kind = if cfg!(windows) {
                "a symbolic link or reparse point"
            } else {
                "a symbolic link"
            };
            next_actions.push(format!(
                "{destination} is {kind}. Merge the reported entry into the linked settings file."
            ));
            (false, false)
        } else {
            let bytes = match &settings {
                Settings::Missing => {
                    let mut bytes = serde_json::to_vec_pretty(&entry).expect("JSON serializes");
                    bytes.push(b'\n');
                    Some(bytes)
                }
                Settings::File { bytes, .. } => merge(&path, bytes, event, &hook)?,
                Settings::Link => unreachable!(),
            };
            if let Some(bytes) = bytes {
                if bytes.len() as u64 > MAX_SETTINGS_BYTES {
                    return Err(file_error(
                        "LOCAL_SIZE",
                        &path,
                        "Updated settings exceed the byte limit",
                    ));
                }
                if !args.dry_run {
                    replace(&path, &settings, &bytes)?;
                }
                (!args.dry_run, true)
            } else {
                (true, false)
            }
        };
        if client == Client::Codex {
            next_actions.push("Review and trust the hook with /hooks before it runs.".into());
        }
        (
            Some(destination.to_owned()),
            Some(event),
            command,
            entry,
            installed,
            changed,
        )
    } else {
        let command = command(args, name.get_name(), Shell::for_client(client))?;
        let (path, event, entry) = manual_entry(client, &args.server, &command);
        next_actions.push(match client {
            Client::Cline => "Add the reported script as an executable PostToolUse file. The editor extension uses ~/Documents/Cline/Hooks/PostToolUse.".into(),
            Client::Generic => "Run the command after a tool call and pass a JSON object with tool_name, tool_input and tool_response on stdin.".into(),
            _ => format!("Merge the reported entry into {}.", path.expect("A documented settings path")),
        });
        (
            path.map(str::to_owned),
            event,
            json!(command),
            entry,
            false,
            false,
        )
    };
    crate::line(
        &json!({
            "path": path, "event": event, "command": command, "entry": entry,
            "installed": installed, "changed": changed, "dry_run": args.dry_run,
            "next_actions": next_actions,
        })
        .to_string(),
    )?;
    Ok(0)
}

fn is_copilot(client: Client) -> bool {
    matches!(
        client,
        Client::CopilotCli | Client::CopilotApp | Client::CopilotVscode
    )
}

fn config_path(client: Client) -> Result<Option<PathBuf>, Diagnostic> {
    let (variable, directory, file) = match client {
        Client::ClaudeCode => ("CLAUDE_CONFIG_DIR", ".claude", "settings.json"),
        Client::Codex => ("CODEX_HOME", ".codex", "hooks.json"),
        client if is_copilot(client) => ("COPILOT_HOME", ".copilot", "hooks/cqlmatic-capture.json"),
        _ => return Ok(None),
    };
    let root = match env::var_os(variable).filter(|value| !value.is_empty()) {
        Some(root) => PathBuf::from(root),
        None => env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .filter(|value| !value.is_empty())
            .map(|home| PathBuf::from(home).join(directory))
            .ok_or_else(|| {
                Diagnostic::error(
                    "LOCAL_PATH",
                    format!("Home directory is unavailable. Set {variable}."),
                )
            })?,
    };
    let path = root.join(file);
    std::path::absolute(&path)
        .map(Some)
        .map_err(|_| file_error("LOCAL_PATH", &path, "Cannot resolve settings path"))
}

fn manual_entry(
    client: Client,
    server: &str,
    command: &str,
) -> (Option<&'static str>, Option<&'static str>, Value) {
    match client {
        Client::Cline => (
            Some("~/.cline/hooks/PostToolUse"),
            Some(POST_TOOL_USE),
            json!(format!("#!/bin/sh\n{command}\necho '{{}}'\n")),
        ),
        Client::Cursor => (
            Some("~/.cursor/hooks.json"),
            Some("afterMCPExecution"),
            json!({"version": 1, "hooks": {"afterMCPExecution": [{"command": command}]}}),
        ),
        Client::GeminiCli => (
            Some("~/.gemini/settings.json"),
            Some("AfterTool"),
            json!({"hooks": {"AfterTool": [matcher_hook(&matcher("mcp_", server, "_.*"), command, false)]}}),
        ),
        Client::Kiro => (
            Some("~/.kiro/hooks/cqlmatic-capture.json"),
            Some(POST_TOOL_USE),
            json!({"version": "v1", "hooks": [{
                "name": "cqlmatic-capture", "trigger": POST_TOOL_USE, "matcher": format!("@{server}"),
                "action": {"type": "command", "command": command}
            }]}),
        ),
        Client::QwenCode => (
            Some("~/.qwen/settings.json"),
            Some(POST_TOOL_USE),
            json!({"hooks": {POST_TOOL_USE: [matcher_hook(&matcher("mcp__", server, "__.*"), command, true)]}}),
        ),
        Client::Generic => (None, None, Value::Null),
        _ => unreachable!("Only manual clients need a manual entry"),
    }
}

fn matcher(prefix: &str, server: &str, suffix: &str) -> String {
    let mut matcher = String::from(prefix);
    for c in server.chars() {
        if ".^$*+?()[]{}|\\".contains(c) {
            matcher.push('\\');
        }
        matcher.push(c);
    }
    matcher.push_str(suffix);
    matcher
}

fn matcher_hook(matcher: &str, command: &str, asynchronous: bool) -> Value {
    let mut hook = json!({"type": "command", "command": command});
    if asynchronous {
        hook["async"] = json!(true);
    }
    json!({"matcher": matcher, "hooks": [hook]})
}

#[derive(Clone, Copy)]
enum Shell {
    Bash,
    PowerShell,
}

impl Shell {
    fn for_client(client: Client) -> Self {
        if client == Client::Cline {
            return Self::Bash;
        }
        #[cfg(windows)]
        if client != Client::ClaudeCode || !claude_uses_bash() {
            return Self::PowerShell;
        }
        Self::Bash
    }

    fn quote(self, text: &str) -> String {
        match self {
            Self::Bash => format!("'{}'", text.replace('\'', "'\\''")),
            Self::PowerShell => {
                let mut quoted = String::from("'");
                for c in text.chars() {
                    if "'\u{2018}\u{2019}\u{201a}\u{201b}".contains(c) {
                        quoted.push(c);
                    }
                    quoted.push(c);
                }
                quoted.push('\'');
                quoted
            }
        }
    }

    fn argument(self, flag: &str, value: &str) -> String {
        if value.is_empty() || value.starts_with('-') {
            self.quote(&format!("{flag}={value}"))
        } else {
            format!("{flag} {}", self.quote(value))
        }
    }
}

#[cfg(windows)]
fn claude_uses_bash() -> bool {
    if let Some(path) = env::var_os("CLAUDE_CODE_GIT_BASH_PATH").map(PathBuf::from)
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                ["bash.exe", "sh.exe", "bash", "sh"]
                    .iter()
                    .any(|shell| name.eq_ignore_ascii_case(shell))
            })
        && path.is_file()
    {
        return true;
    }
    if [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
    ]
    .iter()
    .any(|path| Path::new(path).is_file())
    {
        return true;
    }
    let path = env::var_os("PATH").unwrap_or_default();
    env::split_paths(&path)
        .find(|dir| dir.join("git.exe").is_file())
        .is_some_and(|dir| dir.join("../bin/bash.exe").is_file())
}

fn command(args: &CaptureArgs, client: &str, shell: Shell) -> Result<String, Diagnostic> {
    let program = if let Some(program) = &args.program {
        if program.trim().is_empty() || program.chars().any(char::is_control) {
            return Err(crate::input_error(
                "--program must be non-empty and contain no control characters.",
            ));
        }
        program.clone()
    } else {
        let path = env::current_exe().map_err(|_| {
            Diagnostic::error(
                "LOCAL_PATH",
                "Cannot locate the running executable. Supply --program.",
            )
        })?;
        let text = path.to_str().ok_or_else(|| {
            Diagnostic::error(
                "LOCAL_PATH",
                "The running executable path is not Unicode. Supply --program.",
            )
        })?;
        match shell {
            Shell::Bash if cfg!(windows) => {
                let path = text.replace('\\', "/");
                let path = if let Some(unc) = path.strip_prefix("//?/UNC/") {
                    format!("//{unc}")
                } else {
                    path.strip_prefix("//?/").unwrap_or(&path).to_owned()
                };
                shell.quote(&path)
            }
            Shell::Bash => shell.quote(text),
            Shell::PowerShell => format!("& {}", shell.quote(text)),
        }
    };
    let server = shell.argument("--server", &args.server);
    let mut command = format!("{program} env capture --client {client} {server}");
    if let Some(state) = &args.state {
        // The hook runs from whatever directory the client was started in.
        let state = std::path::absolute(state)
            .map_err(|_| Diagnostic::error("LOCAL_PATH", "Cannot resolve the --state path."))?;
        let state = state.to_str().ok_or_else(|| {
            Diagnostic::error(
                "LOCAL_PATH",
                "The --state path is not Unicode and cannot be registered.",
            )
        })?;
        command.push(' ');
        command.push_str(&shell.argument("--state", state));
    }
    Ok(command)
}

enum Settings {
    Missing,
    Link,
    File {
        bytes: Vec<u8>,
        metadata: fs::Metadata,
    },
}

fn read_settings(path: &Path) -> Result<Settings, Diagnostic> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(meta) if is_link(&meta) => return Ok(Settings::Link),
        Ok(meta) if meta.is_file() => meta,
        Ok(_) => {
            return Err(file_error(
                "LOCAL_FILE_TYPE",
                path,
                "Settings must be a regular file",
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Settings::Missing),
        Err(e) => return Err(io_error(path, "Cannot inspect settings", e)),
    };
    if metadata.len() > MAX_SETTINGS_BYTES {
        return Err(file_error(
            "LOCAL_SIZE",
            path,
            "Settings exceed the byte limit",
        ));
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x100 | 0x4); // O_NOFOLLOW | O_NONBLOCK
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x20000 | 0x800); // O_NOFOLLOW | O_NONBLOCK
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(path)
        .map_err(|e| io_error(path, "Cannot open settings", e))?;
    let metadata = file
        .metadata()
        .map_err(|e| io_error(path, "Cannot inspect open settings", e))?;
    if is_link(&metadata) {
        return Ok(Settings::Link);
    }
    if !metadata.is_file() {
        return Err(file_error(
            "LOCAL_FILE_TYPE",
            path,
            "Settings changed to a non-file",
        ));
    }
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_SETTINGS_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_error(path, "Cannot read settings", e))?;
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        return Err(file_error(
            "LOCAL_SIZE",
            path,
            "Settings exceed the byte limit",
        ));
    }
    Ok(Settings::File { bytes, metadata })
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn merge(
    path: &Path,
    bytes: &[u8],
    event: &str,
    hook: &Value,
) -> Result<Option<Vec<u8>>, Diagnostic> {
    let invalid = || {
        file_error(
            "CLI_INPUT",
            path,
            "Settings must contain strict JSON with unique keys",
        )
    };
    // Require a single JSON value. The shared parser additionally checks for duplicate keys.
    let raw: &RawValue = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let value = mcp::parse_saved_result(bytes).map_err(|_| invalid())?;
    let root = value
        .as_object()
        .ok_or_else(|| file_error("CLI_INPUT", path, "Settings must contain a JSON object"))?;
    let mut root_additions = json!({});
    if event == COPILOT_EVENT {
        match root.get("version") {
            Some(version) if version != 1 => {
                return Err(file_error(
                    "CLI_INPUT",
                    path,
                    "Expected hook configuration version 1",
                ));
            }
            None => root_additions["version"] = json!(1),
            _ => {}
        }
    }
    let mut inner = None;
    if let Some(hooks) = root.get("hooks") {
        let hooks = hooks
            .as_object()
            .ok_or_else(|| file_error("CLI_INPUT", path, "Expected hooks to be a JSON object"))?;
        let root_fields: BTreeMap<String, &RawValue> =
            serde_json::from_str(raw.get()).map_err(|_| invalid())?;
        let raw_hooks = root_fields["hooks"];
        if let Some(entries) = hooks.get(event) {
            let entries = entries.as_array().ok_or_else(|| {
                file_error(
                    "CLI_INPUT",
                    path,
                    &format!("Expected hooks.{event} to be a JSON array"),
                )
            })?;
            if !entries
                .iter()
                .any(|existing| registers(existing, hook, event))
            {
                let hook_fields: BTreeMap<String, &RawValue> =
                    serde_json::from_str(raw_hooks.get()).map_err(|_| invalid())?;
                inner = Some((
                    hook_fields[event],
                    !entries.is_empty(),
                    serde_json::to_string_pretty(hook).expect("JSON serializes"),
                ));
            }
        } else {
            inner = Some((
                raw_hooks,
                !hooks.is_empty(),
                members(&json!({event: [hook]})),
            ));
        }
    } else {
        root_additions["hooks"] = json!({event: [hook]});
    }
    let mut additions = Vec::new();
    if root_additions != json!({}) {
        additions.push(insertion(
            bytes,
            raw,
            !root.is_empty(),
            &members(&root_additions),
        ));
    }
    if let Some((container, nonempty, text)) = inner {
        additions.push(insertion(bytes, container, nonempty, &text));
    }
    if additions.is_empty() {
        return Ok(None);
    }
    let mut output = bytes.to_vec();
    // Outer containers end later, so their insertions leave inner byte offsets valid.
    for (offset, addition) in additions {
        output.splice(offset..offset, addition.bytes());
    }
    Ok(Some(output))
}

fn registers(existing: &Value, hook: &Value, event: &str) -> bool {
    if event == COPILOT_EVENT {
        return existing["type"] == "command"
            && (existing["bash"] == hook["bash"] || existing["powershell"] == hook["powershell"]);
    }
    existing["hooks"].as_array().is_some_and(|commands| {
        commands.iter().any(|command| {
            command["type"] == "command" && command["command"] == hook["hooks"][0]["command"]
        })
    })
}

fn members(object: &Value) -> String {
    let text = serde_json::to_string_pretty(object).expect("JSON serializes");
    text[2..text.len() - 2]
        .lines()
        .map(|line| line.strip_prefix("  ").unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn insertion(bytes: &[u8], raw: &RawValue, nonempty: bool, text: &str) -> (usize, String) {
    let raw = raw.get();
    let before_close = &raw[..raw.len() - 1];
    let end = before_close.trim_end().len();
    // RawValue borrows the exact container from the input byte slice.
    let offset = raw.as_ptr().addr() - bytes.as_ptr().addr() + end;
    let comma = if nonempty { "," } else { "" };
    let closing_indent = before_close[end..]
        .rsplit_once('\n')
        .map_or("", |(_, indent)| indent);
    let newline = if bytes.windows(2).any(|pair| pair == b"\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let prefix = format!("{newline}{closing_indent}  ");
    (
        offset,
        format!("{comma}{prefix}{}", text.replace('\n', &prefix)),
    )
}

fn replace(path: &Path, settings: &Settings, bytes: &[u8]) -> Result<(), Diagnostic> {
    let parent = path
        .parent()
        .expect("An absolute settings path has a parent");
    fs::create_dir_all(parent)
        .map_err(|e| io_error(path, "Cannot create settings directory", e))?;

    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .expect("A known settings filename");
    // Each installation writes a candidate of its own, so none can remove another's.
    let prefix = format!(".{name}.cqlmatic.");
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let temporary = parent.join(format!("{prefix}{}.{unique}.tmp", std::process::id()));
    // Candidates left by interrupted installations are cleared once they are old.
    for entry in fs::read_dir(parent).into_iter().flatten().flatten() {
        let abandoned =
            entry.file_name().to_str().is_some_and(|candidate| {
                candidate.starts_with(&prefix) && candidate.ends_with(".tmp")
            }) && entry.metadata().is_ok_and(|meta| {
                meta.is_file()
                    && meta
                        .modified()
                        .ok()
                        .and_then(|at| at.elapsed().ok())
                        .is_some_and(|age| age >= ABANDONED_CANDIDATE)
            });
        if abandoned {
            let _ = fs::remove_file(entry.path());
        }
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|e| io_error(path, "Cannot create settings candidate", e))?;
    let staged = (|| {
        if let Settings::File { metadata, .. } = settings {
            #[cfg(unix)]
            {
                let candidate = file
                    .metadata()
                    .map_err(|e| io_error(path, "Cannot inspect settings candidate", e))?;
                check_owner(path, metadata, &candidate)?;
            }
            #[cfg(windows)]
            if metadata.permissions().readonly() {
                return Err(file_error(
                    "LOCAL_IO",
                    path,
                    "Cannot replace read-only settings",
                ));
            }
            file.write_all(bytes)
                .and_then(|()| file.set_permissions(metadata.permissions()))
        } else {
            file.write_all(bytes)
        }
        .and_then(|()| file.sync_all())
        .map_err(|e| io_error(path, "Cannot write settings candidate", e))
    })();
    drop(file);
    let result = staged.and_then(|()| {
        let changed = || {
            file_error(
                "LOCAL_IO",
                path,
                "Settings changed during installation. Inspect the file and retry",
            )
        };
        // This reread catches a change made since the settings were first read. A save by
        // another program in the instant between it and the rename is not detected.
        match (settings, read_settings(path)?) {
            (Settings::Missing, Settings::Missing) => {
                // Creation must fail if another process created the destination in the meantime.
                fs::hard_link(&temporary, path).and_then(|()| fs::remove_file(&temporary))
            }
            (
                Settings::File {
                    bytes, metadata, ..
                },
                Settings::File {
                    bytes: current,
                    metadata: current_meta,
                    ..
                },
            ) => {
                #[cfg(unix)]
                check_owner(path, &current_meta, metadata)?;
                if *bytes != current
                    || metadata.permissions() != current_meta.permissions()
                    || metadata.modified().ok() != current_meta.modified().ok()
                {
                    return Err(changed());
                }
                fs::rename(&temporary, path)
            }
            _ => return Err(changed()),
        }
        .map_err(|e| io_error(path, "Cannot activate settings candidate", e))
    });
    if result.is_err() {
        fs::remove_file(&temporary)
            .map_err(|e| io_error(path, "Cannot remove settings candidate", e))?;
    }
    result
}

#[cfg(unix)]
fn check_owner(
    path: &Path,
    settings: &fs::Metadata,
    candidate: &fs::Metadata,
) -> Result<(), Diagnostic> {
    use std::os::unix::fs::MetadataExt;

    if settings.uid() != candidate.uid() {
        return Err(file_error(
            "LOCAL_IO",
            path,
            "Settings file has a different owner. Add the hook entry by hand",
        ));
    }
    Ok(())
}

fn file_error(code: &str, path: &Path, message: &str) -> Diagnostic {
    Diagnostic::error(code, format!("{message} ({}).", path.display()))
}

fn io_error(path: &Path, message: &str, error: io::Error) -> Diagnostic {
    file_error("LOCAL_IO", path, &format!("{message}: {error}"))
}
