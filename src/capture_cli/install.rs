//! Writes the user-level hooks for clients with a supported settings file.
use super::{CaptureArgs, Client, GUARD_EVENT, HookName};
use crate::{inventory_cli, setup_cli};
use clap::ValueEnum;
use cqlmatic::{contracts::Diagnostic, mcp, setup::SEARCH_TOOL, storage};
use serde::Serialize;
use serde_json::{Value, json, value::RawValue};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

const MAX_SETTINGS_BYTES: u64 = 8 * 1024 * 1024;
const MAX_MARKER_BYTES: u64 = 4096;
const POST_TOOL_USE: &str = "PostToolUse";
const COPILOT_EVENT: &str = "postToolUse";
/// Long enough for a check against a large map; after it the client runs the search unchecked.
const GUARD_TIMEOUT_SECONDS: u64 = 5;

struct Managed {
    name: &'static str,
    event: &'static str,
    entry: Value,
    previous_matcher: Option<String>,
    wanted: bool,
}

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Present,
    Added,
    Removed,
    Absent,
    Manual,
}

pub(super) fn run(args: &CaptureArgs) -> Result<i32, Diagnostic> {
    if args.server.is_empty() {
        return Err(crate::input_error("--server must be non-empty."));
    }
    let client = args.client;
    let name = client
        .to_possible_value()
        .expect("A capture client has a name");
    let (capture, guard) = chosen(&args.hooks, client)?;
    let mut next_actions = Vec::new();
    let mut refusal = None;
    let _lock;
    let (path, events, command, entry, statuses, installed, changed) = if let Some(path) =
        config_path(client)?
    {
        let copilot = is_copilot(client);
        let (command, managed, launches) = if copilot {
            // The Copilot clients read the same file and send the same payload, so they share
            // one entry. A second entry would run the capture twice for every search.
            let shared = Client::CopilotCli
                .to_possible_value()
                .expect("A capture client has a name");
            let bash = command(args, shared.get_name(), Shell::Bash)?;
            let powershell = command(args, shared.get_name(), Shell::PowerShell)?;
            let launches = vec![
                Launch::new(args, shared.get_name(), Shell::Bash, &bash)?,
                Launch::new(args, shared.get_name(), Shell::PowerShell, &powershell)?,
            ];
            (
                json!({"bash": bash, "powershell": powershell}),
                vec![Managed {
                    name: "capture",
                    event: COPILOT_EVENT,
                    entry: json!({"type": "command", "bash": bash, "powershell": powershell}),
                    previous_matcher: None,
                    wanted: capture,
                }],
                launches,
            )
        } else {
            let shell = Shell::for_client(client);
            let direct = command(args, name.get_name(), shell)?;
            let command = shell.fail_open(&direct);
            let launches = vec![Launch::new(args, name.get_name(), shell, &direct)?];
            let server = super::codex_server(client, &args.server);
            let matcher = matcher("mcp__", &server, "__.*");
            let mut guard_entry = json!({"matcher": matcher, "hooks": [{
                "type": "command", "command": command, "timeout": GUARD_TIMEOUT_SECONDS,
            }]});
            if client == Client::ClaudeCode {
                guard_entry["hooks"][0]["onFailure"] = json!("continue");
            }
            let capture_matcher = format!("mcp__.+__({SEARCH_TOOL}|execute_tool)$");
            let mut entries = [matcher_hook(&capture_matcher, &command, true), guard_entry];
            // Claude Code on Windows would otherwise pick Git Bash whenever it is installed.
            if cfg!(windows) && client == Client::ClaudeCode {
                for entry in &mut entries {
                    entry["hooks"][0]["shell"] = json!("powershell");
                }
            }
            let [capture_entry, guard_entry] = entries;
            (
                json!(command),
                vec![
                    Managed {
                        name: "capture",
                        event: POST_TOOL_USE,
                        entry: capture_entry,
                        previous_matcher: Some(matcher),
                        wanted: capture,
                    },
                    Managed {
                        name: "guard",
                        event: GUARD_EVENT,
                        entry: guard_entry,
                        previous_matcher: None,
                        wanted: guard,
                    },
                ],
                launches,
            )
        };
        let mut entry = json!({"hooks": {}});
        for hook in managed.iter().filter(|hook| hook.wanted) {
            entry["hooks"][hook.event] = json!([&hook.entry]);
        }
        if copilot {
            entry["version"] = json!(1);
        }
        let events: Vec<_> = managed
            .iter()
            .filter(|hook| hook.wanted)
            .map(|hook| hook.event)
            .collect();

        let destination = path
            .to_str()
            .ok_or_else(|| {
                file_error(
                    "LOCAL_PATH",
                    &path,
                    "Cannot report a non-Unicode settings path",
                )
            })?
            .to_owned();
        // A settings file kept elsewhere and linked into place, as dotfile managers do, is
        // edited where it really is. The link itself is left alone.
        let configured = path;
        #[cfg(unix)]
        let path = match fs::symlink_metadata(&configured) {
            Ok(meta) if meta.file_type().is_symlink() => {
                fs::canonicalize(&configured).unwrap_or_else(|_| configured.clone())
            }
            _ => configured.clone(),
        };
        #[cfg(not(unix))]
        let path = configured.clone();
        // A reason names the real file, and the link that led to it.
        let location = match path.to_str() {
            Some(real) if path != configured => format!("{real}, linked from {destination}"),
            Some(_) => destination.clone(),
            None => {
                return Err(file_error(
                    "LOCAL_PATH",
                    &path,
                    "Cannot use a non-Unicode settings path",
                ));
            }
        };
        let user = current_user()?;
        let manual = |reason: String| Plan {
            edit: Edit::Manual(reason),
            statuses: vec![Status::Manual; managed.len()],
            others: Vec::new(),
        };
        let plan = |settings: &Settings| {
            match settings {
            Settings::Missing => Plan {
                edit: if events.is_empty() {
                    Edit::Unchanged
                } else {
                    let mut bytes = serde_json::to_vec_pretty(&entry).expect("JSON serializes");
                    bytes.push(b'\n');
                    Edit::Write(bytes)
                },
                statuses: managed
                    .iter()
                    .map(|hook| {
                        if hook.wanted {
                            Status::Added
                        } else {
                            Status::Absent
                        }
                    })
                    .collect(),
                others: Vec::new(),
            },
            // Clients fail to read a settings file that starts with a byte order mark.
            Settings::File(snapshot) if snapshot.bytes.starts_with(b"\xEF\xBB\xBF") => manual(
                "Settings start with a byte order mark. Save the file as UTF-8 without it, then retry".into(),
            ),
            Settings::File(snapshot) => match edit(&snapshot.bytes, &managed, &launches) {
                // A file its owner made read-only is theirs to change.
                Ok(Plan {
                    edit: Edit::Write(_),
                    ..
                }) if snapshot.read_only() => manual("Settings are read-only".into()),
                Ok(plan) => plan,
                Err(reason) => manual(reason),
            },
            Settings::Manual(reason) => manual((*reason).into()),
            Settings::NonRegular => manual("Settings must be a regular file".into()),
        }
        };
        let read = || {
            read_settings(&path, &user).map_err(|mut error| {
                if !args.dry_run {
                    match mark(args, &path, &[], |_| true) {
                        Ok(true) => error.message.push_str(" Removed the inventory hook note because the installed hooks could not be confirmed."),
                        Err(marker_error) => error.message.push_str(&format!(" Could not update the inventory hook note. {}", marker_error.message)),
                        Ok(false) => {}
                    }
                }
                error
            })
        };
        let mut settings = read()?;
        let mut planned = plan(&settings);
        let mut failure = None;
        if !args.dry_run && matches!(planned.edit, Edit::Write(_)) {
            // The lock stays beside the configured path, in the client's own folder.
            match lock_installation(&configured, &path) {
                Ok(Some(lock)) => {
                    _lock = lock;
                    // Another installation may have written while this one waited for the lock.
                    settings = read()?;
                    planned = plan(&settings);
                }
                Ok(None) => {
                    return Err(Diagnostic::error(
                        "LOCAL_IO",
                        "Another hook installation is running. Retry.",
                    ));
                }
                Err(error) => failure = Some(error),
            }
        }
        if matches!(settings, Settings::NonRegular) {
            refusal = Some(file_error(
                "LOCAL_FILE_TYPE",
                &path,
                "Settings must be a regular file",
            ));
        }
        let Plan {
            edit,
            statuses,
            others,
        } = planned;
        let settled = !statuses.contains(&Status::Manual);
        for other in others {
            next_actions.push(format!("{other} ({location})."));
        }
        let (installed, changed) = match edit {
            Edit::Manual(reason) => {
                next_actions.push(format!(
                    "{reason} ({location}). Review and merge the reported entry by hand."
                ));
                (false, false)
            }
            Edit::Unchanged => (settled, false),
            Edit::Write(bytes) => {
                if bytes.len() as u64 > MAX_SETTINGS_BYTES {
                    return Err(file_error(
                        "LOCAL_SIZE",
                        &path,
                        "Updated settings exceed the byte limit",
                    ));
                }
                if args.dry_run {
                    (false, true)
                } else {
                    let outcome = match failure.take() {
                        Some(error) => Err(error),
                        None => replace(&path, settings, &user, &bytes),
                    };
                    match outcome {
                        Ok(warning) => {
                            next_actions.extend(warning);
                            (settled, true)
                        }
                        // The entry is still reported, so it can be added by hand.
                        Err(error) => {
                            next_actions.push(format!(
                                "{} If this persists, merge the reported entry by hand.",
                                error.message
                            ));
                            refusal = Some(error);
                            (false, false)
                        }
                    }
                }
            }
        };
        if client == Client::Codex && !events.is_empty() {
            next_actions.push("Review and trust the hooks with /hooks before they run.".into());
        }
        if !args.dry_run {
            let certain = settled && refusal.is_none();
            let hooks: Vec<_> = managed
                .iter()
                .zip(&statuses)
                .filter(|(_, status)| certain && matches!(status, Status::Present | Status::Added))
                .map(|(hook, _)| hook.name)
                .collect();
            match mark(args, &path, &hooks, |server| match read_settings(&path, &user) {
                Ok(Settings::File(snapshot)) => has_capture(args, server, &snapshot.bytes),
                Ok(Settings::Missing) => false,
                _ => true,
            }) {
                Ok(_) if !certain => next_actions.push(
                    "The inventory hook note could not confirm the installed hooks. Review the settings before relying on capture."
                        .into(),
                ),
                Ok(_) => {}
                Err(error) => next_actions.push(format!(
                    "Could not update the inventory hook note. {}",
                    error.message
                )),
            }
        }
        let statuses: serde_json::Map<_, _> = managed
            .iter()
            .zip(statuses)
            .map(|(hook, status)| (hook.name.to_owned(), json!(status)))
            .collect();
        (
            Some(destination),
            events,
            command,
            entry,
            statuses,
            installed,
            changed,
        )
    } else {
        if !capture {
            return Err(crate::input_error(
                "This client's hook is added by hand, so --hooks must include capture.",
            ));
        }
        let command = command(args, name.get_name(), Shell::for_client(client))?;
        let (path, event, entry) = manual_entry(client, &args.server, &command);
        next_actions.push(match client {
            Client::Cline => "Add the reported script as an executable PostToolUse file. The editor extension uses ~/Documents/Cline/Hooks/PostToolUse.".into(),
            Client::Generic => "Run the command after a tool call and pass a JSON object with tool_name, tool_input and tool_response on stdin.".into(),
            _ => format!("Merge the reported entry into {}.", path.expect("A documented settings path")),
        });
        (
            path.map(str::to_owned),
            event.into_iter().collect(),
            json!(command),
            entry,
            serde_json::Map::from_iter([("capture".to_owned(), json!(Status::Manual))]),
            false,
            false,
        )
    };
    next_actions.push(spelling(client, &args.server));
    crate::line(
        &json!({
            "path": path, "events": events, "command": command, "entry": entry,
            "hooks": statuses, "installed": installed, "changed": changed,
            "dry_run": args.dry_run, "next_actions": next_actions,
        })
        .to_string(),
    )?;
    refusal.map_or(Ok(0), Err)
}

/// Record the settings file that confirmed capture, so a different profile can't clear the note.
fn mark(
    args: &CaptureArgs,
    settings: &Path,
    hooks: &[&str],
    has_registration: impl FnOnce(&str) -> bool,
) -> Result<bool, Diagnostic> {
    let client = args.client;
    let name = client
        .to_possible_value()
        .expect("A capture client has a name");
    let server = super::codex_server(client, &args.server);
    let state = inventory_cli::state_path(args.state.clone())?;
    let directory = super::captures_directory(&state)
        .ok_or_else(|| crate::input_error("--state must name the inventory state file."))?;
    let marker = directory.join("hooks.json");
    let Some(settings) = fs::canonicalize(settings)
        .ok()
        .or_else(|| resolved_parent_path(settings))
    else {
        return Ok(false);
    };
    if hooks.contains(&"capture") {
        setup_cli::protect_directory(&directory)?;
        let bytes = serde_json::to_vec_pretty(&json!({
            "client": name.get_name(), "server": args.server, "hooks": hooks,
            "settings_path": settings
        }))
        .expect("A marker serializes");
        storage::Writer::acquire(&marker)?.commit_bytes(&bytes, MAX_MARKER_BYTES)?;
        return Ok(false);
    }
    if !marker.is_file() {
        return Ok(false);
    }
    let _writer = storage::Writer::acquire(&marker)?;
    let named = storage::read_bytes(&marker, MAX_MARKER_BYTES)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|note| {
            // Earlier notes have no profile binding and are replaced on the next install.
            let Some(noted_path) = note["settings_path"].as_str().map(Path::new) else {
                return false;
            };
            if noted_path != settings && !same_existing_path(noted_path, &settings) {
                return false;
            }
            let noted_client = note["client"]
                .as_str()
                .and_then(|name| Client::from_str(name, false).ok());
            let same_client = noted_client
                .is_some_and(|noted| noted == client || is_copilot(noted) && is_copilot(client));
            same_client
                && note["server"].as_str().is_none_or(|name| {
                    super::codex_server(client, name) == server || !has_registration(name)
                })
        });
    if named {
        fs::remove_file(&marker)
            .map_err(|e| io_error(&marker, "Cannot remove the hook note", e))?;
    }
    Ok(named)
}

fn has_capture(args: &CaptureArgs, server: &str, bytes: &[u8]) -> bool {
    let args = CaptureArgs {
        server: server.into(),
        ..args.clone()
    };
    let copilot = is_copilot(args.client);
    let event = if copilot {
        COPILOT_EVENT
    } else {
        POST_TOOL_USE
    };
    let Ok(settings) = settings_value(bytes, [event]) else {
        return false;
    };
    let Some(entries) = settings["hooks"][event].as_array() else {
        return false;
    };
    let (client, shells): (Client, &[_]) = if copilot {
        (Client::CopilotCli, &[Shell::Bash, Shell::PowerShell])
    } else {
        (args.client, &[Shell::for_client(args.client)])
    };
    let name = client
        .to_possible_value()
        .expect("A capture client has a name");
    let mut launches = Vec::new();
    for &shell in shells {
        let Ok(direct) = command(&args, name.get_name(), shell) else {
            return false;
        };
        let Ok(launch) = Launch::new(&args, name.get_name(), shell, &direct) else {
            return false;
        };
        launches.push(launch);
    }
    let matches_target = |launch: &Launch, command: &str| {
        launch
            .prefix(&launch.unwrap(command))
            .is_some_and(|program| !program.trim().is_empty())
    };
    entries.iter().any(|entry| {
        if copilot {
            let [Some(bash), Some(powershell)] =
                [entry["bash"].as_str(), entry["powershell"].as_str()]
            else {
                return false;
            };
            matches_target(&launches[0], bash) && matches_target(&launches[1], powershell)
        } else {
            entry["hooks"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|handler| {
                    handler["command"]
                        .as_str()
                        .is_some_and(|command| matches_target(&launches[0], command))
                })
        }
    })
}

/// The tool name the hook matches for a search on `server`, so a misspelled server shows.
fn spelling(client: Client, server: &str) -> String {
    let server = super::codex_server(client, server);
    let named = |name: String| {
        format!(
            "The hook records a search whose tool name is {name}, or the same server's execute_tool. If the client's tool names spell the server another way, install again with that spelling as --server."
        )
    };
    match client {
        Client::Cursor | Client::GeminiCli => format!(
            "The hook records a search of {SEARCH_TOOL}, or execute_tool, that the client reports from the server named {server}. If the client names the server another way, install again with that name as --server."
        ),
        Client::CopilotApp | Client::CopilotCli | Client::CopilotVscode => {
            named(format!("{server}-{SEARCH_TOOL}"))
        }
        Client::Cline => named(format!("{server}__{SEARCH_TOOL}")),
        Client::Kiro => named(format!(
            "mcp_{server}_{SEARCH_TOOL} or @{server}/{SEARCH_TOOL}"
        )),
        Client::ClaudeCode | Client::Codex | Client::Generic | Client::QwenCode => {
            named(format!("mcp__{server}__{SEARCH_TOOL}"))
        }
    }
}

/// Which hooks an installation leaves in place: capture and guard.
fn chosen(hooks: &[HookName], client: Client) -> Result<(bool, bool), Diagnostic> {
    let guarded = super::guards(client);
    let (capture, guard) = match hooks {
        [] => (true, guarded),
        [HookName::None] => (false, false),
        _ if hooks.contains(&HookName::None) => {
            return Err(crate::input_error(
                "--hooks none cannot be combined with another hook.",
            ));
        }
        _ => (
            hooks.contains(&HookName::Capture),
            hooks.contains(&HookName::Guard),
        ),
    };
    if guard && !guarded {
        return Err(crate::input_error(
            "Only claude-code and codex have the guard hook.",
        ));
    }
    Ok((capture, guard))
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

// PowerShell is that shell's name.
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy)]
enum Shell {
    Bash,
    Cmd,
    PowerShell,
}

impl Shell {
    fn for_client(client: Client) -> Self {
        if !cfg!(windows) || client == Client::Cline {
            Self::Bash
        } else if client == Client::Codex {
            // Codex starts a hook command on Windows with `cmd.exe /C`.
            Self::Cmd
        } else {
            Self::PowerShell
        }
    }

    fn quote(self, text: &str) -> String {
        match self {
            // The program splits its own command line, where backslashes before a quote
            // count as escapes, so those before the closing quote are doubled.
            Self::Cmd => {
                let slashes = text.len() - text.trim_end_matches('\\').len();
                format!("\"{text}{}\"", "\\".repeat(slashes))
            }
            Self::Bash | Self::PowerShell => format!("'{}'", self.escape(text)),
        }
    }

    /// `text` as it is written inside this shell's single quotes. `cmd.exe` has none, so for it
    /// the text is unchanged.
    fn escape(self, text: &str) -> String {
        match self {
            Self::Bash => text.replace('\'', "'\\''"),
            Self::Cmd => text.to_owned(),
            Self::PowerShell => {
                let mut escaped = String::new();
                for c in text.chars() {
                    if "'\u{2018}\u{2019}\u{201a}\u{201b}".contains(c) {
                        escaped.push(c);
                    }
                    escaped.push(c);
                }
                escaped
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

    /// Runs `command` so that every failure, including one to start it, ends with exit status
    /// 0 and nothing printed: a client refuses the tool call on exit status 2.
    fn fail_open(self, command: &str) -> String {
        let (before, after) = self.fail_open_parts();
        format!("{before}{}{after}", self.escape(command))
    }

    fn fail_open_parts(self) -> (String, &'static str) {
        match self {
            // The command reaches a second parser as data, so text it cannot parse fails there
            // and not in the client's shell. The script around it always exits 0, so an option
            // such as errexit in the client's shell has no failure to act on, and it prints the
            // command's output only when the command succeeded.
            Self::Bash => (
                format!(
                    "/bin/sh -c {} sh '",
                    self.quote(
                        r#"if out=$(/bin/sh -c "$1") && [ -n "$out" ]; then printf '%s\n' "$out"; fi; exit 0"#
                    )
                ),
                "' 2>/dev/null; exit 0",
            ),
            Self::PowerShell => (
                "try { Invoke-Expression '".into(),
                "' 2>$null } catch {}; exit 0",
            ),
            Self::Cmd => (String::new(), " 2>nul & exit 0"),
        }
    }
}

struct Launch {
    shell: Shell,
    direct: String,
    arguments: String,
    argument_prefix: String,
    state_identity: Option<PathBuf>,
}

impl Launch {
    fn new(
        args: &CaptureArgs,
        client: &str,
        shell: Shell,
        direct: &str,
    ) -> Result<Self, Diagnostic> {
        let arguments = arguments(args, client, shell)?;
        let state = inventory_cli::state_path(args.state.clone())?;
        let quoted_state = shell.quote(state.to_str().expect("A resolved state path is Unicode"));
        let argument_prefix = arguments
            .strip_suffix(&quoted_state)
            .expect("Generated arguments end with the quoted absolute state path")
            .to_owned();
        Ok(Self {
            shell,
            direct: direct.into(),
            arguments,
            argument_prefix,
            state_identity: resolved_parent_path(&state),
        })
    }

    fn mentions_target(&self, command: &str) -> bool {
        command.contains(&self.arguments)
            || command.contains(&self.shell.escape(&self.arguments))
            || self.prefix(&self.unwrap(command)).is_some()
    }

    fn unwrap<'a>(&self, command: &'a str) -> std::borrow::Cow<'a, str> {
        let (head, tail) = self.shell.fail_open_parts();
        let Some(inner) = command
            .strip_prefix(&head)
            .and_then(|text| text.strip_suffix(tail))
        else {
            return command.into();
        };
        match self.shell {
            Shell::Bash => inner.replace("'\\''", "'").into(),
            Shell::PowerShell => {
                let mut text = String::new();
                let mut chars = inner.chars();
                while let Some(c) = chars.next() {
                    if is_powershell_quote(c) && chars.next() != Some(c) {
                        return command.into();
                    }
                    text.push(c);
                }
                text.into()
            }
            Shell::Cmd => inner.into(),
        }
    }

    fn prefix<'a>(&self, direct: &'a str) -> Option<&'a str> {
        if let Some(prefix) = direct.strip_suffix(&self.arguments) {
            return Some(prefix);
        }
        let (prefix, argument) = direct.rsplit_once(&self.argument_prefix)?;
        let words;
        let state = if matches!(self.shell, Shell::Cmd) {
            // Decode the installer's state operand. Unlike shell launchers it may contain '!'.
            let state = argument.strip_prefix('"')?.strip_suffix('"')?;
            if state.contains(['"', '%']) {
                return None;
            }
            state
        } else {
            words = literal_words(self.shell, argument)?;
            let [state] = words.as_slice() else {
                return None;
            };
            state
        };
        // Historical commands must have the installer's exact argument spelling.
        if self.shell.quote(state) != argument
            || !same_state(
                self.state_identity.as_deref()?,
                &resolved_parent_path(Path::new(state))?,
            )
        {
            return None;
        }
        Some(prefix)
    }

    fn program(&self, command: &str) -> Option<Vec<String>> {
        let direct = self.unwrap(command);
        let mut words = literal_words(self.shell, self.prefix(&direct)?)?;
        if !cqlmatic_program(self.shell, &words) {
            return None;
        }
        if cfg!(windows) {
            for word in &mut words {
                *word = word.replace('\\', "/");
            }
        }
        Some(words)
    }
}

fn resolved_parent_path(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let parent = path.parent()?;
    let name = path.file_name()?;
    match fs::canonicalize(parent) {
        Ok(parent) => Some(parent.join(name)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // Resolve existing ancestors before reducing the remaining components, so
            // a symlink followed by ".." keeps the same meaning as it has on disk.
            let mut missing = Vec::new();
            let mut existing = parent;
            let mut resolved = loop {
                match fs::canonicalize(existing) {
                    Ok(path) => break path,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        missing.push(existing.components().next_back()?);
                        existing = existing.parent()?;
                    }
                    Err(_) => return None,
                }
            };
            for component in missing.into_iter().rev() {
                match component {
                    Component::ParentDir => {
                        resolved.pop();
                    }
                    Component::Normal(part) => resolved.push(part),
                    _ => {}
                }
            }
            Some(resolved.join(name))
        }
        Err(_) => None,
    }
}

fn same_state(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    let (Some(left_captures), Some(right_captures)) = (
        super::captures_directory(left),
        super::captures_directory(right),
    ) else {
        return false;
    };
    let both_missing = |a: &Path, b: &Path| {
        [a, b].into_iter().all(|path| {
            fs::metadata(path).is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
        })
    };
    // Equivalent filenames must refer to the same capture directory when one exists.
    (same_existing_path(left, right)
        && (same_existing_path(&left_captures, &right_captures)
            || both_missing(&left_captures, &right_captures)))
        || (both_missing(left, right) && same_existing_path(&left_captures, &right_captures))
}

#[cfg(unix)]
fn same_existing_path(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    fs::metadata(left).is_ok_and(|left| {
        fs::metadata(right)
            .is_ok_and(|right| left.dev() == right.dev() && left.ino() == right.ino())
    })
}

#[cfg(not(unix))]
fn same_existing_path(left: &Path, right: &Path) -> bool {
    fs::canonicalize(left)
        .is_ok_and(|left| fs::canonicalize(right).is_ok_and(|right| left == right))
}

fn owns_commands(launches: &[Launch], current: &[&str], old: &[&str]) -> bool {
    if current.len() != old.len() || launches.len() != old.len() || old.is_empty() {
        return false;
    }
    if current == old || matches!(old, [single] if *single == launches[0].direct) {
        return true;
    }
    if launches.iter().zip(old).all(|(launch, command)| {
        launch
            .prefix(&launch.unwrap(command))
            .is_some_and(|prefix| Some(prefix) == launch.direct.strip_suffix(&launch.arguments))
    }) {
        return true;
    }
    let programs = launches
        .iter()
        .zip(old)
        .map(|(launch, command)| launch.program(command))
        .collect::<Option<Vec<_>>>();
    programs.is_some_and(|programs| programs.len() == 1 || programs[0] == programs[1])
}

fn is_powershell_quote(c: char) -> bool {
    "'\u{2018}\u{2019}\u{201a}\u{201b}".contains(c)
}

fn cqlmatic_program(shell: Shell, words: &[String]) -> bool {
    let windows = cfg!(windows) || matches!(shell, Shell::Cmd);
    let basename = |word: &str| {
        let name = if windows {
            word.rsplit(['/', '\\'])
                .next()
                .expect("At least one path component")
        } else {
            word.rsplit('/')
                .next()
                .expect("At least one path component")
        };
        if windows {
            name.to_ascii_lowercase()
        } else {
            name.to_owned()
        }
    };
    if let [program] = words {
        return matches!(
            basename(program).as_str(),
            "cqlmatic"
                | "cqlmatic.exe"
                | "cqlmatic.cmd"
                | "cqlmatic.cjs"
                | "cqlmatic-linux-x86_64"
                | "cqlmatic-macos-arm64"
        );
    }
    let launcher = if let [env, assignments @ ..] = words
        && env == "env"
    {
        let count = assignments
            .iter()
            .take_while(|word| {
                word.split_once('=').is_some_and(|(key, _)| {
                    !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                })
            })
            .count();
        &assignments[count..]
    } else {
        words
    };
    match launcher {
        [program, script] if matches!(basename(program).as_str(), "node" | "node.exe") => {
            basename(script) == "cqlmatic.cjs"
        }
        [program, package] | [program, _, package] => {
            matches!(basename(program).as_str(), "npx" | "npx.cmd")
                && (launcher.len() == 2 || launcher[1] == "--yes")
                && (package == "cqlmatic"
                    || package.strip_prefix("cqlmatic@").is_some_and(|version| {
                        !version.is_empty()
                            && version
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
                    }))
        }
        _ => false,
    }
}

/// Read literal launch words without interpreting shell expressions or running a program.
fn literal_words(shell: Shell, prefix: &str) -> Option<Vec<String>> {
    let (prefix, call) = if matches!(shell, Shell::PowerShell) {
        prefix
            .trim_start()
            .strip_prefix("& ")
            .map_or((prefix, false), |text| (text, true))
    } else {
        (prefix, false)
    };
    let mut chars = prefix.chars().peekable();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    while let Some(c) = chars.next() {
        let quote = if matches!(shell, Shell::Cmd) {
            '"'
        } else {
            '\''
        };
        match c {
            c if c == quote => {
                started = true;
                loop {
                    let quoted = chars.next()?;
                    if quoted == quote {
                        if matches!(shell, Shell::PowerShell) && chars.next_if_eq(&quote).is_some()
                        {
                            word.push(quote);
                            continue;
                        }
                        break;
                    }
                    if matches!(shell, Shell::PowerShell)
                        && is_powershell_quote(quoted)
                        && chars.next() != Some(quoted)
                    {
                        return None;
                    }
                    if matches!(shell, Shell::Cmd) && matches!(quoted, '%' | '!') {
                        return None;
                    }
                    word.push(quoted);
                }
            }
            ' ' | '\t' => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            '\\' if matches!(shell, Shell::Bash) && chars.next_if_eq(&'\'').is_some() => {
                started = true;
                word.push('\'');
            }
            c if c.is_alphanumeric()
                || "_./:@+=,-\\".contains(c) && !(c == '\\' && matches!(shell, Shell::Bash)) =>
            {
                started = true;
                word.push(c);
            }
            _ => return None,
        }
    }
    if started {
        words.push(word);
    }
    if call && words.is_empty() {
        return None;
    }
    Some(words)
}

fn command(args: &CaptureArgs, client: &str, shell: Shell) -> Result<String, Diagnostic> {
    let program = if let Some(program) = &args.program {
        if program.trim().is_empty() || program.chars().any(char::is_control) {
            return Err(crate::input_error(
                "--program must be non-empty and contain no control characters.",
            ));
        }
        // cmd.exe would read the rest of the command line as part of the quoted text.
        if matches!(shell, Shell::Cmd) && program.matches('"').count() % 2 == 1 {
            return Err(crate::input_error(
                "This client starts hooks with cmd.exe. --program has a double quote that is not closed.",
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
        cmd_text(shell, "executable path", text)?;
        match shell {
            Shell::Bash if cfg!(windows) => shell.quote(&text.replace('\\', "/")),
            Shell::Bash | Shell::Cmd => shell.quote(text),
            Shell::PowerShell => format!("& {}", shell.quote(text)),
        }
    };
    Ok(program + &arguments(args, client, shell)?)
}

fn arguments(args: &CaptureArgs, client: &str, shell: Shell) -> Result<String, Diagnostic> {
    // One spelling per server, so installing under either leaves one entry.
    let server = super::codex_server(args.client, &args.server);
    cmd_text(shell, "server name", &server)?;
    let server = shell.argument("--server", &server);
    // The client may start the hook in another directory and with another environment, so
    // register the state this installation resolves, default or not.
    let state = inventory_cli::state_path(args.state.clone())?;
    if state.file_name().is_none() || state.is_dir() {
        return Err(crate::input_error(
            "--state must name the inventory state file, not a directory.",
        ));
    }
    let state = state.to_str().expect("A resolved state path is Unicode");
    cmd_text(shell, "state path", state)?;
    let state = shell.argument("--state", state);
    Ok(format!(" env capture --client {client} {server} {state}"))
}

/// `cmd.exe` expands `%NAME%` inside quotes and has no escape for a quote within them.
fn cmd_text(shell: Shell, what: &str, text: &str) -> Result<(), Diagnostic> {
    if matches!(shell, Shell::Cmd) && text.contains(['"', '%']) {
        return Err(crate::input_error(&format!(
            "This client starts hooks with cmd.exe, which cannot pass a double quote and may expand a percent sign. The {what} cannot contain either."
        )));
    }
    Ok(())
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

/// One installation at a time edits a settings file. The lock file stays beside the configured
/// path, so every installer of that path meets the same lock. `None` means another installation
/// is running.
fn lock_installation(settings: &Path, real: &Path) -> Result<Option<Vec<fs::File>>, Diagnostic> {
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
        Ok(()) => {}
        // Another installation holds it.
        Err(fs::TryLockError::WouldBlock) => return Ok(None),
        Err(fs::TryLockError::Error(e)) => {
            return Err(io_error(&path, "Cannot lock hook installation lock", e));
        }
    }
    // A settings file can be reached by more than one path: through different links, or
    // through a link and directly. Those installers have different lock files, so every
    // installer of an existing file also meets the others on the file itself.
    #[cfg(unix)]
    let locks = {
        let mut locks = vec![lock];
        if fs::symlink_metadata(real).is_ok_and(|meta| meta.is_file()) {
            let file =
                fs::File::open(real).map_err(|e| io_error(real, "Cannot open settings", e))?;
            match file.try_lock() {
                Ok(()) => {}
                Err(fs::TryLockError::WouldBlock) => return Ok(None),
                Err(fs::TryLockError::Error(e)) => {
                    return Err(io_error(real, "Cannot lock settings", e));
                }
            }
            // An installation that finished while this one waited swapped in a new file.
            let current =
                fs::metadata(real).map_err(|e| io_error(real, "Cannot inspect settings", e))?;
            let locked = file
                .metadata()
                .map_err(|e| io_error(real, "Cannot inspect settings", e))?;
            if unix_identity(&current) != unix_identity(&locked) {
                return Err(settings_changed());
            }
            locks.push(file);
        }
        locks
    };
    // Elsewhere the lock file is the only lock.
    #[cfg(not(unix))]
    let locks = {
        let _ = real;
        vec![lock]
    };
    Ok(Some(locks))
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
    "Settings path is a symbolic link whose target is missing or cannot be followed"
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
    // Swapping in a new file would leave the other names pointing at the old one.
    #[cfg(unix)]
    let links = metadata.nlink();
    #[cfg(windows)]
    let links = setup_cli::windows_link_count(&file)
        .map_err(|_| file_error("LOCAL_IO", path, "Cannot identify open settings"))?;
    if links > 1 {
        return Ok(Settings::Manual(
            "Settings file has more than one hard link, and replacing it would separate them",
        ));
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
    Unchanged,
    Manual(String),
}

struct Plan {
    edit: Edit,
    statuses: Vec<Status>,
    others: Vec<String>,
}

#[derive(PartialEq)]
enum HandlerEdit {
    Preserve,
    Update,
    Remove,
}

fn edit(bytes: &[u8], managed: &[Managed], launches: &[Launch]) -> Result<Plan, String> {
    settings_value(bytes, managed.iter().map(|hook| hook.event))?;
    let mut output = bytes.to_vec();
    let mut statuses = Vec::new();
    let mut others = Vec::new();
    for hook in managed {
        let copilot = hook.event == COPILOT_EVENT;
        let installed = commands(&hook.entry);
        let mut found = BTreeSet::new();
        let mut legacy = false;
        let mut ambiguous = false;
        let mut changes = Vec::new();
        let mut inspect = |fields: &[&RawValue], matcher: &str| {
            let old = fields
                .iter()
                .map(|field| serde_json::from_str::<String>(field.get()).ok())
                .collect::<Option<Vec<_>>>();
            let Some(old) = old else {
                return HandlerEdit::Preserve;
            };
            let old: Vec<_> = old.iter().map(String::as_str).collect();
            if !owns_commands(launches, &installed, &old) {
                if old.iter().any(|command| {
                    launches
                        .iter()
                        .any(|launch| launch.mentions_target(command))
                }) {
                    ambiguous = true;
                    others.push(format!(
                        "hooks.{} holds a different hook entry that runs CQLmatic for this server and state: {}. Review it by hand",
                        hook.event, old.join(" | ")
                    ));
                }
                return HandlerEdit::Preserve;
            }
            if !found.insert(matcher.to_owned()) || !hook.wanted {
                return HandlerEdit::Remove;
            }
            legacy |= !copilot && old != installed && old[0].ends_with(&launches[0].arguments);
            HandlerEdit::Update
        };
        let mut removals = Vec::new();
        if let Some(array) = raw_member(&output, &["hooks", hook.event]) {
            let entries: Vec<&RawValue> =
                serde_json::from_str(array.get()).expect("A checked array");
            let mut gone = Vec::new();
            for entry in &entries {
                if copilot {
                    let fields = ["bash", "powershell"]
                        .map(|key| raw_member(entry.get().as_bytes(), &[key]));
                    let action = if let [Some(bash), Some(powershell)] = fields {
                        let action = inspect(&[bash, powershell], "");
                        if action == HandlerEdit::Update {
                            changes.extend(
                                [bash, powershell].into_iter().zip(&installed).filter_map(
                                    |(field, replacement)| {
                                        replace_string(&output, field, replacement)
                                    },
                                ),
                            );
                        }
                        action
                    } else if let Some(command) = fields.into_iter().flatten().next() {
                        // A hook can run on only one shell. Leave it with its owner.
                        inspect(&[command], "")
                    } else {
                        HandlerEdit::Preserve
                    };
                    gone.push(action == HandlerEdit::Remove);
                } else if let Some(handlers) = raw_member(entry.get().as_bytes(), &["hooks"])
                    && let Ok(items) = serde_json::from_str::<Vec<&RawValue>>(handlers.get())
                {
                    let matcher = raw_member(entry.get().as_bytes(), &["matcher"]);
                    let old_matcher =
                        matcher.and_then(|raw| serde_json::from_str::<String>(raw.get()).ok());
                    let upgrade =
                        hook.previous_matcher.is_some() && old_matcher == hook.previous_matcher;
                    let effective_matcher = if upgrade {
                        hook.entry["matcher"]
                            .as_str()
                            .expect("The stock matcher is a string")
                    } else {
                        old_matcher.as_deref().unwrap_or_default()
                    };
                    let mut keep = None;
                    let mut removed = Vec::new();
                    for (index, item) in items.iter().enumerate() {
                        let action = raw_member(item.get().as_bytes(), &["command"]).map_or(
                            HandlerEdit::Preserve,
                            |field| {
                                let action = inspect(&[field], effective_matcher);
                                if action == HandlerEdit::Update {
                                    keep = Some((index, field));
                                }
                                action
                            },
                        );
                        removed.push(action == HandlerEdit::Remove);
                    }
                    if let Some((index, field)) = keep {
                        if upgrade
                            && removed
                                .iter()
                                .enumerate()
                                .any(|(i, gone)| i != index && !gone)
                        {
                            // Keep the old matcher and unrelated handlers in place.
                            let item = items[index];
                            let mut handler = item.get().to_owned();
                            if let Some((range, replacement)) =
                                replace_string(item.get().as_bytes(), field, installed[0])
                            {
                                handler.replace_range(range, &replacement);
                            }
                            removed[index] = true;
                            let end = offset(&output, entry) + entry.get().len();
                            changes.push((
                                end..end,
                                format!(
                                    ",{{\"matcher\":{},\"hooks\":[{handler}]}}",
                                    hook.entry["matcher"]
                                ),
                            ));
                        } else {
                            changes.extend(replace_string(&output, field, installed[0]));
                            if let Some(matcher) = matcher.filter(|_| upgrade) {
                                changes.extend(replace_string(&output, matcher, effective_matcher));
                            }
                        }
                    }
                    let all = !removed.is_empty() && removed.iter().all(|gone| *gone);
                    if !all {
                        removals.extend(remove_ranges(&output, handlers, &items, &removed));
                    }
                    gone.push(all);
                } else {
                    gone.push(false);
                }
            }
            removals.extend(remove_ranges(&output, array, &entries, &gone));
        }
        changes.extend(removals.into_iter().map(|range| (range, String::new())));
        changes.sort_by_key(|(range, _)| range.start);
        for (range, replacement) in changes.into_iter().rev() {
            output.splice(range, replacement.bytes());
        }
        let status = if ambiguous {
            Status::Manual
        } else if hook.wanted && found.is_empty() {
            output = insert(&output, hook.event, &hook.entry);
            Status::Added
        } else if hook.wanted && legacy {
            Status::Added
        } else if hook.wanted {
            Status::Present
        } else if !found.is_empty() {
            Status::Removed
        } else {
            Status::Absent
        };
        statuses.push(status);
    }
    Ok(Plan {
        edit: if output == bytes {
            Edit::Unchanged
        } else {
            Edit::Write(output)
        },
        statuses,
        others,
    })
}

fn settings_value<'a>(
    bytes: &[u8],
    events: impl IntoIterator<Item = &'a str>,
) -> Result<Value, String> {
    let invalid = || String::from("Settings must contain strict JSON with unique keys");
    // Require a single JSON value. The shared parser additionally checks for duplicate keys.
    serde_json::from_slice::<&RawValue>(bytes).map_err(|_| invalid())?;
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
    let hooks = root
        .get("hooks")
        .map(|hooks| {
            hooks
                .as_object()
                .ok_or("Expected hooks to be a JSON object")
        })
        .transpose()?;
    for event in events {
        if event == COPILOT_EVENT && root.get("version").is_some_and(|version| version != 1) {
            return Err("Expected hook configuration version 1".into());
        }
        if hooks
            .and_then(|hooks| hooks.get(event))
            .is_some_and(|entries| !entries.is_array())
        {
            return Err(format!("Expected hooks.{event} to be a JSON array"));
        }
    }
    Ok(value)
}

fn commands(entry: &Value) -> Vec<&str> {
    entry["hooks"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|hook| &hook["command"])
        .chain([&entry["command"], &entry["bash"], &entry["powershell"]])
        .filter_map(Value::as_str)
        .collect()
}

fn raw_member<'a>(bytes: &'a [u8], path: &[&str]) -> Option<&'a RawValue> {
    let mut raw: &RawValue = serde_json::from_slice(bytes).ok()?;
    for key in path {
        let fields: BTreeMap<String, &RawValue> = serde_json::from_str(raw.get()).ok()?;
        raw = fields.get(*key).copied()?;
    }
    Some(raw)
}

fn offset(bytes: &[u8], raw: &RawValue) -> usize {
    raw.get().as_ptr().addr() - bytes.as_ptr().addr()
}

fn replace_string(
    bytes: &[u8],
    field: &RawValue,
    value: &str,
) -> Option<(std::ops::Range<usize>, String)> {
    if serde_json::from_str::<String>(field.get()).is_ok_and(|old| old == value) {
        return None;
    }
    let start = offset(bytes, field);
    Some((
        start..start + field.get().len(),
        serde_json::to_string(value).expect("JSON serializes"),
    ))
}

fn remove_ranges(
    bytes: &[u8],
    array: &RawValue,
    items: &[&RawValue],
    gone: &[bool],
) -> Vec<std::ops::Range<usize>> {
    let start = |item: &RawValue| offset(bytes, item);
    let end = |item: &RawValue| offset(bytes, item) + item.get().len();
    let mut ranges = Vec::new();
    if !gone.is_empty() && gone.iter().all(|gone| *gone) {
        ranges.push(offset(bytes, array) + 1..end(items[items.len() - 1]));
    } else {
        let mut first = 0;
        while first < items.len() {
            if !gone[first] {
                first += 1;
                continue;
            }
            let mut last = first;
            while gone.get(last + 1) == Some(&true) {
                last += 1;
            }
            ranges.push(if first == 0 {
                start(items[0])..start(items[last + 1])
            } else {
                end(items[first - 1])..end(items[last])
            });
            first = last + 1;
        }
    }
    ranges
}

fn insert(bytes: &[u8], event: &str, entry: &Value) -> Vec<u8> {
    let raw: &RawValue = serde_json::from_slice(bytes).expect("Checked settings");
    let root: BTreeMap<String, &RawValue> =
        serde_json::from_str(raw.get()).expect("A checked object");
    let mut root_additions = json!({});
    if event == COPILOT_EVENT && !root.contains_key("version") {
        root_additions["version"] = json!(1);
    }
    let mut inner = None;
    if let Some(raw_hooks) = root.get("hooks") {
        let hooks: BTreeMap<String, &RawValue> =
            serde_json::from_str(raw_hooks.get()).expect("A checked object");
        inner = Some(match hooks.get(event) {
            Some(entries) => (
                *entries,
                !serde_json::from_str::<Vec<&RawValue>>(entries.get())
                    .expect("A checked array")
                    .is_empty(),
                serde_json::to_string_pretty(entry).expect("JSON serializes"),
            ),
            None => (
                *raw_hooks,
                !hooks.is_empty(),
                members(&json!({event: [entry]})),
            ),
        });
    } else {
        root_additions["hooks"] = json!({event: [entry]});
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
    let mut output = bytes.to_vec();
    // Outer containers end later, so their insertions leave inner byte offsets valid.
    for (offset, addition) in additions {
        output.splice(offset..offset, addition.bytes());
    }
    output
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

    #[test]
    fn quotes_for_cmd_and_refuses_what_it_cannot_carry() {
        let shell = Shell::Cmd;
        assert_eq!(
            shell.quote(r"C:\Users\A & B\it's $x (1); ^.exe"),
            r#""C:\Users\A & B\it's $x (1); ^.exe""#
        );
        // A backslash before the closing quote would otherwise escape it.
        assert_eq!(shell.quote(r"name\"), r#""name\\""#);
        assert_eq!(shell.argument("--server", "-dash"), r#""--server=-dash""#);
        assert_eq!(shell.argument("--server", "name"), r#"--server "name""#);
        for text in [r#"a"b"#, "100%"] {
            let error = cmd_text(shell, "server name", text).unwrap_err();
            assert_eq!(error.code, "CLI_INPUT");
            assert!(error.message.contains("server name"), "{error:?}");
            assert!(cmd_text(Shell::PowerShell, "server name", text).is_ok());
            assert!(cmd_text(Shell::Bash, "server name", text).is_ok());
        }
        assert!(cmd_text(shell, "server name", "it's $x & (1)").is_ok());

        // The whole command is refused for each part cmd.exe cannot carry.
        let build = |server: &str, state: &str| {
            let args = CaptureArgs {
                client: Client::Codex,
                server: server.into(),
                state: Some(env::temp_dir().join(state).join("state.json")),
                install: true,
                dry_run: true,
                program: Some("launcher".into()),
                hooks: Vec::new(),
            };
            command(&args, "codex", shell)
        };
        assert!(
            build("a-b", "plain")
                .unwrap()
                .starts_with(r#"launcher env capture --client codex --server "a_b" --state ""#)
        );
        for (server, state, what) in [
            ("100%", "plain", "server name"),
            ("a", "100%", "state path"),
        ] {
            let error = build(server, state).unwrap_err();
            assert!(error.message.contains(what), "{error:?}");
        }

        // An unclosed double quote in a program prefix would carry the rest of the command
        // line, including the part that turns a failure into exit status 0, into its text.
        let with_program = |program: &str, shell: Shell| {
            let args = CaptureArgs {
                client: Client::Codex,
                server: "a".into(),
                state: Some(env::temp_dir().join("plain").join("state.json")),
                install: true,
                dry_run: true,
                program: Some(program.into()),
                hooks: Vec::new(),
            };
            command(&args, "codex", shell)
        };
        let unclosed = r#""C:\copy\cqlmatic.exe"#;
        let error = with_program(unclosed, shell).unwrap_err();
        assert_eq!(error.code, "CLI_INPUT");
        assert!(error.message.contains("double quote"), "{error:?}");
        assert!(with_program(r#""C:\copy\cqlmatic.exe" --flag"#, shell).is_ok());
        // The other shells receive the command as quoted text.
        assert!(with_program(unclosed, Shell::Bash).is_ok());
        assert!(with_program(unclosed, Shell::PowerShell).is_ok());
    }

    #[test]
    fn cmd_hook_ownership_recognizes_exact_bang_commands() {
        let shell = Shell::Cmd;
        let args = CaptureArgs {
            client: Client::Codex,
            server: "synthetic".into(),
            state: Some(env::temp_dir().join("inventory !").join("state.json")),
            install: true,
            dry_run: true,
            program: Some(r#""C:\Tools With A Bang!\cqlmatic.exe""#.into()),
            hooks: Vec::new(),
        };
        let direct = command(&args, "codex", shell).unwrap();
        let launch = Launch::new(&args, "codex", shell, &direct).unwrap();
        let launches = [launch];
        for command in [shell.fail_open(&direct), direct] {
            let mut hook = Managed {
                name: "capture",
                event: POST_TOOL_USE,
                entry: matcher_hook(".*", &command, true),
                previous_matcher: None,
                wanted: true,
            };
            let settings = json!({"hooks": {POST_TOOL_USE: [&hook.entry]}}).to_string();
            let present =
                edit(settings.as_bytes(), std::slice::from_ref(&hook), &launches).unwrap();
            assert!(present.statuses == [Status::Present]);
            assert!(matches!(present.edit, Edit::Unchanged));
            hook.wanted = false;
            let removed = edit(settings.as_bytes(), &[hook], &launches).unwrap();
            assert!(removed.statuses == [Status::Removed]);
        }
    }

    #[test]
    fn cmd_hook_ownership_updates_and_removes_an_aliased_bang_path() {
        let temp = Temp::new("cmd-alias");
        let directory = temp.0.join("Inventory !");
        let child = directory.join("child");
        fs::create_dir_all(&child).unwrap();
        let state = directory.join("state.json");
        fs::write(&state, b"{}").unwrap();
        let shell = Shell::Cmd;
        let args = CaptureArgs {
            client: Client::Codex,
            server: "synthetic".into(),
            state: Some(state),
            install: true,
            dry_run: true,
            program: Some(r#""C:\Tools With A Bang!\cqlmatic.exe""#.into()),
            hooks: Vec::new(),
        };
        let old = shell.fail_open(&command(&args, "codex", shell).unwrap());
        let settings = json!({"hooks": {POST_TOOL_USE: [matcher_hook(".*", &old, true)]}});
        #[cfg(windows)]
        let alias = directory.join("STATE.JSON");
        #[cfg(not(windows))]
        let alias = child.join("../state.json");
        let args = CaptureArgs {
            state: Some(alias),
            ..args
        };
        let direct = command(&args, "codex", shell).unwrap();
        let launches = [Launch::new(&args, "codex", shell, &direct).unwrap()];
        let mut hook = Managed {
            name: "capture",
            event: POST_TOOL_USE,
            entry: matcher_hook(".*", &shell.fail_open(&direct), true),
            previous_matcher: None,
            wanted: true,
        };
        let updated = edit(
            settings.to_string().as_bytes(),
            std::slice::from_ref(&hook),
            &launches,
        )
        .unwrap();
        assert_eq!(json!(updated.statuses[0]), "present");
        let Edit::Write(bytes) = updated.edit else {
            panic!("Expected updated command bytes");
        };
        let saved: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(saved["hooks"][POST_TOOL_USE], json!([hook.entry]));
        hook.wanted = false;
        let removed = edit(&bytes, &[hook], &launches).unwrap();
        assert_eq!(json!(removed.statuses[0]), "removed");
        let Edit::Write(bytes) = removed.edit else {
            panic!("Expected the command to be removed");
        };
        let saved: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(saved["hooks"][POST_TOOL_USE], json!([]));
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
