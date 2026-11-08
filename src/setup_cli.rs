//! Terminal and local-I/O adapter for setup; native clients own OAuth and MCP.
use clap::Args;
use cqlmatic::{
    contracts::Diagnostic,
    network,
    setup::{self, *},
    sha256_hex, storage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    env, fs,
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{atomic::Ordering, mpsc},
    thread,
    time::{Duration, Instant},
};

const MAX_CONFIG_BYTES: u64 = 8 * 1024 * 1024;
const PROBE_TIMEOUT: u32 = 10;
const SETUP_WORDMARK: &str = r"   ______ ____    __                      __  _
  / ____// __ \  / /   ____ ___   ____ _  / /_(_)____
 / /    / / / / / /   / __ `__ \ / __ `/ / __/ // ___/
/ /___ / /_/ / / /___/ / / / / // /_/ / / /_/ // /__
\____/ \___\_\/_____/_/ /_/ /_/ \__,_/  \__/_/ \___/
";

#[derive(Args)]
pub struct SetupArgs {
    #[arg(
        long,
        value_name = "FILE",
        help = "One strict v1 SetupRequest; '-' reads bounded stdin"
    )]
    pub request: Option<PathBuf>,
    #[arg(
        long,
        value_name = "FILE",
        help = "Protected setup state in a dedicated private directory"
    )]
    pub state: Option<PathBuf>,
    #[arg(long, help = "Emit one structured result; never prompt on stdin")]
    pub json: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredState {
    schema_version: u32,
    state_sha256: String,
    state: SetupState,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeRegistrationReceipt {
    schema_version: u32,
    selection_sha256: String,
    client_sha256: String,
    commands_sha256: String,
    confirmation_sha256: String,
    baseline: ConfigBaseline,
}

pub(crate) fn default_state_path() -> Result<PathBuf, Diagnostic> {
    #[cfg(windows)]
    let root = env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let root = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state")));
    root.filter(|p| p.is_absolute()).map(|p| p.join("cqlmatic/setup/state.json")).ok_or_else(|| err("SETUP_STATE_PATH", "Cannot determine OS-local setup storage; use --state with a dedicated private directory."))
}
pub fn run(args: SetupArgs) -> Result<i32, Diagnostic> {
    let state_path = absolute(match args.state {
        Some(path) => path,
        None => default_state_path()?,
    })?;
    if let Some(request_path) = args.request {
        let raw = read_request(&request_path)?;
        let request: SetupRequest = serde_json::from_slice(&raw).map_err(|_| err("SETUP_REQUEST_JSON", "Expected one strict v1 SetupRequest JSON object; unknown fields and secrets are rejected."))?;
        request.validate()?;
        crate::install_cancel()?;
        let outcome = with_state(&state_path, |state, runner, writer| {
            perform(
                state,
                request.action,
                &state_path,
                runner,
                writer,
                !args.json && io::stdin().is_terminal(),
            )
        })?;
        emit(setup::result(outcome, request.id), args.json)
    } else if args.json || !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        // Status never probes clients, reads client configs, or waits on stdin.
        emit(setup::result(load_state(&state_path)?, None), args.json)
    } else {
        crate::install_cancel()?;
        interactive(&state_path)
    }
}
fn err(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(code, message)
}
fn absolute(path: PathBuf) -> Result<PathBuf, Diagnostic> {
    if path.is_absolute() {
        Ok(path)
    } else {
        env::current_dir()
            .map(|p| p.join(path))
            .map_err(|_| err("SETUP_PATH", "Cannot resolve local path."))
    }
}
fn parent(path: &Path) -> Result<&Path, Diagnostic> {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            err(
                "SETUP_PATH",
                "Setup state requires a dedicated parent directory.",
            )
        })
}

/// Create private storage or verify protection without changing an existing directory.
pub(crate) fn protect_directory(path: &Path) -> Result<(), Diagnostic> {
    reject_links(path)?;
    let created = !path.exists();
    if created {
        fs::create_dir_all(path)
            .map_err(|_| err("SETUP_STORAGE", "Cannot create private setup directory."))?;
    }
    if !path.is_dir() {
        return Err(err(
            "SETUP_STORAGE",
            "Private setup storage must be a directory.",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        if created {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(|_| err("SETUP_PROTECTION", "Cannot protect setup directory."))?;
        }
        let meta = fs::metadata(path).map_err(|_| {
            err(
                "SETUP_PROTECTION",
                "Cannot inspect setup directory protection.",
            )
        })?;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(private_directory_error());
        }
        // Compare to a private file's actual owner without a new libc dependency.
        let sentinel = path.join(format!(".owner-{}", std::process::id()));
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&sentinel)
            .map_err(|_| {
                err(
                    "SETUP_PROTECTION",
                    "Cannot verify setup directory ownership.",
                )
            })?;
        let owner = file
            .metadata()
            .map_err(|_| err("SETUP_PROTECTION", "Cannot inspect setup ownership."))?
            .uid();
        drop(file);
        fs::remove_file(&sentinel)
            .map_err(|_| err("SETUP_PROTECTION", "Cannot finish setup ownership check."))?;
        if meta.uid() != owner {
            return Err(private_directory_error());
        }
    }
    #[cfg(windows)]
    {
        // Fixed script, untrusted path is an environment value, never script text.
        const CHECK: &str = r#"$ErrorActionPreference='Stop'; $p=$env:CQLMATIC_SETUP_DIRECTORY; $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User; $a=[IO.Directory]::GetAccessControl($p); if($a.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $sid.Value){exit 3}; foreach($r in $a.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier])){if($r.AccessControlType -eq 'Allow' -and $r.IdentityReference.Value -ne $sid.Value -and $r.IdentityReference.Value -ne 'S-1-5-18'){exit 4}}"#;
        const CREATE: &str = r#"$ErrorActionPreference='Stop'; $p=$env:CQLMATIC_SETUP_DIRECTORY; $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User; $a=New-Object System.Security.AccessControl.DirectorySecurity; $a.SetOwner($sid); $a.SetAccessRuleProtection($true,$false); $inherit=[Security.AccessControl.InheritanceFlags]'ContainerInherit,ObjectInherit'; $prop=[Security.AccessControl.PropagationFlags]::None; $allow=[Security.AccessControl.AccessControlType]::Allow; $a.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid,'FullControl',$inherit,$prop,$allow)); $a.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new('S-1-5-18'),'FullControl',$inherit,$prop,$allow)); [IO.Directory]::SetAccessControl($p,$a)"#;
        let mut command = powershell()?;
        command
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                if created { CREATE } else { CHECK },
            ])
            .env("CQLMATIC_SETUP_DIRECTORY", path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().map_err(|_| {
            err(
                "SETUP_PROTECTION",
                "Cannot start the fixed Windows ACL helper.",
            )
        })?;
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => break,
                Ok(Some(_)) => return Err(private_directory_error()),
                Ok(None) if Instant::now() < until => thread::sleep(Duration::from_millis(20)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(err(
                        "SETUP_PROTECTION",
                        "Private-directory ACL check timed out or failed.",
                    ));
                }
            }
        }
    }
    Ok(())
}
fn private_directory_error() -> Diagnostic {
    err(
        "SETUP_PROTECTION",
        "Existing setup directory is not private to this user and SYSTEM. Use --state in a new dedicated directory, or protect that dedicated directory in the OS first. Setup does not change permissions on existing arbitrary directories.",
    )
}
#[cfg(windows)]
fn powershell() -> Result<Command, Diagnostic> {
    use std::os::windows::process::CommandExt;
    let root = env::var_os("SystemRoot").ok_or_else(|| {
        err(
            "SETUP_PROTECTION",
            "SystemRoot is unavailable for the Windows ACL helper.",
        )
    })?;
    let mut command =
        Command::new(PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe"));
    command.creation_flags(0x0800_0000);
    command.env_remove("PSModulePath");
    Ok(command)
}
fn reject_links(path: &Path) -> Result<(), Diagnostic> {
    for ancestor in path.ancestors() {
        if let Ok(meta) = fs::symlink_metadata(ancestor) {
            #[cfg(windows)]
            let linked = {
                use std::os::windows::fs::MetadataExt;
                meta.file_attributes() & 0x400 != 0
            };
            #[cfg(not(windows))]
            let linked = meta.file_type().is_symlink();
            if linked {
                return Err(err(
                    "SETUP_LINK",
                    "Setup paths must not traverse symlinks or Windows reparse points.",
                ));
            }
        }
    }
    Ok(())
}
fn load_state(path: &Path) -> Result<SetupState, Diagnostic> {
    reject_links(path)?;
    if !path.exists() {
        return Ok(SetupState::default());
    }
    protect_directory(parent(path)?)?;
    verify_private_file(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(path)
            .map_err(|_| err("SETUP_STATE", "Cannot inspect state."))?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err(err(
                "SETUP_PROTECTION",
                "Setup state must have owner-only file permissions.",
            ));
        }
    }
    let raw = storage::read_bytes(path, MAX_STATE_BYTES)?;
    let stored: StoredState = serde_json::from_slice(&raw).map_err(|_| {
        err(
            "SETUP_STATE",
            "Setup state is malformed. Preserve it for local review; setup will not overwrite it.",
        )
    })?;
    let bytes = serde_json::to_vec(&stored.state)
        .map_err(|_| err("SETUP_STATE", "Cannot inspect setup state."))?;
    if stored.schema_version != SCHEMA_VERSION || sha256_hex(&bytes) != stored.state_sha256 {
        return Err(err(
            "SETUP_STATE_INTEGRITY",
            "Setup state fingerprint does not match. Preserve it for local review; no native action was run.",
        ));
    }
    stored.state.validate()?;
    Ok(stored.state)
}
fn save_state(writer: &storage::Writer, state: &SetupState) -> Result<(), Diagnostic> {
    state.validate()?;
    let bytes = serde_json::to_vec(state)
        .map_err(|_| err("SETUP_SERIALIZATION", "Cannot serialize setup state."))?;
    let stored = StoredState {
        schema_version: SCHEMA_VERSION,
        state_sha256: sha256_hex(&bytes),
        state: state.clone(),
    };
    let raw = serde_json::to_vec_pretty(&stored).map_err(|_| {
        err(
            "SETUP_SERIALIZATION",
            "Cannot serialize protected setup state.",
        )
    })?;
    writer.commit_bytes(&raw, MAX_STATE_BYTES)
}
fn with_state(
    path: &Path,
    action: impl FnOnce(&mut SetupState, &mut NativeRunner, &storage::Writer) -> Result<(), Diagnostic>,
) -> Result<SetupState, Diagnostic> {
    protect_directory(parent(path)?)?;
    let writer = storage::Writer::acquire(path)?;
    let mut state = load_state(path)?;
    let mut runner = NativeRunner;
    let result = action(&mut state, &mut runner, &writer);
    save_state(&writer, &state)?;
    result?;
    Ok(state)
}
fn read_request(path: &Path) -> Result<Vec<u8>, Diagnostic> {
    if path != Path::new("-") {
        return storage::read_bytes(path, MAX_STATE_BYTES);
    }
    let mut raw = Vec::new();
    io::stdin()
        .take(MAX_STATE_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|_| err("SETUP_REQUEST", "Cannot read setup request from stdin."))?;
    if raw.len() as u64 > MAX_STATE_BYTES {
        return Err(err(
            "SETUP_REQUEST",
            "Setup request exceeds its byte budget.",
        ));
    }
    Ok(raw)
}

struct CommandOutput {
    success: bool,
    stdout: Vec<u8>,
}
trait Runner {
    fn run(&mut self, command: &NativeCommand) -> Result<CommandOutput, Diagnostic>;
    fn detect(&mut self) -> Result<Vec<ClientObservation>, Diagnostic>;
    fn config_path(&self, client: ClientKind) -> Result<PathBuf, Diagnostic>;
}
struct NativeRunner;
impl Runner for NativeRunner {
    fn run(&mut self, command: &NativeCommand) -> Result<CommandOutput, Diagnostic> {
        run_native(command)
    }
    fn detect(&mut self) -> Result<Vec<ClientObservation>, Diagnostic> {
        detect_clients(self)
    }
    fn config_path(&self, client: ClientKind) -> Result<PathBuf, Diagnostic> {
        client_config_path(client)
    }
}
fn run_native(native: &NativeCommand) -> Result<CommandOutput, Diagnostic> {
    let path = Path::new(&native.program);
    if !path.is_absolute() {
        return Err(err(
            "SETUP_EXECUTABLE",
            "Native executable must be an absolute discovered path.",
        ));
    }
    let mut command = if path
        .extension()
        .is_some_and(|s| s.eq_ignore_ascii_case("ps1"))
    {
        #[cfg(windows)]
        {
            let mut command = powershell()?;
            const INVOKE: &str = "$ErrorActionPreference='Stop'; $a=[string[]](ConvertFrom-Json $env:CQLMATIC_NATIVE_ARGV); & $env:CQLMATIC_NATIVE_PROGRAM @a; if($null -ne $LASTEXITCODE){exit $LASTEXITCODE}";
            command
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    INVOKE,
                ])
                .env("CQLMATIC_NATIVE_PROGRAM", &native.program)
                .env(
                    "CQLMATIC_NATIVE_ARGV",
                    serde_json::to_string(&native.args)
                        .map_err(|_| err("SETUP_COMMAND", "Cannot encode native argv."))?,
                );
            command
        }
        #[cfg(not(windows))]
        {
            return Err(err(
                "SETUP_EXECUTABLE",
                "PowerShell scripts are supported only by the Windows adapter.",
            ));
        }
    } else if path
        .extension()
        .is_some_and(|s| s.eq_ignore_ascii_case("cmd") || s.eq_ignore_ascii_case("bat"))
    {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // Native npm/VS Code shims forward %*. Preserve CRT JSON quotes through cmd.
            // Our typed adapter generates only this bounded alphabet; shell metacharacters
            // and expansion characters are refused rather than interpreted.
            let program = native
                .program
                .strip_prefix(r"\\?\")
                .unwrap_or(&native.program);
            if program.chars().any(|c| "%!&|<>^\"\r\n".contains(c))
                || native.args.iter().any(|a| {
                    a.chars()
                        .any(|c| !(c.is_ascii_alphanumeric() || " -_./:\\{}[],\"".contains(c)))
                })
            {
                return Err(err(
                    "SETUP_CMD_ARGUMENT",
                    "The reviewed Windows shim path or argv contains unsupported shell characters. Use a native executable or PowerShell shim; no command was run.",
                ));
            }
            let mut args = String::new();
            for arg in &native.args {
                args.push(' ');
                args.push('"');
                args.push_str(&arg.replace('"', "\\\""));
                args.push('"');
            }
            let root = env::var_os("SystemRoot").ok_or_else(|| {
                err(
                    "SETUP_EXECUTABLE",
                    "SystemRoot is unavailable for the Windows shim adapter.",
                )
            })?;
            let mut command = Command::new(PathBuf::from(root).join("System32").join("cmd.exe"));
            command
                .args(["/d", "/s", "/c"])
                .raw_arg(format!("\"\"{program}\"{args}\""));
            command
        }
        #[cfg(not(windows))]
        {
            return Err(err(
                "SETUP_EXECUTABLE",
                "Windows batch shims are supported only on Windows.",
            ));
        }
    } else {
        let mut command = Command::new(path);
        command.args(&native.args);
        command
    };
    command.stdin(if native.interactive {
        Stdio::inherit()
    } else {
        Stdio::null()
    });
    if native.interactive {
        command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    } else {
        command.stdout(Stdio::piped()).stderr(Stdio::null());
    }
    let mut child = command.spawn().map_err(|_| err("SETUP_NATIVE_START", "Cannot start the reviewed native-client command. No output or credentials were captured in setup state."))?;
    let output_thread = child.stdout.take().map(|stdout| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout
                .take(MAX_COMMAND_BYTES as u64 + 1)
                .read_to_end(&mut bytes);
            (result.is_ok(), bytes)
        })
    });
    let deadline = Instant::now() + Duration::from_secs(u64::from(native.timeout_seconds));
    let status = loop {
        if network::CANCELLED.load(Ordering::Relaxed) || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            drop(output_thread);
            return Err(err(
                if network::CANCELLED.load(Ordering::Relaxed) {
                    "SETUP_CANCELLED"
                } else {
                    "SETUP_TIMEOUT"
                },
                "Native command stopped or timed out; it may have partially changed client configuration. Inspect it before retrying.",
            ));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(err(
                    "SETUP_NATIVE_WAIT",
                    "Cannot determine native command outcome. Inspect the client before another attempt.",
                ));
            }
        }
    };
    let stdout = if let Some(output) = output_thread {
        while !output.is_finished() {
            if Instant::now() >= deadline || network::CANCELLED.load(Ordering::Relaxed) {
                return Err(err(
                    "SETUP_TIMEOUT",
                    "Native output did not close before the command deadline. Inspect the client before retrying.",
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
        let (ok, bytes) = output.join().map_err(|_| {
            err(
                "SETUP_NATIVE_OUTPUT",
                "Cannot capture bounded native output.",
            )
        })?;
        if !ok || bytes.len() > MAX_COMMAND_BYTES {
            return Err(err(
                "SETUP_NATIVE_OUTPUT",
                "Native output exceeded its byte budget or could not be read. No output was persisted.",
            ));
        }
        bytes
    } else {
        Vec::new()
    };
    Ok(CommandOutput {
        success: status.success(),
        stdout,
    })
}
fn probe(runner: &mut impl Runner, program: &Path, args: &[&str]) -> Result<Vec<u8>, Diagnostic> {
    let output = runner.run(&NativeCommand {
        program: program.to_string_lossy().into_owned(),
        args: args.iter().map(|a| (*a).into()).collect(),
        interactive: false,
        timeout_seconds: PROBE_TIMEOUT,
    })?;
    if !output.success {
        return Err(err(
            "SETUP_PROBE",
            "Native client capability probe failed. Update or inspect the client; no fallback command was attempted.",
        ));
    }
    Ok(output.stdout)
}
fn discover_executable(client: ClientKind) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    #[cfg(windows)]
    let names: &[&str] = match client {
        ClientKind::Codex => &["codex.exe", "codex.cmd", "codex.ps1"],
        ClientKind::VsCode => &["code.exe", "code.cmd", "code.ps1"],
    };
    #[cfg(not(windows))]
    let names: &[&str] = match client {
        ClientKind::Codex => &["codex"],
        ClientKind::VsCode => &["code"],
    };
    for directory in env::split_paths(&paths).filter(|p| p.is_absolute()) {
        for name in names {
            let path = directory.join(name);
            if !path.is_file() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if fs::metadata(&path).ok()?.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            if let Ok(path) = fs::canonicalize(path) {
                return Some(path);
            }
        }
    }
    None
}
fn detect_clients(runner: &mut impl Runner) -> Result<Vec<ClientObservation>, Diagnostic> {
    let mut clients = Vec::new();
    for client in [ClientKind::Codex, ClientKind::VsCode] {
        let Some(program) = discover_executable(client) else {
            continue;
        };
        let Ok(version) = probe(runner, &program, &["--version"]) else {
            continue;
        };
        let Ok(help) = probe(runner, &program, &["--help"]) else {
            continue;
        };
        let mut capability = help;
        let (supports_http, supports_cimd, supports_manual, supports_login) = match client {
            ClientKind::Codex => {
                let Ok(add) = probe(runner, &program, &["mcp", "add", "--help"]) else {
                    continue;
                };
                let Ok(login) = probe(runner, &program, &["mcp", "login", "--help"]) else {
                    continue;
                };
                let add_text = String::from_utf8_lossy(&add);
                let login_text = String::from_utf8_lossy(&login);
                let http = add_text.contains("--url");
                let cimd = add_text.contains("--oauth-client-registration")
                    && add_text.contains("cimd")
                    && login_text.contains("--oauth-client-registration")
                    && login_text.contains("cimd");
                capability.extend(add);
                capability.extend(login);
                let manual = String::from_utf8_lossy(&capability).contains("--oauth-client-id");
                (http, cimd, manual, true)
            }
            ClientKind::VsCode => {
                let http = String::from_utf8_lossy(&capability).contains("--add-mcp");
                (
                    http,
                    http && setup::vscode_supports_cimd(&String::from_utf8_lossy(&version)),
                    http,
                    false,
                )
            }
        };
        let version = String::from_utf8_lossy(&version)
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_control())
            .take(160)
            .collect();
        let executable = program.to_string_lossy().into_owned();
        let executable_sha256 = sha256_hex(&storage::read_bytes(&program, 256 * 1024 * 1024)?);
        clients.push(ClientObservation {
            client,
            executable,
            executable_sha256,
            version,
            supports_http,
            supports_cimd,
            supports_manual,
            supports_login,
            capability_sha256: sha256_hex(&capability),
        });
    }
    Ok(clients)
}
fn client_config_path(client: ClientKind) -> Result<PathBuf, Diagnostic> {
    let home = || {
        env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .ok_or_else(|| {
                err(
                    "SETUP_CONFIG_PATH",
                    "Home directory is unavailable; use the client's native configuration UI.",
                )
            })
    };
    let path = match client {
        ClientKind::Codex => env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or(home()?.join(".codex"))
            .join("config.toml"),
        ClientKind::VsCode => {
            if env::var_os("VSCODE_PORTABLE").is_some() {
                return Err(err(
                    "SETUP_PROFILE",
                    "Portable VS Code requires manual review in the native UI; this adapter targets the default user profile.",
                ));
            }
            #[cfg(windows)]
            let root = env::var_os("APPDATA").map(PathBuf::from).ok_or_else(|| {
                err(
                    "SETUP_CONFIG_PATH",
                    "APPDATA is unavailable for the default VS Code user profile.",
                )
            })?;
            #[cfg(target_os = "macos")]
            let root = home()?.join("Library/Application Support");
            #[cfg(all(not(windows), not(target_os = "macos")))]
            let root = env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or(home()?.join(".config"));
            root.join("Code/User/mcp.json")
        }
    };
    if !path.is_absolute() {
        return Err(err(
            "SETUP_CONFIG_PATH",
            "Native configuration root must be absolute.",
        ));
    }
    reject_links(&path)?;
    Ok(path)
}
fn config_hash(path: &Path) -> Result<Option<String>, Diagnostic> {
    reject_links(path)?;
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(sha256_hex(&storage::read_bytes(
        path,
        MAX_CONFIG_BYTES,
    )?)))
}
fn inspect_baseline(
    state: &SetupState,
    state_path: &Path,
    runner: &mut impl Runner,
) -> Result<ConfigBaseline, Diagnostic> {
    inspect_baseline_for(state, state_path, runner, BaselineInspection::Reuse)
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum BaselineInspection {
    Reuse,
    ReviewedAddition,
}
fn inspect_baseline_for(
    state: &SetupState,
    state_path: &Path,
    runner: &mut impl Runner,
    inspection: BaselineInspection,
) -> Result<ConfigBaseline, Diagnostic> {
    let selected = state
        .selection
        .as_ref()
        .ok_or_else(|| err("SETUP_SELECTION", "Choose a client and cloud first."))?;
    let config = runner.config_path(selected.client)?;
    let sha256 = config_hash(&config)?;
    let server = match selected.client {
        ClientKind::Codex => {
            let client = state
                .clients
                .iter()
                .find(|c| c.client == selected.client)
                .ok_or_else(|| {
                    err(
                        "SETUP_INSTALL",
                        "Install the selected native client and detect again.",
                    )
                })?;
            let raw = probe(
                runner,
                Path::new(&client.executable),
                &["mcp", "list", "--json"],
            )?;
            let list: Value = serde_json::from_slice(&raw).map_err(|_| err("SETUP_NATIVE_CONFIG", "Native MCP list did not return a JSON array. No configuration change was attempted."))?;
            let list = list.as_array().ok_or_else(|| {
                err(
                    "SETUP_NATIVE_CONFIG",
                    "Native MCP list did not return a JSON array.",
                )
            })?;
            let matching: Vec<_> = list
                .iter()
                .filter(|entry| {
                    entry.get("name").and_then(Value::as_str) == Some(&selected.server_name)
                })
                .collect();
            if matching.len() > 1 {
                ExistingServer::Collision
            } else if let Some(entry) = matching.first() {
                let transport = &entry["transport"];
                if transport["url"].as_str() == Some(selected.cloud.endpoint())
                    && matches!(transport["type"].as_str(), Some("streamable_http" | "http"))
                    && entry["enabled"].as_bool() != Some(false)
                    && transport.get("http_headers").is_none_or(Value::is_null)
                    && transport
                        .get("bearer_token_env_var")
                        .is_none_or(Value::is_null)
                    && transport.get("env_http_headers").is_none_or(Value::is_null)
                    && transport
                        .get("http_headers_helper")
                        .is_none_or(Value::is_null)
                {
                    let baseline = ConfigBaseline {
                        path: config.to_string_lossy().into_owned(),
                        sha256: sha256.clone(),
                        server: ExistingServer::Same,
                    };
                    if inspection == BaselineInspection::Reuse
                        && !has_registration_receipt(state, state_path, &baseline)?
                    {
                        return Err(err(
                            "SETUP_REGISTRATION_UNINSPECTABLE",
                            "Codex MCP list does not expose the selected OAuth registration. A matching URL cannot confirm the manual client ID or CIMD identity. Review this server's registration and authenticate in Codex, or select an unused server name. Setup will not reuse or overwrite this entry; no native change was attempted.",
                        ));
                    }
                    ExistingServer::Same
                } else {
                    ExistingServer::Collision
                }
            } else {
                ExistingServer::Absent
            }
        }
        ClientKind::VsCode => {
            if sha256.is_none() {
                ExistingServer::Absent
            } else {
                let raw = storage::read_bytes(&config, MAX_CONFIG_BYTES)?;
                let value: Value = serde_json::from_slice(&raw).map_err(|_| err("SETUP_NATIVE_CONFIG", "VS Code mcp.json is not strict JSON (comments/JSONC require manual review). Setup will not overwrite it."))?;
                let object = value.as_object().ok_or_else(|| {
                    err(
                        "SETUP_NATIVE_CONFIG",
                        "VS Code MCP configuration must be an object.",
                    )
                })?;
                let servers = object
                    .get("servers")
                    .map(|v| {
                        v.as_object().ok_or_else(|| {
                            err("SETUP_NATIVE_CONFIG", "VS Code servers must be an object.")
                        })
                    })
                    .transpose()?;
                match servers.and_then(|servers| servers.get(&selected.server_name)) {
                    None => ExistingServer::Absent,
                    Some(entry) => {
                        let expected_id = match &selected.registration {
                            RegistrationMode::Manual { client_id, .. } => Some(client_id.as_str()),
                            _ => None,
                        };
                        if entry["type"].as_str() == Some("http")
                            && entry["url"].as_str() == Some(selected.cloud.endpoint())
                            && entry.pointer("/oauth/clientId").and_then(Value::as_str)
                                == expected_id
                            && entry.get("headers").is_none()
                        {
                            ExistingServer::Same
                        } else {
                            ExistingServer::Collision
                        }
                    }
                }
            }
        }
    };
    Ok(ConfigBaseline {
        path: config.to_string_lossy().into_owned(),
        sha256,
        server,
    })
}
fn registration_receipt_path(state_path: &Path) -> Result<PathBuf, Diagnostic> {
    let mut name = state_path
        .file_name()
        .ok_or_else(|| err("SETUP_PATH", "Setup state requires a filename."))?
        .to_os_string();
    name.push(".registration.json");
    Ok(parent(state_path)?.join(name))
}
fn registration_digest(value: &impl Serialize) -> Result<String, Diagnostic> {
    serde_json::to_vec(value)
        .map(|bytes| sha256_hex(&bytes))
        .map_err(|_| {
            err(
                "SETUP_REGISTRATION_RECEIPT",
                "Cannot fingerprint native registration provenance.",
            )
        })
}
fn has_registration_receipt(
    state: &SetupState,
    state_path: &Path,
    baseline: &ConfigBaseline,
) -> Result<bool, Diagnostic> {
    if !state.configured || baseline.sha256.is_none() || state.configured_sha256 != baseline.sha256
    {
        return Ok(false);
    }
    let path = registration_receipt_path(state_path)?;
    reject_links(&path)?;
    if !path.exists() {
        return Ok(false);
    }
    verify_private_file(&path)?;
    let raw = storage::read_bytes(&path, MAX_STATE_BYTES)?;
    let receipt: NativeRegistrationReceipt = serde_json::from_slice(&raw).map_err(|_| {
        err("SETUP_REGISTRATION_RECEIPT", "Native registration provenance is malformed. Review this server in Codex; no native change was attempted.")
    })?;
    // Reconstruct original add arguments, including during a no-op apply's postcondition.
    let mut provenance_state = state.clone();
    provenance_state.phase = SetupPhase::Selected;
    let expected = setup::build_plan(
        &provenance_state,
        PlanOperation::Configure,
        ConfigBaseline {
            path: baseline.path.clone(),
            sha256: None,
            server: ExistingServer::Absent,
        },
        None,
    )?;
    Ok(receipt.schema_version == SCHEMA_VERSION
        && receipt.baseline == *baseline
        && receipt.selection_sha256 == registration_digest(&expected.selection)?
        && receipt.client_sha256 == registration_digest(&expected.client)?
        && receipt.commands_sha256 == registration_digest(&expected.commands)?
        && setup::is_digest(&receipt.confirmation_sha256))
}
fn save_registration_receipt(
    state_path: &Path,
    plan: &SetupPlan,
    baseline: &ConfigBaseline,
) -> Result<(), Diagnostic> {
    let path = registration_receipt_path(state_path)?;
    reject_links(&path)?;
    if path.exists() {
        verify_private_file(&path)?;
    }
    let receipt = NativeRegistrationReceipt {
        schema_version: SCHEMA_VERSION,
        selection_sha256: registration_digest(&plan.selection)?,
        client_sha256: registration_digest(&plan.client)?,
        commands_sha256: registration_digest(&plan.commands)?,
        confirmation_sha256: plan.confirmation_sha256.clone(),
        baseline: baseline.clone(),
    };
    let raw = serde_json::to_vec_pretty(&receipt).map_err(|_| {
        err("SETUP_REGISTRATION_RECEIPT", "Cannot save native registration provenance. Inspect the pending native command before continuing.")
    })?;
    storage::Writer::acquire(&path)?.commit_bytes(&raw, MAX_STATE_BYTES)?;
    verify_private_file(&path)
}
fn backup_path(state_path: &Path, baseline: &ConfigBaseline) -> Result<Option<String>, Diagnostic> {
    baseline
        .sha256
        .as_ref()
        .map(|digest| {
            parent(state_path).map(|p| {
                p.join("backups")
                    .join(format!("{digest}.original"))
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .transpose()
}
fn protect_backup(plan: &SetupPlan) -> Result<(), Diagnostic> {
    let Some(path) = plan.backup_path.as_deref() else {
        return Ok(());
    };
    let path = Path::new(path);
    protect_directory(parent(path)?)?;
    let bytes = storage::read_bytes(Path::new(&plan.baseline.path), MAX_CONFIG_BYTES)?;
    if Some(sha256_hex(&bytes)) != plan.baseline.sha256 {
        return Err(err(
            "SETUP_BASELINE_CHANGED",
            "Client configuration changed since preview. Preview again; nothing was applied.",
        ));
    }
    // Backups are create-only and byte-exact; Writer snapshots intentionally add a newline.
    if path.exists() {
        verify_private_file(path)?;
        if config_hash(path)? != plan.baseline.sha256 {
            return Err(err(
                "SETUP_BACKUP",
                "Existing protected backup differs from the reviewed baseline. Preserve it and inspect locally.",
            ));
        }
        return Ok(());
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|_| {
        err(
            "SETUP_BACKUP",
            "Cannot create the protected, byte-exact backup. No native command was run.",
        )
    })?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| {
            err(
                "SETUP_BACKUP",
                "Cannot durably write the protected backup. No native command was run.",
            )
        })?;
    sync_directory(parent(path)?)?;
    verify_private_file(path)?;
    if config_hash(path)? != plan.baseline.sha256 {
        return Err(err(
            "SETUP_BACKUP",
            "Backup verification failed. No native command was run.",
        ));
    }
    Ok(())
}
fn sync_directory(path: &Path) -> Result<(), Diagnostic> {
    #[cfg(windows)]
    let result = {
        use std::os::windows::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(0x0200_0000)
            .open(path)
            .and_then(|f| f.sync_all())
    };
    #[cfg(not(windows))]
    let result = fs::File::open(path).and_then(|f| f.sync_all());
    result.map_err(|_| {
        err(
            "SETUP_DURABILITY",
            "Cannot confirm protected backup directory durability. No native command was run.",
        )
    })
}
fn verify_private_file(path: &Path) -> Result<(), Diagnostic> {
    reject_links(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = fs::metadata(path)
            .map_err(|_| err("SETUP_PROTECTION", "Cannot inspect protected file."))?;
        let owner = fs::metadata(parent(path)?)
            .map_err(|_| err("SETUP_PROTECTION", "Cannot inspect protected file owner."))?
            .uid();
        if !meta.is_file()
            || meta.uid() != owner
            || meta.nlink() != 1
            || meta.permissions().mode() & 0o077 != 0
        {
            return Err(err(
                "SETUP_PROTECTION",
                "Protected setup files must be owner-only regular files without extra hard links.",
            ));
        }
    }
    #[cfg(windows)]
    {
        const CHECK: &str = "$ErrorActionPreference='Stop'; $p=$env:CQLMATIC_SETUP_FILE; $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User; $a=[IO.File]::GetAccessControl($p); if($a.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $sid.Value){exit 3}; foreach($r in $a.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier])){if($r.AccessControlType -eq 'Allow' -and $r.IdentityReference.Value -ne $sid.Value -and $r.IdentityReference.Value -ne 'S-1-5-18'){exit 4}}";
        let mut command = powershell()?;
        command
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                CHECK,
            ])
            .env("CQLMATIC_SETUP_FILE", path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().map_err(|_| {
            err(
                "SETUP_PROTECTION",
                "Cannot inspect setup-file ACL protection.",
            )
        })?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(_)) => {
                    return Err(err(
                        "SETUP_PROTECTION",
                        "Setup state or backup file is accessible to another principal. Protect that file before resuming; setup will not read it.",
                    ));
                }
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(err(
                        "SETUP_PROTECTION",
                        "Setup-file ACL verification did not complete.",
                    ));
                }
            }
        }
    }
    #[cfg(not(windows))]
    Ok(())
}
fn perform(
    state: &mut SetupState,
    action: SetupAction,
    state_path: &Path,
    runner: &mut impl Runner,
    writer: &storage::Writer,
    interactive: bool,
) -> Result<(), Diagnostic> {
    if network::CANCELLED.load(Ordering::Relaxed) {
        state.cancel();
        return Ok(());
    }
    match action {
        SetupAction::Status {} => Ok(()),
        SetupAction::Detect {} => {
            let clients = runner.detect()?;
            if state.phase == SetupPhase::CommandPending {
                return Err(err(
                    "SETUP_PENDING",
                    "Inspect and acknowledge the pending native command before re-detecting.",
                ));
            }
            state.invalidate_plan();
            state.clients = clients;
            if let Some(s) = &state.selection
                && !state.clients.iter().any(|c| c.client == s.client)
            {
                state.phase = SetupPhase::InstallationRequired;
            }
            Ok(())
        }
        SetupAction::Select { selection } => state.select(selection),
        SetupAction::Preview { operation } => {
            if state.phase == SetupPhase::CommandPending {
                return Err(err(
                    "SETUP_PENDING",
                    "Inspect and acknowledge the pending native command before previewing another operation.",
                ));
            }
            state.clients = runner.detect()?;
            state.invalidate_plan();
            let baseline = inspect_baseline(state, state_path, runner)?;
            let backup = if operation == PlanOperation::Configure {
                backup_path(state_path, &baseline)?
            } else {
                None
            };
            let mut plan = setup::build_plan(state, operation, baseline, backup)?;
            plan.changes
                .push(format!("Protected setup state: {}", state_path.display()));
            if plan.backup_path.is_some() {
                plan.changes.push("The original client configuration may contain secrets. Its exact backup remains private; setup never prints config bytes or stores native output.".into());
            }
            plan.confirmation_sha256 = plan.digest()?;
            state.plan = Some(plan);
            state.phase = SetupPhase::ReviewRequired;
            Ok(())
        }
        SetupAction::Apply {
            confirmation_sha256,
        } => {
            if state.phase == SetupPhase::CommandPending {
                return Err(err(
                    "SETUP_PENDING",
                    "A previous native command may have changed client configuration. Inspect and acknowledge it; automatic retries are blocked.",
                ));
            }
            let plan = state.plan.clone().ok_or_else(|| {
                err(
                    "SETUP_PREVIEW_REQUIRED",
                    "Preview local changes and review the exact command before apply.",
                )
            })?;
            plan.validate()?;
            if confirmation_sha256 != plan.confirmation_sha256
                || plan.state_revision != state.revision
            {
                return Err(err(
                    "SETUP_CONFIRMATION",
                    "Confirmation does not match the current reviewed plan. Preview again.",
                ));
            }
            let current = runner.detect()?;
            if !current.iter().any(|client| client == &plan.client) {
                return Err(err(
                    "SETUP_CLIENT_CHANGED",
                    "The selected executable or capabilities changed since review. Detect and preview again.",
                ));
            }
            let baseline = inspect_baseline(state, state_path, runner)?;
            if baseline != plan.baseline {
                return Err(err(
                    "SETUP_BASELINE_CHANGED",
                    "Client configuration changed since preview. No native change was attempted; preview again.",
                ));
            }
            if plan.commands.iter().any(|c| c.interactive) && !interactive {
                return Err(err(
                    "SETUP_INTERACTIVE_REQUIRED",
                    "Native browser consent requires a terminal. Run cqlmatic setup interactively; JSON mode never waits for login input.",
                ));
            }
            // Rebuild executable argv from trusted selection/capability data; state is not a command input format.
            let expected =
                setup::build_plan(state, plan.operation, baseline, plan.backup_path.clone())?;
            if expected.commands != plan.commands
                || plan.backup_path
                    != if plan.operation == PlanOperation::Configure {
                        backup_path(state_path, &plan.baseline)?
                    } else {
                        None
                    }
            {
                return Err(err(
                    "SETUP_PLAN_INTEGRITY",
                    "Stored command or backup target differs from the native adapter's derived plan. Preview again.",
                ));
            }
            if plan.operation == PlanOperation::Configure && !plan.commands.is_empty() {
                protect_backup(&plan)?;
            }
            state.phase = SetupPhase::CommandPending;
            state.last_notice = Some("Native command is pending. If interrupted, inspect the client; no rollback or automatic retry is assumed.".into());
            save_state(writer, state)?;
            for command in &plan.commands {
                match runner.run(command) {
                    Ok(output) if output.success => {}
                    Ok(_) => {
                        return Err(err(
                            "SETUP_NATIVE_FAILED",
                            "Native command failed and may have partially changed configuration. Inspect the client, preserve the protected backup, and review the current baseline before another attempt. No rollback was performed.",
                        ));
                    }
                    Err(diagnostic) => return Err(diagnostic),
                }
            }
            if plan.operation == PlanOperation::Configure {
                // Only a fresh reviewed add supplies registration arguments itself.
                // Native list output can establish the HTTP entry, never OAuth identity.
                let inspection = if plan.client.client == ClientKind::Codex
                    && plan.baseline.server == ExistingServer::Absent
                    && !plan.commands.is_empty()
                {
                    BaselineInspection::ReviewedAddition
                } else {
                    BaselineInspection::Reuse
                };
                let actual = inspect_baseline_for(state, state_path, runner, inspection)?;
                if actual.server != ExistingServer::Same
                    || (inspection == BaselineInspection::ReviewedAddition
                        && actual.sha256.is_none())
                {
                    return Err(err(
                        "SETUP_POSTCONDITION",
                        "Native command returned success but the desired server was not observed. Configuration remains pending for review; no authentication is claimed.",
                    ));
                }
                if inspection == BaselineInspection::ReviewedAddition {
                    save_registration_receipt(state_path, &plan, &actual)?;
                }
                state.configured = true;
                state.configured_sha256 = actual.sha256;
                state.authentication_evidence = AuthenticationEvidence::None;
                state.discovery_sha256 = None;
                state.last_notice = Some(if plan.client.client == ClientKind::Codex {
                    "Configured the HTTP entry after the exact reviewed native add command. A protected receipt binds the requested registration arguments to unchanged local configuration; Codex list output does not independently inspect OAuth identity or attest authentication."
                } else {
                    "Configured the selected native client; authentication is still required. Native list/config output does not attest authentication."
                }.into());
            } else {
                state.last_notice = Some("Native authentication command completed, or native UI steps are pending. Its exit status does not attest authentication. Import a saved host tool-discovery report next.".into());
            }
            state.invalidate_plan();
            state.phase = SetupPhase::AuthenticationRequired;
            Ok(())
        }
        SetupAction::ConfirmAuthentication {} => {
            if !state.configured || state.phase == SetupPhase::CommandPending {
                return Err(err(
                    "SETUP_NOT_CONFIGURED",
                    "Configure the selected server and inspect pending native actions first.",
                ));
            }
            state.invalidate_plan();
            state.authentication_evidence = AuthenticationEvidence::UserConfirmed;
            state.phase = SetupPhase::AuthenticationRequired;
            state.last_notice = Some("User confirmed native-client sign-in. This is user confirmation, not tool-discovery or independent authentication evidence.".into());
            Ok(())
        }
        SetupAction::ImportDiscovery { file } => {
            let raw = storage::read_bytes(Path::new(&file), MAX_DISCOVERY_BYTES)?;
            let report: HostToolDiscovery = serde_json::from_slice(&raw).map_err(|_| err("SETUP_DISCOVERY_JSON", "Expected a strict v1 saved HostToolDiscovery report. No query or network call was run."))?;
            let baseline = inspect_baseline(state, state_path, runner)?;
            if baseline.server != ExistingServer::Same || baseline.sha256 != state.configured_sha256
            {
                return Err(err(
                    "SETUP_BASELINE_CHANGED",
                    "Client configuration changed after setup. Review configuration before importing discovery evidence.",
                ));
            }
            setup::verify_discovery(state, &report, sha256_hex(&raw))
        }
        SetupAction::AcknowledgePending {
            confirmation_sha256,
        } => {
            if state.phase != SetupPhase::CommandPending
                || state
                    .plan
                    .as_ref()
                    .is_none_or(|p| p.confirmation_sha256 != confirmation_sha256)
            {
                return Err(err(
                    "SETUP_CONFIRMATION",
                    "Acknowledging a pending command requires its previously reviewed plan hash after inspecting the native client.",
                ));
            }
            state.invalidate_plan();
            state.phase = SetupPhase::Selected;
            state.configured = false;
            state.configured_sha256 = None;
            state.discovery_sha256 = None;
            state.authentication_evidence = AuthenticationEvidence::None;
            state.last_notice = Some("User acknowledged inspecting the pending native command. Preview the current configuration baseline before any further native action; no rollback was claimed.".into());
            Ok(())
        }
        SetupAction::Cancel {} => {
            state.cancel();
            Ok(())
        }
    }
}
fn emit(result: SetupResult, json: bool) -> Result<i32, Diagnostic> {
    let code = if result.state.phase == SetupPhase::ToolDiscoveryVerified {
        0
    } else {
        1
    };
    let text = if json {
        serde_json::to_string(&result)
            .map_err(|_| err("SETUP_OUTPUT", "Cannot serialize setup result."))?
    } else {
        let mut text = format!(
            "CQLmatic setup: {:?}\nConfigured: {}\nAuthentication evidence: {:?}\n",
            result.state.phase, result.state.configured, result.state.authentication_evidence
        );
        if let Some(notice) = &result.state.last_notice {
            text.push_str(notice);
            text.push('\n');
        }
        if let Some(plan) = &result.state.plan {
            text.push_str("\nReview local changes:\n");
            for change in &plan.changes {
                text.push_str(&format!("- {change}\n"));
            }
            for command in &plan.commands {
                text.push_str(&format!(
                    "- Command executable: {}\n  Exact argv JSON: {}\n",
                    command.program,
                    serde_json::to_string(&command.args).unwrap_or_default()
                ));
            }
            text.push_str(&format!(
                "Confirmation SHA256: {}\n",
                plan.confirmation_sha256
            ));
        }
        for action in &result.next_actions {
            text.push_str(&format!("- {action}\n"));
        }
        if let Some(handoff) = &result.handoff {
            text.push_str(&format!("\nReady-to-send host agent handoff:\n{handoff}\n"));
        }
        text
    };
    writeln!(io::stdout().lock(), "{text}")
        .map_err(|_| err("SETUP_OUTPUT", "Cannot write setup result."))?;
    Ok(code)
}
fn banner_uses_color(terminal: bool, term: Option<&str>, no_color: bool) -> bool {
    // IsTerminal and TERM do not establish Windows console ANSI processing.
    cfg!(not(windows))
        && terminal
        && !no_color
        && term.is_some_and(|term| {
            matches!(
                term,
                "ansi" | "linux" | "xterm" | "screen" | "tmux" | "rxvt"
            ) || ["xterm-", "screen-", "tmux-", "rxvt-"]
                .iter()
                .any(|prefix| term.starts_with(prefix))
        })
}
fn write_setup_banner(output: &mut impl Write, color: bool) -> Result<(), Diagnostic> {
    (if color {
        write!(output, "\x1b[31m{SETUP_WORDMARK}\x1b[0m")
    } else {
        write!(output, "{SETUP_WORDMARK}")
    })
    .and_then(|()| writeln!(output, "CQLmatic v{}\n", env!("CARGO_PKG_VERSION")))
    .map_err(|_| err("SETUP_OUTPUT", "Cannot write setup banner."))
}
fn interactive(state_path: &Path) -> Result<i32, Diagnostic> {
    write_setup_banner(
        &mut io::stdout().lock(),
        banner_uses_color(
            io::stdout().is_terminal(),
            env::var("TERM").ok().as_deref(),
            env::var_os("NO_COLOR").is_some(),
        ),
    )?;
    println!(
        "CQLmatic native-client setup. OAuth, credentials and MCP stay in your chosen client.\nSources reviewed {}: https://developers.openai.com/codex/mcp/ and https://code.visualstudio.com/docs/copilot/customization/mcp-servers\nCIMD metadata and CrowdStrike allowlisting can change; confirm the exact current identity. No DCR fallback.\n",
        SOURCE_REVIEWED
    );
    let (tx, rx) = mpsc::channel();
    let (read_tx, read_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        // Read exactly once per prompt. Do not compete with native-client consent stdin.
        while read_rx.recv().is_ok() {
            line.clear();
            match io::stdin().read_line(&mut line) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(None);
                    break;
                }
                Ok(_) => {
                    if tx.send(Some(line.trim().to_owned())).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let rx = TerminalInput {
        requests: read_tx,
        lines: rx,
    };
    loop {
        let state = load_state(state_path)?;
        emit(setup::result(state.clone(), None), false)?;
        println!(
            "\nChoose: 1 detect clients, 2 choose client/cloud, 3 preview configuration, 4 preview login, 5 apply reviewed plan, 6 confirm sign-in, 7 import saved host report, 8 quit (progress saved), 9 acknowledge inspected pending command"
        );
        let Some(choice) = prompt(&rx, "> ")? else {
            with_state(state_path, |s, _, _| {
                s.cancel();
                Ok(())
            })?;
            return Ok(1);
        };
        let action = match choice.as_str() {
            "1" => SetupAction::Detect {},
            "2" => {
                let clients = with_state(state_path, |s, r, w| {
                    perform(s, SetupAction::Detect {}, state_path, r, w, true)
                })?;
                for client in &clients.clients {
                    println!(
                        "{}: {} ({})",
                        client.client.label(),
                        client.executable,
                        client.version
                    );
                }
                let Some(client) = prompt(&rx, "Client [codex/vs-code]: ")? else {
                    continue;
                };
                let client = match client.as_str() {
                    "codex" => ClientKind::Codex,
                    "vs-code" => ClientKind::VsCode,
                    _ => {
                        println!("Choose codex or vs-code.");
                        continue;
                    }
                };
                if !clients.clients.iter().any(|c| c.client == client) {
                    println!(
                        "Install {} from {} and resume this wizard. No installer is run.",
                        client.label(),
                        client.install_url()
                    );
                }
                let Some(cloud) = prompt(&rx, "Falcon cloud [us1/us2/eu1]: ")? else {
                    continue;
                };
                let cloud = match cloud.as_str() {
                    "us1" => Cloud::Us1,
                    "us2" => Cloud::Us2,
                    "eu1" => Cloud::Eu1,
                    _ => {
                        println!("Choose us1, us2 or eu1.");
                        continue;
                    }
                };
                let Some(server_name) = prompt(&rx, "Server name [cs-mcp]: ")? else {
                    continue;
                };
                let server_name = if server_name.is_empty() {
                    "cs-mcp".into()
                } else {
                    server_name
                };
                let Some(mode) = prompt(&rx, "Registration [cimd/manual/api-client]: ")? else {
                    continue;
                };
                let registration = match mode.as_str() {
                    "cimd" => {
                        let url = if client == ClientKind::VsCode {
                            "https://vscode.dev/oauth/client-metadata.json".into()
                        } else {
                            println!(
                                "Codex's metadata URL depends on client version/server capabilities. Inspect the exact identity in your native client; there is no assumed universal client ID."
                            );
                            let Some(url) = prompt(&rx, "Exact current Codex metadata URL: ")?
                            else {
                                continue;
                            };
                            url
                        };
                        println!(
                            "Metadata identity: {url}\nCanonical endpoint: {}\nConfirm current CrowdStrike CIMD allowlisting and your CrowdStrike MCP role in Falcon. This wizard cannot detect your tenant allowlist.",
                            cloud.endpoint()
                        );
                        let Some(confirm) =
                            prompt(&rx, "Type allowlisted after checking that exact identity: ")?
                        else {
                            continue;
                        };
                        RegistrationMode::Cimd {
                            metadata_url: url,
                            allowlist_confirmed: confirm == "allowlisted",
                        }
                    }
                    "manual" => {
                        println!(
                            "In Falcon MCP Client Management, register your native client using its exact callback URI. Enter only its generated client ID here; optional secrets belong solely in the native client's protected UI."
                        );
                        let Some(client_id) = prompt(&rx, "Generated manual MCP client ID: ")?
                        else {
                            continue;
                        };
                        let Some(callback_url) = prompt(&rx, "Exact native-client callback URI: ")?
                        else {
                            continue;
                        };
                        RegistrationMode::Manual {
                            client_id,
                            callback_url,
                        }
                    }
                    "api-client" => {
                        println!(
                            "Advanced service mode requires a native client's supported plugin. Store credentials through its protected UI. This wizard records only a file reference, does not open it, and leaves service setup pending."
                        );
                        let Some(credentials_file) =
                            prompt(&rx, "Absolute protected credential-file reference: ")?
                        else {
                            continue;
                        };
                        RegistrationMode::ApiClient { credentials_file }
                    }
                    _ => {
                        println!("Choose cimd, manual or api-client.");
                        continue;
                    }
                };
                SetupAction::Select {
                    selection: Selection {
                        client,
                        cloud,
                        server_name,
                        registration,
                    },
                }
            }
            "3" => SetupAction::Preview {
                operation: PlanOperation::Configure,
            },
            "4" => SetupAction::Preview {
                operation: PlanOperation::Authenticate,
            },
            "5" => {
                let Some(plan) = &state.plan else {
                    println!("Preview a plan first.");
                    continue;
                };
                println!(
                    "Review the displayed exact executable, argv, config target and backup path before continuing."
                );
                let Some(hash) = prompt(
                    &rx,
                    "Paste the displayed confirmation SHA256 to apply (blank cancels): ",
                )?
                else {
                    continue;
                };
                if hash.is_empty() {
                    continue;
                }
                if hash != plan.confirmation_sha256 {
                    println!("Confirmation does not match; no command ran.");
                    continue;
                }
                SetupAction::Apply {
                    confirmation_sha256: hash,
                }
            }
            "6" => SetupAction::ConfirmAuthentication {},
            "7" => {
                let Some(file) = prompt(&rx, "Saved HostToolDiscovery JSON file: ")? else {
                    continue;
                };
                SetupAction::ImportDiscovery { file }
            }
            "8" | "q" | "quit" => {
                return Ok(if state.phase == SetupPhase::ToolDiscoveryVerified {
                    0
                } else {
                    1
                });
            }
            "9" => {
                println!(
                    "Inspect the native client's current configuration and authentication UI first. The previous command may have partially changed them. This acknowledges your review; it does not retry or restore configuration."
                );
                let Some(hash) = prompt(
                    &rx,
                    "Paste the pending plan's confirmation SHA256 after inspection: ",
                )?
                else {
                    continue;
                };
                SetupAction::AcknowledgePending {
                    confirmation_sha256: hash,
                }
            }
            _ => {
                println!("Choose a listed step.");
                continue;
            }
        };
        match with_state(state_path, |s, r, w| {
            perform(s, action, state_path, r, w, true)
        }) {
            Ok(_) => {}
            Err(d) => eprintln!("{}: {}", d.code, d.message),
        }
    }
}
struct TerminalInput {
    requests: mpsc::Sender<()>,
    lines: mpsc::Receiver<Option<String>>,
}
fn prompt(rx: &TerminalInput, label: &str) -> Result<Option<String>, Diagnostic> {
    print!("{label}");
    io::stdout()
        .flush()
        .map_err(|_| err("SETUP_OUTPUT", "Cannot write prompt."))?;
    if rx.requests.send(()).is_err() {
        return Ok(None);
    }
    loop {
        if network::CANCELLED.load(Ordering::Relaxed) {
            return Ok(None);
        }
        match rx.lines.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => return Ok(line),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    #[test]
    fn setup_banner_is_compact_ascii_with_plain_version() {
        let mut plain = Vec::new();
        write_setup_banner(&mut plain, false).unwrap();
        let plain = String::from_utf8(plain).unwrap();
        assert!(plain.is_ascii());
        assert!(plain.lines().all(|line| line.len() <= 78));
        assert_eq!(SETUP_WORDMARK.lines().count(), 5);
        assert_eq!(
            plain,
            format!(
                "{SETUP_WORDMARK}CQLmatic v{}\n\n",
                env!("CARGO_PKG_VERSION")
            )
        );
        assert!(!plain.contains('\x1b'));
        let mut colored = Vec::new();
        write_setup_banner(&mut colored, true).unwrap();
        assert_eq!(
            String::from_utf8(colored).unwrap(),
            format!(
                "\x1b[31m{SETUP_WORDMARK}\x1b[0mCQLmatic v{}\n\n",
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    #[test]
    fn setup_banner_color_requires_known_terminal_and_no_opt_out() {
        for term in [None, Some(""), Some("dumb"), Some("unknown")] {
            assert!(!banner_uses_color(true, term, false));
        }
        assert!(!banner_uses_color(false, Some("xterm-256color"), false));
        assert!(!banner_uses_color(true, Some("xterm-256color"), true));
        assert_eq!(
            banner_uses_color(true, Some("xterm-256color"), false),
            cfg!(not(windows))
        );
    }

    #[test]
    fn setup_banner_reports_wordmark_and_version_write_errors() {
        struct FailingWriter(usize);
        impl Write for FailingWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.0 == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "synthetic failure",
                    ));
                }
                let written = bytes.len().min(self.0);
                self.0 -= written;
                Ok(written)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        for available in [0, SETUP_WORDMARK.len()] {
            let diagnostic = write_setup_banner(&mut FailingWriter(available), false).unwrap_err();
            assert_eq!(diagnostic.code, "SETUP_OUTPUT");
        }
    }

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = env::temp_dir().join(format!(
                "cqlmatic-setup-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, AtomicOrdering::Relaxed)
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

    struct FakeRunner {
        config: PathBuf,
        state_path: PathBuf,
        observations: Vec<ClientObservation>,
        native_calls: usize,
        fail_after_mutation: bool,
        no_mutation: bool,
        saw_durable_pending: bool,
        commands: Vec<NativeCommand>,
    }
    impl Runner for FakeRunner {
        fn run(&mut self, command: &NativeCommand) -> Result<CommandOutput, Diagnostic> {
            if command.args == ["mcp", "list", "--json"] {
                let config: Value = if self.config.exists() {
                    serde_json::from_slice(&fs::read(&self.config).unwrap()).unwrap()
                } else {
                    json!({"servers": {}})
                };
                let entries: Vec<Value> = config["servers"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(name, entry)| {
                        json!({
                            "name": name,
                            "enabled": true,
                            "transport": {
                                "type": "streamable_http",
                                "url": entry["url"],
                                "http_headers": null,
                                "bearer_token_env_var": null,
                                "env_http_headers": null,
                                "http_headers_helper": null
                            }
                        })
                    })
                    .collect();
                return Ok(CommandOutput {
                    success: true,
                    stdout: serde_json::to_vec(&entries).unwrap(),
                });
            }
            let codex = command.args.starts_with(&["mcp".into(), "add".into()]);
            if !codex {
                assert_eq!(command.args.first().map(String::as_str), Some("--add-mcp"));
            }
            self.native_calls += 1;
            self.commands.push(command.clone());
            self.saw_durable_pending =
                load_state(&self.state_path)?.phase == SetupPhase::CommandPending;
            if !self.no_mutation {
                let mut value: Value = if self.config.exists() {
                    serde_json::from_slice(&fs::read(&self.config).unwrap()).unwrap()
                } else {
                    json!({"servers":{}})
                };
                let entry: Value = if codex {
                    assert_eq!(command.args[3], "--url");
                    let mut entry = json!({
                        "name": command.args[2],
                        "type": "http",
                        "url": command.args[4]
                    });
                    if command.args[5] == "--oauth-client-id" {
                        entry["oauth"] = json!({"clientId": command.args[6]});
                    } else {
                        assert_eq!(&command.args[5..], ["--oauth-client-registration", "cimd"]);
                    }
                    entry
                } else {
                    serde_json::from_str(&command.args[1]).unwrap()
                };
                let name = entry["name"].as_str().unwrap().to_owned();
                let mut entry = entry.as_object().unwrap().clone();
                entry.remove("name");
                value["servers"][&name] = Value::Object(entry);
                fs::write(&self.config, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
            }
            Ok(CommandOutput {
                success: !self.fail_after_mutation,
                stdout: b"synthetic-sensitive-native-output-never-persist".to_vec(),
            })
        }
        fn detect(&mut self) -> Result<Vec<ClientObservation>, Diagnostic> {
            Ok(self.observations.clone())
        }
        fn config_path(&self, _: ClientKind) -> Result<PathBuf, Diagnostic> {
            Ok(self.config.clone())
        }
    }
    struct Harness {
        _temp: Temp,
        path: PathBuf,
        state: SetupState,
        runner: FakeRunner,
    }
    impl Harness {
        fn new() -> Self {
            let temp = Temp::new();
            let directory = temp.0.join("private");
            protect_directory(&directory).unwrap();
            let path = directory.join("state.json");
            let config = temp.0.join("synthetic-mcp.json");
            let executable = temp.0.join("synthetic-client.exe");
            fs::write(&executable, b"synthetic; never executed").unwrap();
            let client = ClientObservation {
                client: ClientKind::VsCode,
                executable: executable.to_string_lossy().into_owned(),
                executable_sha256: sha256_hex(b"synthetic; never executed"),
                version: "synthetic 1".into(),
                supports_http: true,
                supports_cimd: true,
                supports_manual: true,
                supports_login: false,
                capability_sha256: sha256_hex(b"synthetic capability"),
            };
            let mut state = SetupState {
                clients: vec![client.clone()],
                ..SetupState::default()
            };
            state
                .select(Selection {
                    client: ClientKind::VsCode,
                    cloud: Cloud::Us1,
                    server_name: "cs-mcp".into(),
                    registration: RegistrationMode::Cimd {
                        metadata_url: "https://vscode.dev/oauth/client-metadata.json".into(),
                        allowlist_confirmed: true,
                    },
                })
                .unwrap();
            let runner = FakeRunner {
                config,
                state_path: path.clone(),
                observations: vec![client],
                native_calls: 0,
                fail_after_mutation: false,
                no_mutation: false,
                saw_durable_pending: false,
                commands: Vec::new(),
            };
            Self {
                _temp: temp,
                path,
                state,
                runner,
            }
        }
        fn codex(registration: RegistrationMode) -> Self {
            let mut h = Self::new();
            h.state.clients[0].client = ClientKind::Codex;
            h.state.clients[0].supports_login = true;
            h.runner.observations = h.state.clients.clone();
            h.state
                .select(Selection {
                    client: ClientKind::Codex,
                    cloud: Cloud::Us1,
                    server_name: "cs-mcp".into(),
                    registration,
                })
                .unwrap();
            h
        }
        fn action(&mut self, action: SetupAction) -> Result<(), Diagnostic> {
            let writer = storage::Writer::acquire(&self.path)?;
            let result = perform(
                &mut self.state,
                action,
                &self.path,
                &mut self.runner,
                &writer,
                false,
            );
            save_state(&writer, &self.state)?;
            result
        }
        fn preview(&mut self) -> String {
            self.action(SetupAction::Preview {
                operation: PlanOperation::Configure,
            })
            .unwrap();
            self.state
                .plan
                .as_ref()
                .unwrap()
                .confirmation_sha256
                .clone()
        }
        fn apply(&mut self, hash: String) -> Result<(), Diagnostic> {
            self.action(SetupAction::Apply {
                confirmation_sha256: hash,
            })
        }
        fn apply_in_terminal(&mut self, hash: String) -> Result<(), Diagnostic> {
            let writer = storage::Writer::acquire(&self.path)?;
            let result = perform(
                &mut self.state,
                SetupAction::Apply {
                    confirmation_sha256: hash,
                },
                &self.path,
                &mut self.runner,
                &writer,
                true,
            );
            save_state(&writer, &self.state)?;
            result
        }
    }
    fn manual_registration(client_id: &str) -> RegistrationMode {
        RegistrationMode::Manual {
            client_id: client_id.into(),
            callback_url: "http://127.0.0.1:1234/callback".into(),
        }
    }
    fn codex_cimd_registration() -> RegistrationMode {
        RegistrationMode::Cimd {
            metadata_url: "https://chatgpt.com/oauth/codex/synthetic/client.json".into(),
            allowlist_confirmed: true,
        }
    }
    #[test]
    fn setup_codex_preexisting_manual_registration_is_not_inferred_from_url() {
        for existing_id in [Some("synthetic-a"), Some("synthetic-b"), None] {
            let mut h = Harness::codex(manual_registration("synthetic-b"));
            let mut config = json!({"servers":{
                "cs-mcp":{"type":"http","url":Cloud::Us1.endpoint()},
                "unrelated":{"type":"http","url":"https://synthetic.example/mcp","headers":{"Authorization":"synthetic-secret"}}
            }});
            if let Some(client_id) = existing_id {
                config["servers"]["cs-mcp"]["oauth"] = json!({"clientId":client_id});
            }
            let original = serde_json::to_vec(&config).unwrap();
            fs::write(&h.runner.config, &original).unwrap();
            // Legacy configured state without provenance must also fail closed.
            h.state.configured = true;
            h.state.configured_sha256 = config_hash(&h.runner.config).unwrap();
            let diagnostic = h
                .action(SetupAction::Preview {
                    operation: PlanOperation::Configure,
                })
                .unwrap_err();
            assert_eq!(diagnostic.code, "SETUP_REGISTRATION_UNINSPECTABLE");
            assert!(diagnostic.message.contains("unused server name"));
            assert!(!diagnostic.message.contains("synthetic-secret"));
            assert_eq!(fs::read(&h.runner.config).unwrap(), original);
            assert_eq!(h.runner.native_calls, 0);
            assert!(h.state.plan.is_none());
            assert!(
                !fs::read_to_string(&h.path)
                    .unwrap()
                    .contains("synthetic-secret")
            );
        }
    }
    #[test]
    fn setup_codex_cimd_requires_capability_and_registration_provenance() {
        let mut h = Harness::codex(codex_cimd_registration());
        h.runner.observations[0].supports_cimd = false;
        assert_eq!(
            h.action(SetupAction::Preview {
                operation: PlanOperation::Configure,
            })
            .unwrap_err()
            .code,
            "SETUP_CAPABILITY"
        );
        assert_eq!(h.runner.native_calls, 0);
        assert!(!h.runner.config.exists());

        h.runner.observations[0].supports_cimd = true;
        fs::write(
            &h.runner.config,
            serde_json::to_vec(&json!({"servers":{
                "cs-mcp":{"type":"http","url":Cloud::Us1.endpoint()}
            }}))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            h.action(SetupAction::Preview {
                operation: PlanOperation::Configure,
            })
            .unwrap_err()
            .code,
            "SETUP_REGISTRATION_UNINSPECTABLE"
        );
        assert_eq!(h.runner.native_calls, 0);
    }
    #[test]
    fn setup_codex_fresh_add_receipt_allows_resume_and_bound_discovery() {
        for registration in [
            manual_registration("synthetic-b"),
            codex_cimd_registration(),
        ] {
            let mut h = Harness::codex(registration);
            let original = json!({"servers":{
                "unrelated":{"type":"http","url":"https://synthetic.example/mcp","headers":{"Authorization":"synthetic-secret"}}
            }});
            fs::write(&h.runner.config, serde_json::to_vec(&original).unwrap()).unwrap();
            let hash = h.preview();
            let reviewed = h.state.plan.as_ref().unwrap().commands.clone();
            h.apply_in_terminal(hash).unwrap();
            assert!(h.runner.saw_durable_pending);
            assert_eq!(h.runner.commands, reviewed);
            assert!(h.state.configured);
            assert_eq!(h.state.phase, SetupPhase::AuthenticationRequired);
            assert_eq!(
                h.state.authentication_evidence,
                AuthenticationEvidence::None
            );
            let receipt = registration_receipt_path(&h.path).unwrap();
            let stored = fs::read_to_string(receipt).unwrap();
            assert!(!stored.contains("synthetic-secret"));
            assert!(!stored.contains("synthetic-sensitive-native-output"));
            let config: Value =
                serde_json::from_slice(&fs::read(&h.runner.config).unwrap()).unwrap();
            assert_eq!(
                config["servers"]["unrelated"],
                original["servers"]["unrelated"]
            );
            h.state = load_state(&h.path).unwrap();
            let hash = h.preview();
            assert!(h.state.plan.as_ref().unwrap().commands.is_empty());
            h.apply(hash).unwrap();
            assert_eq!(h.runner.native_calls, 1);

            let report_path = h._temp.0.join("synthetic-discovery.json");
            let report = HostToolDiscovery {
                schema_version: SCHEMA_VERSION,
                client: ClientKind::Codex,
                server_name: "cs-mcp".into(),
                endpoint: Cloud::Us1.endpoint().into(),
                authenticated: true,
                tools: vec![DiscoveryTool {
                    name: SEARCH_TOOL.into(),
                    available: true,
                }],
            };
            fs::write(&report_path, serde_json::to_vec(&report).unwrap()).unwrap();
            h.action(SetupAction::ImportDiscovery {
                file: report_path.to_string_lossy().into_owned(),
            })
            .unwrap();
            assert_eq!(h.state.phase, SetupPhase::ToolDiscoveryVerified);
            assert_eq!(h.runner.native_calls, 1);
        }
    }
    #[test]
    fn setup_codex_registration_receipt_does_not_survive_id_change_or_config_drift() {
        let mut h = Harness::codex(manual_registration("synthetic-a"));
        let hash = h.preview();
        h.apply_in_terminal(hash).unwrap();
        let mut selection = h.state.selection.clone().unwrap();
        selection.registration = manual_registration("synthetic-b");
        h.action(SetupAction::Select { selection }).unwrap();
        assert_eq!(
            h.action(SetupAction::Preview {
                operation: PlanOperation::Configure,
            })
            .unwrap_err()
            .code,
            "SETUP_REGISTRATION_UNINSPECTABLE"
        );
        assert!(!h.state.configured);
        assert_eq!(h.runner.native_calls, 1);

        let mut h = Harness::codex(codex_cimd_registration());
        let hash = h.preview();
        h.apply_in_terminal(hash).unwrap();
        let mut config: Value =
            serde_json::from_slice(&fs::read(&h.runner.config).unwrap()).unwrap();
        config["servers"]["cs-mcp"]["oauth"] = json!({"clientId":"synthetic-changed"});
        fs::write(&h.runner.config, serde_json::to_vec(&config).unwrap()).unwrap();
        assert_eq!(
            h.action(SetupAction::Preview {
                operation: PlanOperation::Configure,
            })
            .unwrap_err()
            .code,
            "SETUP_REGISTRATION_UNINSPECTABLE"
        );
        assert_eq!(h.runner.native_calls, 1);
    }
    #[test]
    fn setup_codex_manual_reauthentication_is_guided_and_cimd_is_explicit() {
        let mut h = Harness::codex(manual_registration("synthetic-a"));
        let hash = h.preview();
        h.apply_in_terminal(hash).unwrap();
        assert_eq!(
            h.action(SetupAction::Preview {
                operation: PlanOperation::Authenticate,
            })
            .unwrap_err()
            .code,
            "SETUP_CAPABILITY"
        );
        assert_eq!(h.runner.native_calls, 1);

        let mut h = Harness::codex(codex_cimd_registration());
        let hash = h.preview();
        h.apply_in_terminal(hash).unwrap();
        h.action(SetupAction::Preview {
            operation: PlanOperation::Authenticate,
        })
        .unwrap();
        assert_eq!(
            h.state.plan.unwrap().commands[0].args,
            [
                "mcp",
                "login",
                "cs-mcp",
                "--oauth-client-registration",
                "cimd"
            ]
        );
        assert_eq!(h.runner.native_calls, 1);
    }
    #[test]
    fn setup_apply_backs_up_exact_bytes_preserves_other_servers_and_stays_unauthenticated() {
        let mut h = Harness::new();
        let original = br#"{"servers":{"unrelated":{"type":"http","url":"https://synthetic.example/mcp","headers":{"Authorization":"synthetic-secret"}}}}"#;
        fs::write(&h.runner.config, original).unwrap();
        let hash = h.preview();
        assert_eq!(h.runner.native_calls, 0);
        assert_eq!(fs::read(&h.runner.config).unwrap(), original);
        let plan = h.state.plan.clone().unwrap();
        assert!(!Path::new(plan.backup_path.as_ref().unwrap()).exists());
        h.apply(hash).unwrap();
        assert!(h.runner.saw_durable_pending);
        assert_eq!(h.runner.native_calls, 1);
        assert_eq!(fs::read(plan.backup_path.unwrap()).unwrap(), original);
        let config: Value = serde_json::from_slice(&fs::read(&h.runner.config).unwrap()).unwrap();
        assert_eq!(
            config["servers"]["unrelated"]["headers"]["Authorization"],
            "synthetic-secret"
        );
        assert_eq!(config["servers"]["cs-mcp"]["url"], Cloud::Us1.endpoint());
        assert_eq!(h.state.phase, SetupPhase::AuthenticationRequired);
        assert!(h.state.configured);
        assert_eq!(
            h.state.authentication_evidence,
            AuthenticationEvidence::None
        );
        let stored = fs::read_to_string(&h.path).unwrap();
        assert!(!stored.contains("synthetic-secret"));
        assert!(!stored.contains("synthetic-sensitive-native-output"));
        assert_eq!(
            load_state(&h.path).unwrap().phase,
            SetupPhase::AuthenticationRequired
        );
    }
    #[test]
    fn setup_repeated_desired_configuration_is_idempotent() {
        let mut h = Harness::new();
        let hash = h.preview();
        h.apply(hash).unwrap();
        let hash = h.preview();
        assert!(h.state.plan.as_ref().unwrap().commands.is_empty());
        h.apply(hash).unwrap();
        assert_eq!(h.runner.native_calls, 1);
        assert_eq!(h.state.phase, SetupPhase::AuthenticationRequired);
    }
    #[test]
    fn setup_stale_hash_changed_executable_and_modified_baseline_prevent_native_mutation() {
        let mut h = Harness::new();
        let hash = h.preview();
        assert_eq!(
            h.apply("0".repeat(64)).unwrap_err().code,
            "SETUP_CONFIRMATION"
        );
        h.runner.observations[0].executable_sha256 = sha256_hex(b"updated executable");
        assert_eq!(h.apply(hash).unwrap_err().code, "SETUP_CLIENT_CHANGED");
        let hash = h.preview();
        fs::write(&h.runner.config, br#"{"servers":{},"changed":true}"#).unwrap();
        assert_eq!(h.apply(hash).unwrap_err().code, "SETUP_BASELINE_CHANGED");
        assert_eq!(h.runner.native_calls, 0);
    }
    #[test]
    fn setup_collision_is_an_error_and_preserves_original_bytes() {
        let mut h = Harness::new();
        let original =
            br#"{"servers":{"cs-mcp":{"type":"http","url":"https://synthetic.example/other"}}}"#;
        fs::write(&h.runner.config, original).unwrap();
        assert_eq!(
            h.action(SetupAction::Preview {
                operation: PlanOperation::Configure
            })
            .unwrap_err()
            .code,
            "SETUP_COLLISION"
        );
        assert_eq!(fs::read(&h.runner.config).unwrap(), original);
        assert_eq!(h.runner.native_calls, 0);
    }
    #[test]
    fn setup_partial_failure_is_durable_pending_and_never_blind_retried() {
        let mut h = Harness::new();
        let original = br#"{"servers":{}}"#;
        fs::write(&h.runner.config, original).unwrap();
        let hash = h.preview();
        let backup = h.state.plan.as_ref().unwrap().backup_path.clone().unwrap();
        h.runner.fail_after_mutation = true;
        assert_eq!(
            h.apply(hash.clone()).unwrap_err().code,
            "SETUP_NATIVE_FAILED"
        );
        assert_eq!(h.state.phase, SetupPhase::CommandPending);
        assert_eq!(
            load_state(&h.path).unwrap().phase,
            SetupPhase::CommandPending
        );
        assert_ne!(fs::read(&h.runner.config).unwrap(), original);
        assert_eq!(fs::read(backup).unwrap(), original);
        assert_eq!(h.apply(hash.clone()).unwrap_err().code, "SETUP_PENDING");
        assert_eq!(h.runner.native_calls, 1);
        h.action(SetupAction::AcknowledgePending {
            confirmation_sha256: hash,
        })
        .unwrap();
        assert_eq!(h.state.phase, SetupPhase::Selected);
        let hash = h.preview();
        assert!(h.state.plan.as_ref().unwrap().commands.is_empty());
        h.apply(hash).unwrap();
        assert_eq!(h.runner.native_calls, 1);
    }
    #[test]
    fn setup_native_exit_zero_without_config_is_not_success() {
        let mut h = Harness::new();
        h.runner.no_mutation = true;
        let hash = h.preview();
        assert_eq!(h.apply(hash).unwrap_err().code, "SETUP_POSTCONDITION");
        assert!(!h.state.configured);
        assert_eq!(h.state.phase, SetupPhase::CommandPending);
    }
    #[test]
    fn setup_user_confirmation_is_distinct_from_host_evidence() {
        let mut h = Harness::new();
        let hash = h.preview();
        h.apply(hash).unwrap();
        h.action(SetupAction::ConfirmAuthentication {}).unwrap();
        assert_eq!(h.state.phase, SetupPhase::AuthenticationRequired);
        assert_eq!(
            h.state.authentication_evidence,
            AuthenticationEvidence::UserConfirmed
        );
        assert!(h.state.discovery_sha256.is_none());
    }
    #[cfg(windows)]
    #[test]
    fn setup_windows_native_shims_preserve_exact_json_argv() {
        let temp = Temp::new();
        let shim_directory = temp.0.join("native client");
        fs::create_dir(&shim_directory).unwrap();
        let payload =
            r#"{"name":"synthetic-mcp","type":"http","url":"https://mcp.us-2.crowdstrike.com/v1"}"#;
        let script = shim_directory.join("synthetic.ps1");
        fs::write(&script, "[Console]::Write($args[1])\n").unwrap();
        let output = run_native(&NativeCommand {
            program: script.to_string_lossy().into_owned(),
            args: vec!["--add-mcp".into(), payload.into()],
            interactive: false,
            timeout_seconds: 30,
        })
        .unwrap();
        assert!(output.success);
        assert_eq!(output.stdout, payload.as_bytes());
        let script = shim_directory.join("synthetic.cmd");
        fs::write(&script, "@echo off\r\n\"%SystemRoot%\\System32\\WindowsPowerShell\\v1.0\\powershell.exe\" -NoLogo -NoProfile -NonInteractive -File \"%~dp0synthetic.ps1\" %*\r\n").unwrap();
        let output = run_native(&NativeCommand {
            program: script.to_string_lossy().into_owned(),
            args: vec!["--add-mcp".into(), payload.into()],
            interactive: false,
            timeout_seconds: 30,
        })
        .unwrap();
        assert!(output.success);
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), payload);
    }
    #[test]
    fn setup_tampered_state_fails_closed_before_native_operation() {
        let mut h = Harness::new();
        h.preview();
        let mut stored: Value = serde_json::from_slice(&fs::read(&h.path).unwrap()).unwrap();
        stored["state"]["configured"] = json!(true);
        fs::write(&h.path, serde_json::to_vec(&stored).unwrap()).unwrap();
        assert_eq!(
            load_state(&h.path).unwrap_err().code,
            "SETUP_STATE_INTEGRITY"
        );
        assert_eq!(h.runner.native_calls, 0);
    }
    #[cfg(unix)]
    #[test]
    fn setup_refuses_shared_existing_directory_and_linked_paths() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = Temp::new();
        fs::set_permissions(&temp.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            protect_directory(&temp.0).unwrap_err().code,
            "SETUP_PROTECTION"
        );
        assert_eq!(
            fs::metadata(&temp.0).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let target = temp.0.join("target");
        protect_directory(&target).unwrap();
        let linked = temp.0.join("linked");
        symlink(&target, &linked).unwrap();
        assert_eq!(protect_directory(&linked).unwrap_err().code, "SETUP_LINK");
    }
}
