//! Writes the user-level hook for clients with a supported settings file.
use super::{CaptureArgs, Client};
use crate::{inventory_cli, setup_cli};
use clap::ValueEnum;
use cqlmatic::{contracts::Diagnostic, mcp};
use serde_json::{Value, json, value::RawValue};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

const MAX_SETTINGS_BYTES: u64 = 8 * 1024 * 1024;
const POST_TOOL_USE: &str = "PostToolUse";
const COPILOT_EVENT: &str = "postToolUse";

pub(super) fn run(args: &CaptureArgs) -> Result<i32, Diagnostic> {
    if args.server.is_empty() {
        return Err(crate::input_error("--server must be non-empty."));
    }
    let client = args.client;
    let name = client
        .to_possible_value()
        .expect("A capture client has a name");
    let mut next_actions = Vec::new();
    let mut refusal = None;
    let _lock;
    let (path, event, command, entry, installed, changed) = if let Some(path) = config_path(client)?
    {
        let copilot = is_copilot(client);
        let (event, command, hook) = if copilot {
            // The Copilot clients read the same file and send the same payload, so they share
            // one entry. A second entry would run the capture twice for every search.
            let shared = Client::CopilotCli
                .to_possible_value()
                .expect("A capture client has a name");
            let bash = command(args, shared.get_name(), Shell::Bash)?;
            let powershell = command(args, shared.get_name(), Shell::PowerShell)?;
            (
                COPILOT_EVENT,
                json!({"bash": bash, "powershell": powershell}),
                json!({"type": "command", "bash": bash, "powershell": powershell}),
            )
        } else {
            let command = command(args, name.get_name(), Shell::for_client(client))?;
            let mut hook = matcher_hook(&matcher("mcp__", &args.server, "__.*"), &command, true);
            // Claude Code on Windows would otherwise pick Git Bash whenever it is installed.
            if cfg!(windows) && client == Client::ClaudeCode {
                hook["hooks"][0]["shell"] = json!("powershell");
            }
            (POST_TOOL_USE, json!(command), hook)
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
        let user = current_user()?;
        let plan = |settings: &Settings| {
            match settings {
            Settings::Missing => {
                let mut bytes = serde_json::to_vec_pretty(&entry).expect("JSON serializes");
                bytes.push(b'\n');
                Edit::Write(bytes)
            }
            // Clients fail to read a settings file that starts with a byte order mark.
            Settings::File(snapshot) if snapshot.bytes.starts_with(b"\xEF\xBB\xBF") => {
                Edit::Manual(
                    "Settings start with a byte order mark. Save the file as UTF-8 without it, then retry".into(),
                )
            }
            Settings::File(snapshot) => match merge(&snapshot.bytes, event, &hook) {
                // A file its owner made read-only is theirs to change.
                Ok(Edit::Write(_)) if snapshot.read_only() => {
                    Edit::Manual("Settings are read-only".into())
                }
                Ok(edit) => edit,
                Err(reason) => Edit::Manual(reason),
            },
            Settings::Manual(reason) => Edit::Manual((*reason).into()),
            Settings::NonRegular => Edit::Manual("Settings must be a regular file".into()),
        }
        };
        let mut settings = read_settings(&path, &user)?;
        let mut edit = plan(&settings);
        if !args.dry_run && matches!(edit, Edit::Write(_)) {
            _lock = lock_installation(&path)?;
            // Another installation may have written while this one waited for the lock.
            settings = read_settings(&path, &user)?;
            edit = plan(&settings);
        }
        if matches!(settings, Settings::NonRegular) {
            refusal = Some(file_error(
                "LOCAL_FILE_TYPE",
                &path,
                "Settings must be a regular file",
            ));
        }
        let (installed, changed) = match edit {
            Edit::Manual(reason) => {
                next_actions.push(format!(
                    "{reason} ({destination}). Review and merge the reported entry by hand."
                ));
                (false, false)
            }
            Edit::Installed => (true, false),
            Edit::Write(bytes) => {
                if bytes.len() as u64 > MAX_SETTINGS_BYTES {
                    return Err(file_error(
                        "LOCAL_SIZE",
                        &path,
                        "Updated settings exceed the byte limit",
                    ));
                }
                if !args.dry_run {
                    next_actions.extend(replace(&path, settings, &user, &bytes)?);
                    (true, true)
                } else {
                    (false, true)
                }
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
    refusal.map_or(Ok(0), Err)
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
        if cfg!(windows) && client != Client::Cline {
            Self::PowerShell
        } else {
            Self::Bash
        }
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
        // Neither shell runs a path that keeps the Windows verbatim prefix.
        #[cfg(windows)]
        let text = &match text.get(..8) {
            Some(prefix) if prefix.eq_ignore_ascii_case(r"\\?\UNC\") => {
                format!(r"\\{}", &text[8..])
            }
            _ => text.strip_prefix(r"\\?\").unwrap_or(text).to_owned(),
        };
        match shell {
            Shell::Bash if cfg!(windows) => shell.quote(&text.replace('\\', "/")),
            Shell::Bash => shell.quote(text),
            Shell::PowerShell => format!("& {}", shell.quote(text)),
        }
    };
    let server = shell.argument("--server", &args.server);
    let mut command = format!("{program} env capture --client {client} {server}");
    // The client may start the hook in another directory and with another environment, so
    // register the state this installation resolves, default or not.
    let state = inventory_cli::state_path(args.state.clone())?;
    if state.is_dir() {
        return Err(crate::input_error(
            "--state must name the inventory state file, not a directory.",
        ));
    }
    let state = state.to_str().expect("A resolved state path is Unicode");
    command.push(' ');
    command.push_str(&shell.argument("--state", state));
    Ok(command)
}

enum Settings {
    Missing,
    Manual(&'static str),
    NonRegular,
    File(Snapshot),
}

struct Snapshot {
    bytes: Vec<u8>,
    stamp: Stamp,
    // Keep the inode alive until the final comparison so its identity cannot be reused.
    _file: fs::File,
}

#[derive(PartialEq, Eq)]
struct Stamp {
    identity: (u64, u64),
    mode: u32,
    length: u64,
    modified: SystemTime,
    #[cfg(unix)]
    group: u32,
}

impl Snapshot {
    fn matches(&self, other: &Self) -> bool {
        self.stamp == other.stamp && self.bytes == other.bytes
    }

    fn read_only(&self) -> bool {
        if cfg!(unix) {
            self.stamp.mode & 0o200 == 0
        } else {
            self.stamp.mode != 0
        }
    }
}

fn read_options() -> fs::OpenOptions {
    let mut options = fs::File::options();
    options.read(true);
    #[cfg(windows)]
    options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    options
}

/// One installation at a time edits a settings file. The lock file stays beside it, so every
/// installer of that file meets the same lock.
fn lock_installation(settings: &Path) -> Result<fs::File, Diagnostic> {
    let parent = settings_directory(settings);
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder
        .create(parent)
        .map_err(|e| io_error(parent, "Cannot create settings directory", e))?;
    let name = settings
        .file_name()
        .and_then(|n| n.to_str())
        .expect("A known settings filename");
    let path = parent.join(format!(".{name}.cqlmatic.lock"));
    if fs::symlink_metadata(&path).is_ok_and(|meta| is_link(&meta) || !meta.is_file()) {
        return Err(file_error(
            "LOCAL_FILE_TYPE",
            &path,
            "Hook installation lock must be a regular file",
        ));
    }
    // The file is only ever locked. Nothing reads or writes its content.
    let mut options = read_options();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    options.mode(0o600);
    let lock = options
        .open(&path)
        .map_err(|e| io_error(&path, "Cannot open hook installation lock", e))?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(fs::TryLockError::WouldBlock) => Err(Diagnostic::error(
            "LOCAL_IO",
            "Another hook installation is running. Retry.",
        )),
        Err(fs::TryLockError::Error(e)) => {
            Err(io_error(&path, "Cannot lock hook installation lock", e))
        }
    }
}

/// The account running the installer. Windows settings keep the access rules of their
/// folder, so ownership is inspected only on Unix.
struct User(#[cfg(unix)] u32);

fn current_user() -> Result<User, Diagnostic> {
    #[cfg(unix)]
    return setup_cli::current_unix_uid().map(User);
    #[cfg(windows)]
    Ok(User())
}

fn settings_directory(path: &Path) -> &Path {
    path.parent()
        .expect("An absolute settings path has a parent")
}

fn permission_issue(metadata: &fs::Metadata, user: &User) -> Option<&'static str> {
    #[cfg(unix)]
    let (owned, shared) = (metadata.uid() == user.0, mode(metadata) & 0o002 != 0);
    #[cfg(windows)]
    let (owned, shared) = {
        let User() = user;
        (true, false)
    };
    match (metadata.is_dir(), owned, shared) {
        (true, false, _) => Some("Settings directory is owned by another user"),
        (false, false, _) => Some("Settings file is owned by another user"),
        (true, true, true) => Some("Settings directory is writable by everyone"),
        (false, true, true) => Some("Settings file is writable by everyone"),
        (_, true, false) => None,
    }
}

fn directory_issue(path: &Path, user: &User) -> Result<Option<&'static str>, Diagnostic> {
    // If the directory is missing, check where it would be created. Directory aliases are allowed.
    for directory in settings_directory(path).ancestors() {
        match fs::metadata(directory) {
            Ok(meta) if meta.is_dir() => return Ok(permission_issue(&meta, user)),
            Ok(_) => {
                return Err(file_error(
                    "LOCAL_FILE_TYPE",
                    directory,
                    "Settings parent must be a directory",
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error(directory, "Cannot inspect settings directory", e)),
        }
    }
    Err(file_error(
        "LOCAL_IO",
        path,
        "Cannot locate settings directory",
    ))
}

fn mode(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    return metadata.permissions().mode() & 0o7777;
    #[cfg(windows)]
    return u32::from(metadata.permissions().readonly());
}

#[cfg(unix)]
fn unix_identity(metadata: &fs::Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

const LINK_REASON: &str = if cfg!(windows) {
    "Settings path is a symbolic link or reparse point"
} else {
    "Settings path is a symbolic link"
};

fn read_settings(path: &Path, user: &User) -> Result<Settings, Diagnostic> {
    let inspected = match fs::symlink_metadata(path) {
        Ok(meta) if is_link(&meta) => return Ok(Settings::Manual(LINK_REASON)),
        Ok(meta) if meta.is_file() => Some(meta),
        Ok(_) => return Ok(Settings::NonRegular),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(io_error(path, "Cannot inspect settings", e)),
    };
    if let Some(meta) = &inspected
        && let Some(reason) = permission_issue(meta, user)
    {
        return Ok(Settings::Manual(reason));
    }
    if let Some(reason) = directory_issue(path, user)? {
        return Ok(Settings::Manual(reason));
    }
    let Some(inspected) = inspected else {
        return Ok(Settings::Missing);
    };

    let file = read_options()
        .open(path)
        .map_err(|e| io_error(path, "Cannot open settings", e))?;
    let metadata = file
        .metadata()
        .map_err(|e| io_error(path, "Cannot inspect open settings", e))?;
    if is_link(&metadata) {
        return Ok(Settings::Manual(LINK_REASON));
    }
    if !metadata.is_file() {
        return Err(settings_changed());
    }
    #[cfg(unix)]
    {
        let current = fs::symlink_metadata(path)
            .map_err(|e| io_error(path, "Cannot re-inspect settings", e))?;
        if is_link(&current) {
            return Ok(Settings::Manual(LINK_REASON));
        }
        if !current.is_file() || unix_identity(&current) != unix_identity(&metadata) {
            return Err(settings_changed());
        }
        if let Some(reason) = permission_issue(&current, user) {
            return Ok(Settings::Manual(reason));
        }
    }
    if let Some(reason) = permission_issue(&metadata, user) {
        return Ok(Settings::Manual(reason));
    }
    let modified = |meta: &fs::Metadata| {
        meta.modified()
            .map_err(|e| io_error(path, "Cannot read settings modification time", e))
    };
    let stamp = Stamp {
        #[cfg(unix)]
        identity: unix_identity(&metadata),
        #[cfg(windows)]
        identity: setup_cli::windows_file_identity(&file)
            .map_err(|_| file_error("LOCAL_IO", path, "Cannot identify open settings"))?,
        mode: mode(&metadata),
        length: metadata.len(),
        modified: modified(&metadata)?,
        #[cfg(unix)]
        group: metadata.gid(),
    };
    if inspected.len() != stamp.length
        || mode(&inspected) != stamp.mode
        || modified(&inspected)? != stamp.modified
    {
        return Err(settings_changed());
    }
    #[cfg(unix)]
    if unix_identity(&inspected) != stamp.identity || inspected.gid() != stamp.group {
        return Err(settings_changed());
    }
    if stamp.length > MAX_SETTINGS_BYTES {
        return Err(file_error(
            "LOCAL_SIZE",
            path,
            "Settings exceed the byte limit",
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
    if bytes.len() as u64 != stamp.length {
        return Err(settings_changed());
    }
    Ok(Settings::File(Snapshot {
        bytes,
        stamp,
        _file: file,
    }))
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

enum Edit {
    Write(Vec<u8>),
    Installed,
    Manual(String),
}

/// The edit that registers the hook, or the reason the settings need a manual edit.
fn merge(bytes: &[u8], event: &str, hook: &Value) -> Result<Edit, String> {
    let invalid = || String::from("Settings must contain strict JSON with unique keys");
    // Require a single JSON value. The shared parser additionally checks for duplicate keys.
    let raw: &RawValue = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let value = mcp::parse_saved_result(bytes).map_err(|_| {
        // Distinguish duplicate keys from values outside the parser's representation limits.
        match serde_json::from_slice::<Value>(bytes) {
            Ok(_) => invalid(),
            Err(e) => format!("Cannot read settings JSON: {e}"),
        }
    })?;
    let root = value
        .as_object()
        .ok_or("Settings must contain a JSON object")?;
    let mut root_additions = json!({});
    if event == COPILOT_EVENT {
        match root.get("version") {
            Some(version) if version != 1 => {
                return Err("Expected hook configuration version 1".into());
            }
            None => root_additions["version"] = json!(1),
            _ => {}
        }
    }
    let mut inner = None;
    if let Some(hooks) = root.get("hooks") {
        let hooks = hooks
            .as_object()
            .ok_or("Expected hooks to be a JSON object")?;
        let root_fields: BTreeMap<String, &RawValue> =
            serde_json::from_str(raw.get()).map_err(|_| invalid())?;
        let raw_hooks = root_fields["hooks"];
        if let Some(entries) = hooks.get(event) {
            let entries = entries
                .as_array()
                .ok_or_else(|| format!("Expected hooks.{event} to be a JSON array"))?;
            let mut installed = false;
            for existing in entries {
                match matching_command(existing, hook, event) {
                    Some(true) => installed = true,
                    Some(false) => {
                        return Err("The capture command is part of a different hook entry".into());
                    }
                    None => {}
                }
            }
            if !installed {
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
        return Ok(Edit::Installed);
    }
    let mut output = bytes.to_vec();
    // Outer containers end later, so their insertions leave inner byte offsets valid.
    for (offset, addition) in additions {
        output.splice(offset..offset, addition.bytes());
    }
    Ok(Edit::Write(output))
}

// When our command is present, report whether the entry registers it as requested.
fn matching_command(existing: &Value, hook: &Value, event: &str) -> Option<bool> {
    if event == COPILOT_EVENT {
        let bash = existing["bash"] == hook["bash"];
        let powershell = existing["powershell"] == hook["powershell"];
        return (bash || powershell).then_some(existing["type"] == "command" && bash && powershell);
    }
    let commands = existing["hooks"].as_array()?;
    let mut matching = commands
        .iter()
        .filter(|command| command["command"] == hook["hooks"][0]["command"])
        .peekable();
    matching.peek()?;
    Some(
        existing["matcher"] == hook["matcher"]
            && matching.any(|command| {
                command["type"] == "command" && command["shell"] == hook["hooks"][0]["shell"]
            }),
    )
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

/// Publishes the new settings. A returned warning does not undo the installation.
fn replace(
    path: &Path,
    settings: Settings,
    user: &User,
    bytes: &[u8],
) -> Result<Option<String>, Diagnostic> {
    let snapshot = match settings {
        Settings::Missing => None,
        Settings::File(snapshot) => Some(snapshot),
        _ => unreachable!("Only writable settings reach replacement"),
    };
    let parent = settings_directory(path);

    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .expect("A known settings filename");
    let prefix = format!(".{name}.cqlmatic.");
    let (temporary, file) = create_candidate(parent, &prefix)?;
    let commit = (|| {
        write_candidate(path, file, snapshot.as_ref().map(|s| &s.stamp), bytes)?;
        if let Some(original) = snapshot {
            // The client can still save after this check and before the rename.
            let current = read_settings(path, user).map_err(|error| {
                if error.code == "LOCAL_SIZE" {
                    settings_changed()
                } else {
                    error
                }
            })?;
            if !matches!(current, Settings::File(ref current) if original.matches(current)) {
                return Err(settings_changed());
            }
            drop(current);
            drop(original);
            fs::rename(&temporary, path)
                .map_err(|e| io_error(path, "Cannot replace settings", e))?;
            Ok(None)
        } else {
            fs::hard_link(&temporary, path).map_err(|e| {
                if e.kind() == io::ErrorKind::AlreadyExists || fs::symlink_metadata(path).is_ok() {
                    file_error(
                        "LOCAL_IO",
                        path,
                        "Settings appeared during installation. Retry",
                    )
                } else {
                    io_error(path, "Cannot publish new settings", e)
                }
            })?;
            Ok(fs::remove_file(&temporary).err().map(|e| {
                format!("Warning. The hook was installed, but the temporary file could not be removed ({}). {e}", temporary.display())
            }))
        }
    })();
    commit.map_err(|error| cleanup_failed_install(&temporary, error))
}

fn create_candidate(parent: &Path, prefix: &str) -> Result<(PathBuf, fs::File), Diagnostic> {
    // A new name is never an existing file or link. On Windows it takes the folder's access rules.
    let mut options = fs::File::options();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    for counter in 0..u64::MAX {
        let path = parent.join(format!("{prefix}{}.{counter}.tmp", std::process::id()));
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_error(&path, "Cannot create temporary settings", e)),
        }
    }
    Err(file_error(
        "LOCAL_IO",
        parent,
        "Cannot find a fresh temporary settings name",
    ))
}

fn write_candidate(
    path: &Path,
    file: fs::File,
    previous: Option<&Stamp>,
    bytes: &[u8],
) -> Result<(), Diagnostic> {
    let inspect = || {
        file.metadata()
            .map_err(|e| io_error(path, "Cannot inspect temporary settings", e))
    };
    (&file)
        .write_all(bytes)
        .map_err(|e| io_error(path, "Cannot write temporary settings", e))?;
    #[cfg(unix)]
    let expected = match previous {
        None => 0o600,
        Some(previous) => {
            let kept = inspect()?.gid() == previous.group
                || std::os::unix::fs::fchown(&file, None, Some(previous.group)).is_ok();
            if kept {
                previous.mode
            } else {
                // The user is not in the old group, so the file takes the folder's group,
                // which gets no more access than everyone already had.
                previous.mode & !0o070 | (previous.mode & 0o007) << 3
            }
        }
    };
    #[cfg(windows)]
    let expected = previous.map_or(0, |s| s.mode);
    #[cfg(unix)]
    let permissions = fs::Permissions::from_mode(expected);
    #[cfg(windows)]
    let permissions = {
        let mut permissions = inspect()?.permissions();
        permissions.set_readonly(expected != 0);
        permissions
    };
    file.set_permissions(permissions)
        .map_err(|e| io_error(path, "Cannot set temporary settings permissions", e))?;
    let actual = inspect()?;
    if mode(&actual) != expected {
        return Err(file_error(
            "LOCAL_IO",
            path,
            "Cannot verify temporary settings permissions",
        ));
    }
    file.sync_all()
        .map_err(|e| io_error(path, "Cannot sync temporary settings", e))
}

fn cleanup_failed_install(temporary: &Path, mut error: Diagnostic) -> Diagnostic {
    if let Err(cleanup) = fs::remove_file(temporary) {
        error.message.push_str(&format!(
            " Cannot remove the temporary file ({}). {cleanup}",
            temporary.display()
        ));
    }
    error
}

fn settings_changed() -> Diagnostic {
    Diagnostic::error("LOCAL_IO", "Settings changed during installation. Retry.")
}

fn file_error(code: &str, path: &Path, message: &str) -> Diagnostic {
    Diagnostic::error(code, format!("{message} ({}).", path.display()))
}

fn io_error(path: &Path, message: &str, error: io::Error) -> Diagnostic {
    file_error("LOCAL_IO", path, &format!("{message}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Temp(PathBuf);
    impl Temp {
        fn new(name: &str) -> Self {
            let path =
                env::temp_dir().join(format!("cqlmatic-install-{}-{name}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn skips_occupied_candidate_names_and_removes_only_its_own_file() {
        let temp = Temp::new("candidates");
        let path = temp.0.join("hooks.json");
        let prefix = format!(".hooks.json.cqlmatic.{}", std::process::id());
        let symlink = temp.0.join(format!("{prefix}.0.tmp"));
        let regular = temp.0.join(format!("{prefix}.1.tmp"));
        let leftover = temp.0.join(".hooks.json.cqlmatic.123.4.tmp");
        let sentinel = temp.0.join("sentinel");
        let unrelated = temp.0.join(".hooks.json.cqlmatic.tmp");
        fs::write(&sentinel, b"sentinel").unwrap();
        fs::write(&leftover, b"leftover").unwrap();
        fs::write(&unrelated, b"unrelated").unwrap();
        std::os::unix::fs::symlink(&sentinel, &symlink).unwrap();
        fs::hard_link(&sentinel, &regular).unwrap();

        let result = replace(&path, Settings::Missing, &current_user().unwrap(), b"{}").unwrap();
        assert_eq!(result, None);
        assert_eq!(fs::read(&path).unwrap(), b"{}");
        assert_eq!(fs::read_link(&symlink).unwrap(), sentinel);
        assert_eq!(fs::read(sentinel).unwrap(), b"sentinel");
        assert_eq!(fs::read(unrelated).unwrap(), b"unrelated");
        assert_eq!(fs::read(regular).unwrap(), b"sentinel");
        assert_eq!(fs::read(leftover).unwrap(), b"leftover");
        assert!(!temp.0.join(format!("{prefix}.2.tmp")).exists());
    }

    #[cfg(unix)]
    #[test]
    fn reports_settings_owned_by_another_user() {
        let temp = Temp::new("owner");
        let path = temp.0.join("hooks.json");
        fs::write(&path, b"{}").unwrap();
        let User(own) = current_user().unwrap();
        let Settings::Manual(reason) = read_settings(&path, &User(own + 1)).unwrap() else {
            panic!("Expected a manual report");
        };
        assert_eq!(reason, "Settings file is owned by another user");
    }

    #[cfg(unix)]
    #[test]
    fn a_group_the_user_is_not_in_gives_way_to_the_group_of_the_folder() {
        let temp = Temp::new("group");
        let path = temp.0.join("hooks.json");
        for (mode, limited) in [(0o640, 0o600), (0o664, 0o644), (0o604, 0o644)] {
            let (temporary, file) = create_candidate(&temp.0, ".hooks.json.cqlmatic.").unwrap();
            let previous = Stamp {
                identity: (0, 0),
                mode,
                length: 0,
                modified: SystemTime::now(),
                group: u32::MAX - 2,
            };
            write_candidate(&path, file, Some(&previous), b"{}").unwrap();
            let written = fs::metadata(&temporary).unwrap();
            // Only a privileged user can hand the file to a group it is not in.
            let expected = if written.gid() == previous.group {
                mode
            } else {
                limited
            };
            assert_eq!(written.permissions().mode() & 0o777, expected);
            fs::remove_file(temporary).unwrap();
        }
    }

    #[test]
    fn detects_a_different_file_with_identical_bytes_and_metadata() {
        let temp = Temp::new("identity");
        let path = temp.0.join("hooks.json");
        fs::write(&path, b"{}").unwrap();
        let user = current_user().unwrap();
        let Settings::File(original) = read_settings(&path, &user).unwrap() else {
            panic!("Expected regular settings");
        };
        let away = temp.0.join("old");
        fs::rename(&path, &away).unwrap();
        fs::write(&path, b"{}").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(original.stamp.modified)
            .unwrap();
        let Settings::File(current) = read_settings(&path, &user).unwrap() else {
            panic!("Expected regular replacement");
        };
        assert_eq!(original.stamp.modified, current.stamp.modified);
        assert_eq!(original.stamp.mode, current.stamp.mode);
        assert_eq!(original.bytes, current.bytes);
        drop(current);
        let result = replace(&path, Settings::File(original), &user, br#"{"hook":true}"#);
        assert_eq!(
            result.err().unwrap().message,
            "Settings changed during installation. Retry."
        );
        assert_eq!(fs::read(&path).unwrap(), b"{}");
        assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 2);
    }

    #[test]
    fn creation_keeps_settings_that_appeared_after_the_read() {
        let temp = Temp::new("appeared");
        let path = temp.0.join("hooks.json");
        let user = current_user().unwrap();
        let settings = read_settings(&path, &user).unwrap();
        assert!(matches!(settings, Settings::Missing));
        fs::write(&path, br#"{"keep":true}"#).unwrap();
        let result = replace(&path, settings, &user, br#"{"hook":true}"#);
        assert!(
            result
                .err()
                .unwrap()
                .message
                .contains("Settings appeared during installation")
        );
        assert_eq!(fs::read(&path).unwrap(), br#"{"keep":true}"#);
        assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 1);
    }
}
