//! Terminal and local-I/O adapter for setup; native clients own OAuth and MCP.
use clap::Args;
use cqlmatic::{
    contracts::Diagnostic,
    mcp, network,
    setup::{self, *},
    sha256_hex, storage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

const MAX_CONFIG_BYTES: u64 = 8 * 1024 * 1024;
const MAX_EXECUTABLE_BYTES: u64 = 1024 * 1024 * 1024;
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
    std::path::absolute(path).map_err(|_| err("SETUP_PATH", "Cannot resolve local path."))
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
#[cfg(unix)]
fn current_unix_uid() -> Result<u32, Diagnostic> {
    let output = Command::new("/usr/bin/id")
        .arg("-u")
        .env_clear()
        .env("LC_ALL", "C")
        .output()
        .map_err(|_| private_directory_error())?;
    if !output.status.success() {
        return Err(private_directory_error());
    }
    std::str::from_utf8(&output.stdout)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .ok_or_else(private_directory_error)
}

#[cfg(unix)]
fn check_unix_ancestors(path: &Path, owner: u32) -> Result<(), Diagnostic> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    for ancestor in path.ancestors().skip(1) {
        let meta = match fs::metadata(ancestor) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return Err(private_directory_error()),
        };
        let mode = meta.permissions().mode();
        if (meta.uid() != 0 && meta.uid() != owner)
            || (mode & 0o022 != 0
                && !(mode & 0o1000 != 0 && (meta.uid() == 0 || meta.uid() == owner)))
        {
            return Err(private_directory_error());
        }
        #[cfg(target_os = "macos")]
        check_macos_acl(ancestor)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn check_macos_acl(path: &Path) -> Result<(), Diagnostic> {
    use std::{
        ffi::{CString, c_char, c_int, c_void},
        os::unix::ffi::OsStrExt,
    };
    unsafe extern "C" {
        fn acl_get_file(path: *const c_char, kind: c_int) -> *mut c_void;
        fn acl_get_entry(acl: *mut c_void, id: c_int, entry: *mut *mut c_void) -> c_int;
        fn acl_get_tag_type(entry: *mut c_void, tag: *mut c_int) -> c_int;
        fn acl_valid(acl: *mut c_void) -> c_int;
        fn acl_free(acl: *mut c_void) -> c_int;
    }
    let name = CString::new(path.as_os_str().as_bytes()).map_err(|_| private_directory_error())?;
    // Darwin returns ENOENT for no ACL; other read errors must fail closed.
    let acl = unsafe { acl_get_file(name.as_ptr(), 0x100) };
    if acl.is_null() {
        return if io::Error::last_os_error().raw_os_error() == Some(2) && fs::metadata(path).is_ok()
        {
            Ok(())
        } else {
            Err(private_directory_error())
        };
    }
    let result = (|| {
        if unsafe { acl_valid(acl) } != 0 {
            return Err(private_directory_error());
        }
        // Darwin returns EINVAL after the last indexed entry and caps ACLs at 128 entries.
        for index in 0..=128 {
            let mut entry = std::ptr::null_mut();
            if unsafe { acl_get_entry(acl, index, &mut entry) } != 0 {
                return if io::Error::last_os_error().raw_os_error() == Some(22) {
                    Ok(())
                } else {
                    Err(private_directory_error())
                };
            }
            let mut tag = 0;
            if unsafe { acl_get_tag_type(entry, &mut tag) } != 0 || tag != 2 {
                return Err(private_directory_error());
            }
        }
        Err(private_directory_error())
    })();
    if unsafe { acl_free(acl) } != 0 {
        return Err(private_directory_error());
    }
    result
}

/// Create private storage or verify protection without changing an existing directory.
pub(crate) fn protect_directory(path: &Path) -> Result<(), Diagnostic> {
    private_directory(path, true)
}

/// Verify existing private storage without creating missing directories.
pub(crate) fn verify_directory(path: &Path) -> Result<(), Diagnostic> {
    private_directory(path, false)
}

fn private_directory(path: &Path, create: bool) -> Result<(), Diagnostic> {
    reject_links(path)?;
    let created = match fs::symlink_metadata(path) {
        Ok(_) => false,
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => true,
        Err(_) => {
            return Err(err(
                "SETUP_STORAGE",
                "Cannot inspect private setup directory.",
            ));
        }
    };
    #[cfg(unix)]
    let owner = current_unix_uid()?;
    #[cfg(unix)]
    check_unix_ancestors(path, owner)?;
    #[cfg(unix)]
    if created {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(path)
            .map_err(|_| err("SETUP_STORAGE", "Cannot create private setup directory."))?;
    }
    #[cfg(not(windows))]
    if !path.is_dir() {
        return Err(err(
            "SETUP_STORAGE",
            "Private setup storage must be a directory.",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = fs::metadata(path).map_err(|_| {
            err(
                "SETUP_PROTECTION",
                "Cannot inspect setup directory protection.",
            )
        })?;
        if meta.uid() != owner || meta.permissions().mode() & 0o077 != 0 {
            return Err(private_directory_error());
        }
        #[cfg(target_os = "macos")]
        check_macos_acl(path)?;
        check_unix_ancestors(path, owner)?;
    }
    #[cfg(windows)]
    check_windows_acl(
        path,
        if created {
            "create-directory"
        } else {
            "directory"
        },
    )?;
    #[cfg(windows)]
    if !path.is_dir() {
        return Err(err(
            "SETUP_STORAGE",
            "Private setup storage must be a directory.",
        ));
    }
    reject_links(path)?;
    Ok(())
}
#[cfg(unix)]
fn private_directory_error() -> Diagnostic {
    err(
        "SETUP_PROTECTION",
        "Setup storage or its parent path is accessible or replaceable by another user. Choose a fresh --state path under trusted private ancestry. Setup does not change permissions on existing arbitrary directories.",
    )
}
#[cfg(windows)]
const WINDOWS_ACL_CHECK: &str = r#"
$ErrorActionPreference='Stop'
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
$p=$env:CQLMATIC_SETUP_PATH
$mode=$env:CQLMATIC_SETUP_ACL_MODE
$sid=[Security.Principal.WindowsIdentity]::GetCurrent().User
function Fail($at,$who,$right,$reason){
  @{path=[string]$at;sid=[string]$who;right=[string]$right;reason=[string]$reason} | ConvertTo-Json -Compress
  exit 3
}
$trusted=@($sid.Value,'S-1-5-18','S-1-5-32-544','S-1-3-0')
try {$trusted+=([Security.Principal.NTAccount]::new('NT SERVICE','TrustedInstaller')).Translate([Security.Principal.SecurityIdentifier]).Value} catch {}
$danger=[Security.AccessControl.FileSystemRights]'Delete,DeleteSubdirectoriesAndFiles,ChangePermissions,TakeOwnership'
function Check-Ancestors {
  for($d=[IO.Directory]::GetParent($p); $null -ne $d; $d=$d.Parent){
    try {$attributes=[IO.File]::GetAttributes($d.FullName)} catch {
      $e=$_.Exception; while($null -ne $e.InnerException){$e=$e.InnerException}
      if($e -is [IO.FileNotFoundException] -or $e -is [IO.DirectoryNotFoundException]){continue}
      Fail $d.FullName 'unknown' 'AclInspection' 'inspect'
    }
    if(($attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0){Fail $d.FullName 'unknown' 'ReparsePoint' 'reparse-point'}
    if(($attributes -band [IO.FileAttributes]::Directory) -eq 0){Fail $d.FullName 'unknown' 'Directory' 'not-directory'}
    try {$ac=$d.GetAccessControl()} catch {Fail $d.FullName 'unknown' 'AclInspection' 'inspect'}
    $owner=$ac.GetOwner([Security.Principal.SecurityIdentifier]).Value
    if($trusted -notcontains $owner){Fail $d.FullName $owner 'Owner' 'owner'}
    $rules=$ac.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier])
    if(@($rules).Count -eq 0){Fail $d.FullName 'unknown' 'AccessRules' 'empty-acl'}
    foreach($r in $rules){
      $rights=[Security.AccessControl.FileSystemRights]($r.FileSystemRights -band $danger)
      if($r.AccessControlType -eq 'Allow' -and ($r.PropagationFlags -band [Security.AccessControl.PropagationFlags]::InheritOnly) -eq 0 -and $trusted -notcontains $r.IdentityReference.Value -and $rights -ne 0){Fail $d.FullName $r.IdentityReference.Value $rights.ToString() 'allow'}
    }
  }
}
function Check-Private {
  try {
    $attributes=[IO.File]::GetAttributes($p)
    if(($attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0){Fail $p 'unknown' 'ReparsePoint' 'reparse-point'}
    if($mode -eq 'file'){
      if(($attributes -band [IO.FileAttributes]::Directory) -ne 0){Fail $p 'unknown' 'RegularFile' 'not-file'}
      $a=[IO.File]::GetAccessControl($p)
    } else {
      if(($attributes -band [IO.FileAttributes]::Directory) -eq 0){Fail $p 'unknown' 'Directory' 'not-directory'}
      $a=[IO.Directory]::GetAccessControl($p)
    }
    $owner=$a.GetOwner([Security.Principal.SecurityIdentifier]).Value
    if($owner -ne $sid.Value){Fail $p $owner 'Owner' 'owner'}
    $self=$false
    foreach($r in $a.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier])){
      if($r.AccessControlType -eq 'Allow'){
        if($r.IdentityReference.Value -ne $sid.Value -and $r.IdentityReference.Value -ne 'S-1-5-18'){Fail $p $r.IdentityReference.Value $r.FileSystemRights.ToString() 'allow'}
        if($r.IdentityReference.Value -eq $sid.Value){$self=$true}
      }
    }
    if(-not $self){Fail $p $sid.Value 'AccessRules' 'missing-self'}
  } catch {Fail $p 'unknown' 'AclInspection' 'inspect'}
}
if($mode -ne 'file'){
  Check-Ancestors
  if($mode -eq 'create-directory'){
    try {
      $a=New-Object System.Security.AccessControl.DirectorySecurity
      $a.SetOwner($sid)
      $a.SetAccessRuleProtection($true,$false)
      $inherit=[Security.AccessControl.InheritanceFlags]'ContainerInherit,ObjectInherit'
      $prop=[Security.AccessControl.PropagationFlags]::None
      $allow=[Security.AccessControl.AccessControlType]::Allow
      $a.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid,'FullControl',$inherit,$prop,$allow))
      $a.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new('S-1-5-18'),'FullControl',$inherit,$prop,$allow))
      [IO.Directory]::CreateDirectory($p,$a) | Out-Null
    } catch {Fail $p 'unknown' 'CreateDirectory' 'create'}
  }
}
Check-Private
if($mode -ne 'file'){Check-Ancestors}
"#;
#[cfg(any(windows, test))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowsAclFailure {
    path: String,
    sid: String,
    right: String,
    reason: String,
}
#[cfg(any(windows, test))]
fn windows_acl_failure(output: &[u8]) -> Diagnostic {
    let failure = serde_json::from_slice::<WindowsAclFailure>(output)
        .ok()
        .filter(|failure| {
            !failure.path.is_empty()
                && failure.path.len() <= 16 * 1024
                && [&failure.sid, &failure.right, &failure.reason]
                    .into_iter()
                    .all(|value| {
                        !value.is_empty()
                            && value.len() <= 256
                            && !value.chars().any(char::is_control)
                    })
        });
    match failure {
        Some(failure) => err(
            "SETUP_PROTECTION",
            format!(
                "Unsafe Windows ACL at {:?}: principal {}; right {}; check {}. Choose a fresh --state path under trusted private ancestry. Setup does not change existing directory permissions.",
                failure.path, failure.sid, failure.right, failure.reason
            ),
        ),
        None => err(
            "SETUP_PROTECTION",
            "Windows ACL verification failed without a bounded diagnostic. Choose a fresh --state path under trusted private ancestry; no unsafe path is cleaned up automatically.",
        ),
    }
}
#[cfg(windows)]
fn check_windows_acl(path: &Path, mode: &str) -> Result<(), Diagnostic> {
    let mut command = powershell()?;
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            WINDOWS_ACL_CHECK,
        ])
        .env("CQLMATIC_SETUP_PATH", path)
        .env("CQLMATIC_SETUP_ACL_MODE", mode);
    let output = run_bounded_command(command, false, 20, 32 * 1024).map_err(|_| err(
        "SETUP_PROTECTION",
        "The fixed Windows ACL helper timed out, exceeded its output budget, or could not be read. No unsafe path is cleaned up automatically.",
    ))?;
    if output.success && output.stdout.is_empty() {
        Ok(())
    } else {
        Err(windows_acl_failure(&output.stdout))
    }
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
    let path = std::path::absolute(path)
        .map_err(|_| err("SETUP_PROTECTION", "Cannot resolve protected path."))?;
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) => {
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
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(err("SETUP_PROTECTION", "Cannot inspect protected path.")),
        }
    }
    Ok(())
}

/// Missing files are distinguished from unsafe or inaccessible protected paths.
pub(crate) fn private_input_exists(path: &Path) -> Result<bool, Diagnostic> {
    reject_links(path)?;
    match fs::symlink_metadata(path) {
        Ok(_) => {
            verify_directory(parent(path)?)?;
            verify_private_file(path)?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(err("SETUP_PROTECTION", "Cannot inspect protected input.")),
    }
}
fn load_state(path: &Path) -> Result<SetupState, Diagnostic> {
    reject_links(path)?;
    if !path.exists() {
        return Ok(SetupState::default());
    }
    verify_directory(parent(path)?)?;
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
    protect_directory(parent(path)?)?;
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

#[derive(Debug)]
struct CommandOutput {
    success: bool,
    stdout: Vec<u8>,
}
trait Runner {
    fn run(&mut self, command: &NativeCommand) -> Result<CommandOutput, Diagnostic>;
    fn detect(
        &mut self,
        selected: Option<ClientKind>,
    ) -> Result<Vec<ClientObservation>, Diagnostic>;
    fn config_path(&self, client: ClientKind) -> Result<PathBuf, Diagnostic>;
}
struct NativeRunner;
impl Runner for NativeRunner {
    fn run(&mut self, command: &NativeCommand) -> Result<CommandOutput, Diagnostic> {
        run_native(command)
    }
    fn detect(
        &mut self,
        selected: Option<ClientKind>,
    ) -> Result<Vec<ClientObservation>, Diagnostic> {
        detect_clients(self, selected, discover_executable)
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
    let command = if path
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
    run_bounded_command(
        command,
        native.interactive,
        native.timeout_seconds,
        MAX_COMMAND_BYTES,
    )
}
fn run_bounded_command(
    mut command: Command,
    interactive: bool,
    timeout_seconds: u32,
    max_output_bytes: usize,
) -> Result<CommandOutput, Diagnostic> {
    command.stdin(if interactive {
        Stdio::inherit()
    } else {
        Stdio::null()
    });
    if interactive {
        command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    } else {
        command.stdout(Stdio::piped()).stderr(Stdio::null());
    }
    let mut child = command.spawn().map_err(|_| err("SETUP_NATIVE_START", "Cannot start the reviewed native-client command. No output or credentials were captured in setup state."))?;
    let output_thread = child.stdout.take().map(|stdout| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout
                .take(max_output_bytes as u64 + 1)
                .read_to_end(&mut bytes);
            (result.is_ok(), bytes)
        })
    });
    let deadline = Instant::now() + Duration::from_secs(u64::from(timeout_seconds));
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
        if !ok || bytes.len() > max_output_bytes {
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
        ClientKind::ClaudeCode => &["claude.exe", "claude.cmd", "claude.ps1"],
        _ => return None,
    };
    #[cfg(not(windows))]
    let names: &[&str] = match client {
        ClientKind::Codex => &["codex"],
        ClientKind::VsCode => &["code"],
        ClientKind::ClaudeCode => &["claude"],
        _ => return None,
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
#[derive(Debug, PartialEq)]
struct ExecutableFingerprint {
    identity: (u64, u64),
    len: u64,
    modified: SystemTime,
    #[cfg(unix)]
    changed: (i64, i64),
    #[cfg(windows)]
    attributes: u32,
}
fn executable_error() -> Diagnostic {
    err(
        "SETUP_EXECUTABLE_CHANGED",
        "Discovered executable must remain the same regular file throughout capability detection and fingerprinting. Inspect the client and detect again.",
    )
}
#[cfg(windows)]
#[allow(unsafe_code)]
fn windows_file_identity(file: &fs::File) -> Result<(u64, u64), Diagnostic> {
    use std::{ffi::c_void, os::windows::io::AsRawHandle};
    #[repr(C)]
    struct FileInformation {
        attributes: u32,
        creation_time: [u32; 2],
        access_time: [u32; 2],
        write_time: [u32; 2],
        volume: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(handle: *mut c_void, info: *mut FileInformation) -> i32;
    }
    let mut info = std::mem::MaybeUninit::<FileInformation>::uninit();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(executable_error());
    }
    // The successful Windows call initialized the complete fixed-layout structure.
    let info = unsafe { info.assume_init() };
    Ok((
        u64::from(info.volume),
        (u64::from(info.index_high) << 32) | u64::from(info.index_low),
    ))
}
fn executable_fingerprint(file: &fs::File) -> Result<ExecutableFingerprint, Diagnostic> {
    let meta = file.metadata().map_err(|_| executable_error())?;
    if !meta.is_file() {
        return Err(executable_error());
    }
    if meta.len() > MAX_EXECUTABLE_BYTES {
        return Err(err(
            "SETUP_EXECUTABLE_SIZE",
            "Discovered executable exceeds the 1 GiB fingerprint byte budget.",
        ));
    }
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    #[cfg(windows)]
    use std::os::windows::fs::MetadataExt;
    #[cfg(windows)]
    if meta.file_attributes() & 0x400 != 0 {
        return Err(executable_error());
    }
    Ok(ExecutableFingerprint {
        #[cfg(unix)]
        identity: (meta.dev(), meta.ino()),
        #[cfg(windows)]
        identity: windows_file_identity(file)?,
        len: meta.len(),
        modified: meta.modified().map_err(|_| executable_error())?,
        #[cfg(unix)]
        changed: (meta.ctime(), meta.ctime_nsec()),
        #[cfg(windows)]
        attributes: meta.file_attributes(),
    })
}
fn open_executable(path: &Path) -> Result<(fs::File, ExecutableFingerprint), Diagnostic> {
    let meta = fs::symlink_metadata(path).map_err(|_| executable_error())?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(executable_error());
    }
    if meta.len() > MAX_EXECUTABLE_BYTES {
        return Err(err(
            "SETUP_EXECUTABLE_SIZE",
            "Discovered executable exceeds the 1 GiB fingerprint byte budget.",
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
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        if meta.file_attributes() & 0x400 != 0 {
            return Err(executable_error());
        }
        options.share_mode(1).custom_flags(0x0020_0000); // Read sharing only; open reparse point itself.
    }
    let file = options.open(path).map_err(|_| executable_error())?;
    let fingerprint = executable_fingerprint(&file)?;
    if meta.len() != fingerprint.len || meta.modified().ok() != Some(fingerprint.modified) {
        return Err(executable_error());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (meta.dev(), meta.ino()) != fingerprint.identity
            || (meta.ctime(), meta.ctime_nsec()) != fingerprint.changed
        {
            return Err(executable_error());
        }
    }
    Ok((file, fingerprint))
}
fn hash_executable_bytes(reader: &mut impl Read, limit: u64) -> Result<(String, u64), Diagnostic> {
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let available = buffer.len().min((limit - total + 1) as usize);
        let read = match reader.read(&mut buffer[..available]) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(executable_error()),
        };
        total += read as u64;
        if total > limit {
            return Err(err(
                "SETUP_EXECUTABLE_SIZE",
                "Discovered executable grew beyond the 1 GiB fingerprint byte budget.",
            ));
        }
        hash.update(&buffer[..read]);
    }
    Ok((
        hash.finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        total,
    ))
}
fn hash_executable(
    path: &Path,
    mut file: fs::File,
    before: ExecutableFingerprint,
) -> Result<String, Diagnostic> {
    let (digest, count) = hash_executable_bytes(&mut file, MAX_EXECUTABLE_BYTES)?;
    let (_, named) = open_executable(path)?;
    if count != before.len || executable_fingerprint(&file)? != before || named != before {
        return Err(executable_error());
    }
    Ok(digest)
}
fn detect_clients(
    runner: &mut impl Runner,
    selected: Option<ClientKind>,
    mut discover: impl FnMut(ClientKind) -> Option<PathBuf>,
) -> Result<Vec<ClientObservation>, Diagnostic> {
    let mut clients = Vec::new();
    for client in [
        ClientKind::Codex,
        ClientKind::VsCode,
        ClientKind::ClaudeCode,
    ] {
        if selected.is_some_and(|selected| selected != client) {
            continue;
        }
        let Some(program) = discover(client) else {
            continue;
        };
        match detect_client(runner, client, &program) {
            Ok(observation) => clients.push(observation),
            Err(diagnostic) if selected == Some(client) => {
                return Err(err(
                    "SETUP_CLIENT_DETECT",
                    format!(
                        "Cannot verify selected {}: {} ({})",
                        client.label(),
                        diagnostic.message,
                        diagnostic.code
                    ),
                ));
            }
            Err(_) => {}
        }
    }
    Ok(clients)
}
fn detect_client(
    runner: &mut impl Runner,
    client: ClientKind,
    program: &Path,
) -> Result<ClientObservation, Diagnostic> {
    let (file, fingerprint) = open_executable(program)?;
    let version = probe(runner, program, &["--version"])?;
    let help = probe(runner, program, &["--help"])?;
    let mut capability = help;
    let (supports_http, supports_cimd, supports_manual, supports_login) = match client {
        ClientKind::Codex => {
            let add = probe(runner, program, &["mcp", "add", "--help"])?;
            let login = probe(runner, program, &["mcp", "login", "--help"])?;
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
        ClientKind::ClaudeCode => {
            let add = probe(runner, program, &["mcp", "add", "--help"])?;
            let add_text = String::from_utf8_lossy(&add);
            let http = add_text.contains("--transport")
                && add_text.contains("http")
                && add_text.contains("--scope");
            let cimd = http && setup::claude_supports_cimd(&String::from_utf8_lossy(&version));
            let manual = http && add_text.contains("--client-id");
            capability.extend(add);
            (http, cimd, manual, false)
        }
        _ => {
            return Err(err(
                "SETUP_EXECUTABLE",
                "This host requires no executable detection.",
            ));
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
    let executable_sha256 = hash_executable(program, file, fingerprint)?;
    Ok(ClientObservation {
        client,
        executable,
        executable_sha256,
        version,
        supports_http,
        supports_cimd,
        supports_manual,
        supports_login,
        capability_sha256: sha256_hex(&capability),
    })
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
        ClientKind::ClaudeCode => {
            let root = match env::var_os("CLAUDE_CONFIG_DIR") {
                Some(root) => PathBuf::from(root),
                None => home()?,
            };
            root.join(".claude.json")
        }
        ClientKind::CopilotCli | ClientKind::CopilotApp | ClientKind::CopilotVscode => {
            let root = match env::var_os("COPILOT_HOME") {
                Some(root) => PathBuf::from(root),
                None => home()?.join(".copilot"),
            };
            root.join("mcp-config.json")
        }
        _ => {
            return Err(err(
                "SETUP_HOST_MANAGED",
                "This host owns its MCP configuration; CQLmatic does not inspect it.",
            ));
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
fn read_json_config(path: &Path) -> Result<Value, Diagnostic> {
    reject_links(path)?;
    let value = if path.exists() {
        let raw = storage::read_bytes(path, MAX_CONFIG_BYTES)?;
        mcp::parse_saved_result(&raw).map_err(|_| {
            err(
                "SETUP_NATIVE_CONFIG",
                "MCP configuration has duplicate keys or invalid JSON; inspect it in the host before changing it.",
            )
        })?;
        serde_json::from_slice(&raw).map_err(|_| {
            err(
                "SETUP_NATIVE_CONFIG",
                "MCP configuration is not strict JSON; inspect it in the host before changing it.",
            )
        })?
    } else {
        serde_json::json!({})
    };
    if !value.is_object() {
        return Err(err(
            "SETUP_NATIVE_CONFIG",
            "MCP configuration must be a JSON object.",
        ));
    }
    Ok(value)
}
fn server_entry<'a>(
    config: &'a Value,
    key: &str,
    name: &str,
) -> Result<Option<&'a Value>, Diagnostic> {
    let servers = config
        .get(key)
        .map(|value| {
            value.as_object().ok_or_else(|| {
                err(
                    "SETUP_NATIVE_CONFIG",
                    "MCP server collection must be a JSON object.",
                )
            })
        })
        .transpose()?;
    Ok(servers.and_then(|servers| servers.get(name)))
}
fn copilot_entry(selection: &Selection) -> Result<Value, Diagnostic> {
    let RegistrationMode::Manual { client_id, .. } = &selection.registration else {
        return Err(err(
            "SETUP_REGISTRATION_MODE",
            "Copilot requires a pre-registered OAuth client ID.",
        ));
    };
    Ok(
        serde_json::json!({"type":"http","url":selection.cloud.endpoint(),"tools":[SEARCH_TOOL],"oauthClientId":client_id}),
    )
}
fn copilot_config_bytes(selection: &Selection, path: &Path) -> Result<Vec<u8>, Diagnostic> {
    if path.exists() {
        return Err(err(
            "SETUP_EXISTING_CONFIG",
            "Copilot configuration already exists. Add the selected HTTP server and registered oauthClientId in Copilot's own configuration flow, then preview again; setup will verify the exact entry without replacing the file.",
        ));
    }
    let mut config = read_json_config(path)?;
    let root = config.as_object_mut().expect("checked above");
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    let servers = servers.as_object_mut().ok_or_else(|| {
        err(
            "SETUP_NATIVE_CONFIG",
            "Copilot mcpServers must be a JSON object.",
        )
    })?;
    if servers.contains_key(&selection.server_name) {
        return Err(err(
            "SETUP_COLLISION",
            "Copilot server already exists; setup will not overwrite it.",
        ));
    }
    servers.insert(selection.server_name.clone(), copilot_entry(selection)?);
    let mut bytes = serde_json::to_vec_pretty(&config).map_err(|_| {
        err(
            "SETUP_SERIALIZATION",
            "Cannot serialize Copilot MCP configuration.",
        )
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}
fn write_new_copilot_config(
    path: &Path,
    bytes: &[u8],
    private_dir: &Path,
) -> Result<(), Diagnostic> {
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(err(
            "SETUP_NATIVE_CONFIG",
            "Copilot configuration exceeds its size limit.",
        ));
    }
    protect_directory(private_dir)?;
    let _writer = storage::Writer::acquire(path)?;
    if config_hash(path)?.is_some() {
        return Err(err(
            "SETUP_BASELINE_CHANGED",
            "Copilot configuration appeared before the reviewed write. Inspect it and preview again.",
        ));
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().ok_or_else(|| {
        err(
            "SETUP_CONFIG_PATH",
            "Copilot configuration needs a parent directory.",
        )
    })?;
    // The host directory can inherit broad Windows ACLs; stage under protected setup storage.
    let temp = private_dir.join(format!(
        ".cqlmatic-setup-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(|_| {
        err(
            "SETUP_NATIVE_CONFIG",
            "Cannot create a private Copilot configuration candidate.",
        )
    })?;
    let result = (|| {
        #[cfg(windows)]
        protect_copilot_candidate(&temp)?;
        verify_private_file(&temp)?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| {
                err(
                    "SETUP_NATIVE_CONFIG",
                    "Cannot durably write the private Copilot configuration candidate.",
                )
            })?;
        reject_links(path)?;
        fs::hard_link(&temp, path).map_err(|cause| {
            if cause.kind() == io::ErrorKind::AlreadyExists {
                err(
                    "SETUP_BASELINE_CHANGED",
                    "Copilot configuration appeared before activation. Inspect it and preview again.",
                )
            } else {
                err(
                    "SETUP_NATIVE_CONFIG",
                    "Cannot atomically create Copilot configuration from protected setup storage. Keep both paths on one volume or add the entry through Copilot's configuration flow.",
                )
            }
        })?;
        drop(file);
        fs::remove_file(&temp).map_err(|_| err("SETUP_DURABILITY", "Copilot configuration is visible, but candidate cleanup failed. Inspect it before retrying."))?;
        sync_directory(private_dir)?;
        sync_directory(parent)?;
        if config_hash(path)?.as_deref() != Some(sha256_hex(bytes).as_str()) {
            return Err(err(
                "SETUP_POSTCONDITION",
                "Copilot configuration bytes changed after activation. Inspect the host before retrying.",
            ));
        }
        verify_private_file(path)?;
        Ok(())
    })();
    if temp.exists() {
        let _ = fs::remove_file(&temp);
    }
    result
}
#[cfg(windows)]
fn protect_copilot_candidate(path: &Path) -> Result<(), Diagnostic> {
    const CREATE: &str = r#"$ErrorActionPreference='Stop'; $p=$env:CQLMATIC_SETUP_FILE; $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User; $a=New-Object System.Security.AccessControl.FileSecurity; $a.SetOwner($sid); $a.SetAccessRuleProtection($true,$false); $allow=[Security.AccessControl.AccessControlType]::Allow; $a.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid,'FullControl',$allow)); $a.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new('S-1-5-18'),'FullControl',$allow)); [IO.File]::SetAccessControl($p,$a)"#;
    let mut command = powershell()?;
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            CREATE,
        ])
        .env("CQLMATIC_SETUP_FILE", path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(|_| {
        err(
            "SETUP_PROTECTION",
            "Cannot protect Copilot configuration candidate.",
        )
    })?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => {
                return Err(err(
                    "SETUP_PROTECTION",
                    "Cannot protect Copilot configuration candidate.",
                ));
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(err(
                    "SETUP_PROTECTION",
                    "Copilot candidate ACL protection timed out.",
                ));
            }
        }
    }
}
fn check_portable_shadow(name: &str, copilot: bool) -> Result<(), Diagnostic> {
    let cwd = env::current_dir()
        .map_err(|_| err("SETUP_PATH", "Cannot inspect the current host workspace."))?;
    check_portable_shadow_from(&cwd, name, copilot)
}
fn check_portable_shadow_from(cwd: &Path, name: &str, copilot: bool) -> Result<(), Diagnostic> {
    for directory in cwd.ancestors() {
        for relative in if copilot {
            &[".mcp.json", ".github/mcp.json"][..]
        } else {
            &[".mcp.json"][..]
        } {
            let path = directory.join(relative);
            if path.exists()
                && server_entry(&read_json_config(&path)?, "mcpServers", name)?.is_some()
            {
                return Err(err(
                    "SETUP_CONFIG_SHADOW",
                    "A workspace MCP server with this name can override the selected user configuration. Choose another name or inspect that host entry.",
                ));
            }
        }
    }
    Ok(())
}
fn check_claude_managed(name: &str) -> Result<(), Diagnostic> {
    #[cfg(target_os = "macos")]
    let root = PathBuf::from("/Library/Application Support/ClaudeCode");
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let root = PathBuf::from("/etc/claude-code");
    #[cfg(windows)]
    let root = env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            err(
                "SETUP_CONFIG_PATH",
                "Windows ProgramFiles is unavailable for Claude managed MCP inspection.",
            )
        })?
        .join("ClaudeCode");
    check_claude_managed_at(&root, name)
}
fn check_claude_managed_at(root: &Path, name: &str) -> Result<(), Diagnostic> {
    if root.join("managed-mcp.json").exists() {
        return Err(err(
            "SETUP_CONFIG_SHADOW",
            "Claude's managed MCP file controls the effective servers; inspect it with your administrator before configuring a user entry.",
        ));
    }
    let settings = root.join("managed-settings.json");
    if settings.exists()
        && server_entry(&read_json_config(&settings)?, "managedMcpServers", name)?.is_some()
    {
        return Err(err(
            "SETUP_CONFIG_SHADOW",
            "A managed Claude MCP server with this name overrides the user entry.",
        ));
    }
    Ok(())
}
fn check_claude_project_shadow(config: &Value, cwd: &Path, name: &str) -> Result<(), Diagnostic> {
    let Some(projects) = config.get("projects") else {
        return Ok(());
    };
    let projects = projects.as_object().ok_or_else(|| {
        err(
            "SETUP_NATIVE_CONFIG",
            "Claude projects must be a JSON object.",
        )
    })?;
    let active = cwd.canonicalize().map_err(|_| {
        err(
            "SETUP_PATH",
            "Cannot inspect the active Claude project path.",
        )
    })?;
    for (root, project) in projects {
        if Path::new(root)
            .canonicalize()
            .is_ok_and(|root| active.starts_with(root))
            && server_entry(project, "mcpServers", name)?.is_some()
        {
            return Err(err(
                "SETUP_CONFIG_SHADOW",
                "A Claude local-scope server in this project overrides user scope. Choose another name or inspect that entry.",
            ));
        }
    }
    Ok(())
}
fn configured_fingerprint(
    selection: &Selection,
    path: &Path,
) -> Result<Option<String>, Diagnostic> {
    if !matches!(
        selection.client,
        ClientKind::ClaudeCode
            | ClientKind::CopilotCli
            | ClientKind::CopilotApp
            | ClientKind::CopilotVscode
    ) {
        return config_hash(path);
    }
    let config = read_json_config(path)?;
    let entry = server_entry(&config, "mcpServers", &selection.server_name)?;
    entry
        .map(|entry| {
            serde_json::to_vec(&(path.to_string_lossy(), entry))
                .map(|bytes| sha256_hex(&bytes))
                .map_err(|_| {
                    err(
                        "SETUP_SERIALIZATION",
                        "Cannot fingerprint native server entry.",
                    )
                })
        })
        .transpose()
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
    if !selected.client.is_native() {
        return Err(err(
            "SETUP_HOST_MANAGED",
            "This host owns its MCP configuration; no native baseline is inspected.",
        ));
    }
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
        ClientKind::ClaudeCode => {
            check_claude_managed(&selected.server_name)?;
            check_portable_shadow(&selected.server_name, false)?;
            let value = read_json_config(&config)?;
            check_claude_project_shadow(
                &value,
                &env::current_dir().map_err(|_| {
                    err(
                        "SETUP_PATH",
                        "Cannot inspect the active Claude project path.",
                    )
                })?,
                &selected.server_name,
            )?;
            let expected = serde_json::json!({"type":"http","url":selected.cloud.endpoint()});
            match server_entry(&value, "mcpServers", &selected.server_name)? {
                None => ExistingServer::Absent,
                Some(entry) if *entry == expected => ExistingServer::Same,
                Some(_) => ExistingServer::Collision,
            }
        }
        ClientKind::CopilotCli | ClientKind::CopilotApp | ClientKind::CopilotVscode => {
            check_portable_shadow(&selected.server_name, true)?;
            let value = read_json_config(&config)?;
            let expected = copilot_entry(selected)?;
            match server_entry(&value, "mcpServers", &selected.server_name)? {
                None => ExistingServer::Absent,
                Some(entry) if *entry == expected => ExistingServer::Same,
                Some(_) => ExistingServer::Collision,
            }
        }
        _ => {
            return Err(err(
                "SETUP_HOST_MANAGED",
                "This host owns its MCP configuration; no native baseline is inspected.",
            ));
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
            "Cannot confirm directory durability. Inspect the local files before retrying.",
        )
    })
}
pub(crate) fn verify_private_file(path: &Path) -> Result<(), Diagnostic> {
    reject_links(path)?;
    if !fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file()) {
        return Err(err(
            "SETUP_PROTECTION",
            "Protected setup input must be a regular file.",
        ));
    }
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
        #[cfg(target_os = "macos")]
        check_macos_acl(path)?;
    }
    #[cfg(windows)]
    {
        check_windows_acl(path, "file")
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
            let clients = runner.detect(None)?;
            if state.phase == SetupPhase::CommandPending {
                return Err(err(
                    "SETUP_PENDING",
                    "Inspect and acknowledge the pending native command before re-detecting.",
                ));
            }
            state.invalidate_plan();
            state.clients = clients;
            if let Some(s) = &state.selection
                && s.client.needs_executable()
                && !state.clients.iter().any(|c| c.client == s.client)
            {
                state.phase = SetupPhase::InstallationRequired;
            }
            Ok(())
        }
        SetupAction::Select { selection } => state.select(selection),
        SetupAction::Preview { operation } => {
            if state
                .selection
                .as_ref()
                .is_some_and(|s| !s.client.is_native())
            {
                return Err(err(
                    "SETUP_HOST_MANAGED",
                    "This host uses its own MCP configuration. Save and import a host tool-discovery report; no native preview is available.",
                ));
            }
            if state.phase == SetupPhase::CommandPending {
                return Err(err(
                    "SETUP_PENDING",
                    "Inspect and acknowledge the pending native command before previewing another operation.",
                ));
            }
            if state
                .selection
                .as_ref()
                .is_some_and(|s| s.client.needs_executable())
            {
                let selected = state.selection.as_ref().map(|selection| selection.client);
                let clients = runner.detect(selected)?;
                state
                    .clients
                    .retain(|client| Some(client.client) != selected);
                state.clients.extend(clients);
            }
            state.invalidate_plan();
            let baseline = inspect_baseline(state, state_path, runner)?;
            let backup = if operation == PlanOperation::Configure {
                backup_path(state_path, &baseline)?
            } else {
                None
            };
            let mut plan = setup::build_plan(state, operation, baseline, backup)?;
            if operation == PlanOperation::Configure
                && !plan.selection.client.needs_executable()
                && plan.baseline.server == ExistingServer::Absent
            {
                let bytes = copilot_config_bytes(&plan.selection, Path::new(&plan.baseline.path))?;
                plan.config_write_sha256 = Some(sha256_hex(&bytes));
                if let RegistrationMode::Manual { client_id, .. } = &plan.selection.registration {
                    plan.changes.push(format!(
                        "Effective OAuth client ID to configure: {client_id}"
                    ));
                }
            }
            if let RegistrationMode::Manual { callback_url, .. } = &plan.selection.registration {
                plan.changes.push(format!("Expected host callback (operator supplied; setup does not set it): {callback_url}"));
            }
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
            if state
                .selection
                .as_ref()
                .is_some_and(|s| !s.client.is_native())
            {
                return Err(err(
                    "SETUP_HOST_MANAGED",
                    "This host uses its own MCP configuration. No native plan can be applied.",
                ));
            }
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
            if plan.selection.client.needs_executable() {
                let current = runner.detect(Some(plan.selection.client))?;
                if !current
                    .iter()
                    .any(|client| Some(client) == plan.client.as_ref())
                {
                    return Err(err(
                        "SETUP_CLIENT_CHANGED",
                        "The selected executable or capabilities changed since review. Detect and preview again.",
                    ));
                }
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
                || expected.client != plan.client
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
            let expected_write = if plan.operation == PlanOperation::Configure
                && !plan.selection.client.needs_executable()
                && plan.baseline.server == ExistingServer::Absent
            {
                let bytes = copilot_config_bytes(&plan.selection, Path::new(&plan.baseline.path))?;
                if plan.config_write_sha256.as_deref() != Some(sha256_hex(&bytes).as_str()) {
                    return Err(err(
                        "SETUP_PLAN_INTEGRITY",
                        "Reviewed Copilot configuration differs from the derived write. Preview again.",
                    ));
                }
                Some(bytes)
            } else {
                if plan.config_write_sha256.is_some() {
                    return Err(err(
                        "SETUP_PLAN_INTEGRITY",
                        "Stored configuration write is not part of this plan. Preview again.",
                    ));
                }
                None
            };
            if plan.operation == PlanOperation::Configure
                && (!plan.commands.is_empty() || expected_write.is_some())
            {
                protect_backup(&plan)?;
            }
            state.phase = SetupPhase::CommandPending;
            state.last_notice = Some("Native configuration action is pending. If interrupted, inspect the client; no rollback or automatic retry is assumed.".into());
            save_state(writer, state).map_err(|_| {
                err(
                    "SETUP_STATE_SAVE",
                    "Cannot durably save pending setup state; no native action was started.",
                )
            })?;
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
            if let Some(bytes) = expected_write {
                let path = Path::new(&plan.baseline.path);
                if config_hash(path)? != plan.baseline.sha256 {
                    return Err(err(
                        "SETUP_BASELINE_CHANGED",
                        "Copilot configuration changed before the reviewed write. Inspect it before retrying.",
                    ));
                }
                write_new_copilot_config(path, &bytes, parent(state_path)?)?;
            }
            if plan.operation == PlanOperation::Configure {
                // Only a fresh reviewed add supplies registration arguments itself.
                // Native list output can establish the HTTP entry, never OAuth identity.
                let inspection = if plan.selection.client == ClientKind::Codex
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
                state.configured_sha256 =
                    configured_fingerprint(&plan.selection, Path::new(&actual.path))?;
                state.authentication_evidence = AuthenticationEvidence::None;
                state.discovery_sha256 = None;
                state.last_notice = Some(if plan.selection.client == ClientKind::Codex {
                    "Configured the HTTP entry after the exact reviewed native add command. A protected receipt binds the requested registration arguments to unchanged local configuration; Codex list output does not independently inspect OAuth identity or attest authentication."
                } else if matches!(plan.selection.client, ClientKind::ClaudeCode | ClientKind::CopilotCli | ClientKind::CopilotApp | ClientKind::CopilotVscode) {
                    "Configured the selected host's HTTP entry. Its effective session source and OAuth client identity still require host-session confirmation; configuration does not attest authentication."
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
            if state
                .selection
                .as_ref()
                .is_some_and(|s| !s.client.is_native())
            {
                return Err(err(
                    "SETUP_HOST_MANAGED",
                    "This host owns sign-in and consent. Import its saved tool-discovery report instead of confirming native sign-in.",
                ));
            }
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
            if state
                .selection
                .as_ref()
                .is_some_and(|s| s.client.is_native())
            {
                let baseline = inspect_baseline(state, state_path, runner)?;
                if baseline.server != ExistingServer::Same
                    || configured_fingerprint(
                        state.selection.as_ref().expect("checked by baseline"),
                        Path::new(&baseline.path),
                    )? != state.configured_sha256
                {
                    return Err(err(
                        "SETUP_BASELINE_CHANGED",
                        "Client configuration changed after setup. Review configuration before importing discovery evidence.",
                    ));
                }
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
            if let Some(hash) = &plan.config_write_sha256 {
                text.push_str(&format!("- Copilot config write SHA256: {hash}\n"));
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
fn fatal_interactive_error(code: &str) -> bool {
    matches!(
        code,
        "SETUP_STATE"
            | "SETUP_STATE_INTEGRITY"
            | "SETUP_PLAN_INTEGRITY"
            | "SETUP_STATE_SAVE"
            | "SETUP_SERIALIZATION"
            | "SETUP_OUTPUT"
            | "SETUP_INPUT"
    )
}
fn interactive_action(
    state_path: &Path,
    action: SetupAction,
) -> Result<Option<SetupState>, Diagnostic> {
    let mut action_error = None;
    let state = with_state(state_path, |state, runner, writer| {
        match perform(state, action, state_path, runner, writer, true) {
            Err(diagnostic) if !fatal_interactive_error(&diagnostic.code) => {
                action_error = Some(diagnostic);
                Ok(())
            }
            result => result,
        }
    })?;
    if let Some(diagnostic) = action_error {
        writeln!(
            io::stderr().lock(),
            "{}: {}",
            diagnostic.code,
            diagnostic.message
        )
        .map_err(|_| err("SETUP_OUTPUT", "Cannot write setup action error."))?;
        Ok(None)
    } else {
        Ok(Some(state))
    }
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
        "CQLmatic setup. OAuth, credentials and MCP stay in your chosen agent.\nConfirm the effective OAuth identity and CrowdStrike allowlisting before connecting. No DCR fallback.\n"
    );
    let (tx, rx) = mpsc::channel();
    let (read_tx, read_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        // Read exactly once per prompt. Do not compete with native-client consent stdin.
        while read_rx.recv().is_ok() {
            line.clear();
            match io::stdin().read_line(&mut line) {
                Ok(0) => {
                    let _ = tx.send(Ok(None));
                    break;
                }
                Err(_) => {
                    let _ = tx.send(Err(err("SETUP_INPUT", "Cannot read terminal input.")));
                    break;
                }
                Ok(_) => {
                    if tx.send(Ok(Some(line.trim().to_owned()))).is_err() {
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
        let host_managed = state
            .selection
            .as_ref()
            .is_some_and(|s| !s.client.is_native());
        if host_managed {
            println!(
                "\nChoose: 1 detect native clients, 2 choose agent/cloud, 7 import saved host report, 8 quit (progress saved)"
            );
        } else {
            println!(
                "\nChoose: 1 detect native clients, 2 choose agent/cloud, 3 preview configuration, 4 preview login, 5 apply reviewed plan, 6 confirm sign-in, 7 import saved host report, 8 quit (progress saved), 9 acknowledge inspected pending command"
            );
        }
        let Some(choice) = prompt(&rx, "> ")? else {
            with_state(state_path, |s, _, _| {
                s.cancel();
                Ok(())
            })?;
            return Ok(1);
        };
        if host_managed && matches!(choice.as_str(), "3" | "4" | "5" | "6" | "9") {
            println!(
                "This agent uses its own MCP configuration and sign-in. Import a saved host tool-discovery report after configuring it."
            );
            continue;
        }
        let action = match choice.as_str() {
            "1" => SetupAction::Detect {},
            "2" => {
                let Some(client) = prompt(
                    &rx,
                    "Agent [codex/vs-code/claude-code/copilot-cli/copilot-app/copilot-vscode/external]: ",
                )?
                else {
                    continue;
                };
                let client = match client.as_str() {
                    "codex" => ClientKind::Codex,
                    "vs-code" => ClientKind::VsCode,
                    "claude-code" => ClientKind::ClaudeCode,
                    "copilot-cli" | "copilot" | "gh copilot" => ClientKind::CopilotCli,
                    "copilot-app" => ClientKind::CopilotApp,
                    "copilot-vscode" => ClientKind::CopilotVscode,
                    "external" => ClientKind::External,
                    _ => {
                        println!("Choose one listed agent or external.");
                        continue;
                    }
                };
                if client.needs_executable() {
                    let Some(clients) = interactive_action(state_path, SetupAction::Detect {})?
                    else {
                        continue;
                    };
                    for detected in &clients.clients {
                        println!(
                            "{}: {} ({})",
                            detected.client.label(),
                            detected.executable,
                            detected.version
                        );
                    }
                    if !clients.clients.iter().any(|c| c.client == client)
                        && let Some(url) = client.install_url()
                    {
                        println!(
                            "Install {} from {} and resume this wizard. No installer is run.",
                            client.label(),
                            url
                        );
                    }
                }
                let host_id = if client == ClientKind::External {
                    let Some(id) = prompt(&rx, "Host ID (1-64 ASCII letters/digits/_/-): ")? else {
                        continue;
                    };
                    Some(id)
                } else {
                    None
                };
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
                let registration = if !client.is_native() {
                    RegistrationMode::HostManaged {}
                } else {
                    let options = if client == ClientKind::ClaudeCode {
                        "Registration [cimd/api-client]: "
                    } else if matches!(
                        client,
                        ClientKind::CopilotCli | ClientKind::CopilotApp | ClientKind::CopilotVscode
                    ) {
                        "Registration [manual/api-client]: "
                    } else {
                        "Registration [cimd/manual/api-client]: "
                    };
                    let Some(mode) = prompt(&rx, options)? else {
                        continue;
                    };
                    match mode.as_str() {
                        "cimd" => {
                            let url = if client == ClientKind::ClaudeCode {
                                "https://claude.ai/oauth/claude-code-client-metadata".into()
                            } else if client == ClientKind::VsCode {
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
                            let Some(confirm) = prompt(
                                &rx,
                                "Type allowlisted after checking that exact identity: ",
                            )?
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
                                "In Falcon MCP Client Management, register your native client using its actual callback behavior. Enter only its generated client ID here; optional secrets belong solely in the native client's protected UI. Setup records the expected callback but cannot set it for every host."
                            );
                            let Some(client_id) = prompt(&rx, "Generated manual MCP client ID: ")?
                            else {
                                continue;
                            };
                            let Some(callback_url) =
                                prompt(&rx, "Exact native-client callback URI: ")?
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
                    }
                };
                SetupAction::Select {
                    selection: Selection {
                        client,
                        host_id,
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
        interactive_action(state_path, action)?;
    }
}
struct TerminalInput {
    requests: mpsc::Sender<()>,
    lines: mpsc::Receiver<Result<Option<String>, Diagnostic>>,
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
            Ok(line) => return line,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return Err(err("SETUP_INPUT", "Terminal input ended unexpectedly.")),
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

    struct ProbeRunner {
        failing_program: Option<PathBuf>,
        calls: Vec<String>,
    }
    impl Runner for ProbeRunner {
        fn run(&mut self, command: &NativeCommand) -> Result<CommandOutput, Diagnostic> {
            self.calls.push(command.program.clone());
            if self.failing_program.as_deref() == Some(Path::new(&command.program)) {
                return Err(err("SETUP_PROBE", "Synthetic capability probe failure."));
            }
            Ok(CommandOutput {
                success: true,
                stdout: if command.args == ["--version"] {
                    b"2.2.0\n".to_vec()
                } else {
                    b"--url --add-mcp --transport http --scope --oauth-client-registration cimd --oauth-client-id".to_vec()
                },
            })
        }
        fn detect(&mut self, _: Option<ClientKind>) -> Result<Vec<ClientObservation>, Diagnostic> {
            unreachable!("Probe tests supply synthetic executable paths directly")
        }
        fn config_path(&self, _: ClientKind) -> Result<PathBuf, Diagnostic> {
            unreachable!("Capability detection must not inspect configuration")
        }
    }
    #[test]
    fn setup_detect_streams_executable_larger_than_256_mib() {
        let temp = Temp::new();
        let path = temp.0.join("synthetic-large-client");
        fs::File::create(&path)
            .unwrap()
            .set_len(256 * 1024 * 1024 + 1)
            .unwrap();
        let mut runner = ProbeRunner {
            failing_program: None,
            calls: Vec::new(),
        };
        let client = detect_client(&mut runner, ClientKind::ClaudeCode, &path).unwrap();
        assert_eq!(
            client.executable_sha256,
            "da6ce8755151acd05195db67ebce3ee0fb5f4012e71e821cc5750f3304eaf41e"
        );
        assert!(client.supports_cimd);
    }
    #[test]
    fn setup_executable_byte_budget_checks_metadata_and_stream_growth() {
        let temp = Temp::new();
        let path = temp.0.join("oversized-client");
        fs::File::create(&path)
            .unwrap()
            .set_len(MAX_EXECUTABLE_BYTES + 1)
            .unwrap();
        assert_eq!(
            open_executable(&path).unwrap_err().code,
            "SETUP_EXECUTABLE_SIZE"
        );
        let (digest, count) = hash_executable_bytes(&mut io::Cursor::new([0_u8; 32]), 32).unwrap();
        assert_eq!(count, 32);
        assert_eq!(digest, sha256_hex(&[0_u8; 32]));
        assert_eq!(
            hash_executable_bytes(&mut io::Cursor::new([0_u8; 33]), 32)
                .unwrap_err()
                .code,
            "SETUP_EXECUTABLE_SIZE"
        );
        assert_eq!(
            open_executable(&temp.0).unwrap_err().code,
            "SETUP_EXECUTABLE_CHANGED"
        );
    }
    #[cfg(unix)]
    #[test]
    fn setup_executable_fingerprint_rejects_growth_replacement_and_links() {
        use std::os::unix::fs::symlink;
        let temp = Temp::new();
        let path = temp.0.join("client");
        fs::write(&path, b"original").unwrap();
        let (file, before) = open_executable(&path).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"grew")
            .unwrap();
        assert_eq!(
            hash_executable(&path, file, before).unwrap_err().code,
            "SETUP_EXECUTABLE_CHANGED"
        );
        fs::write(&path, b"original").unwrap();
        let (file, before) = open_executable(&path).unwrap();
        let replacement = temp.0.join("replacement");
        fs::write(&replacement, b"replaced").unwrap();
        fs::File::open(&replacement)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(before.modified))
            .unwrap();
        fs::rename(&replacement, &path).unwrap();
        assert_eq!(
            hash_executable(&path, file, before).unwrap_err().code,
            "SETUP_EXECUTABLE_CHANGED"
        );
        let linked = temp.0.join("linked");
        symlink(&path, &linked).unwrap();
        assert_eq!(
            open_executable(&linked).unwrap_err().code,
            "SETUP_EXECUTABLE_CHANGED"
        );
    }
    #[test]
    fn setup_detect_isolates_failing_clients_and_reports_selected_failure() {
        let temp = Temp::new();
        let codex = temp.0.join("codex");
        let code = temp.0.join("code");
        fs::write(&codex, b"synthetic codex").unwrap();
        fs::File::create(&code)
            .unwrap()
            .set_len(MAX_EXECUTABLE_BYTES + 1)
            .unwrap();
        let paths = |client| match client {
            ClientKind::Codex => Some(codex.clone()),
            ClientKind::VsCode => Some(code.clone()),
            _ => None,
        };
        let mut runner = ProbeRunner {
            failing_program: None,
            calls: Vec::new(),
        };
        let clients = detect_clients(&mut runner, None, paths).unwrap();
        assert_eq!(clients.len(), 1);
        assert_eq!(clients[0].client, ClientKind::Codex);
        runner.calls.clear();
        let diagnostic = detect_clients(&mut runner, Some(ClientKind::VsCode), paths).unwrap_err();
        assert_eq!(diagnostic.code, "SETUP_CLIENT_DETECT");
        assert!(diagnostic.message.contains(ClientKind::VsCode.label()));
        assert!(diagnostic.message.contains("SETUP_EXECUTABLE_SIZE"));
        assert!(runner.calls.is_empty());
        fs::write(&code, b"synthetic code").unwrap();
        runner.failing_program = Some(code.clone());
        assert_eq!(detect_clients(&mut runner, None, paths).unwrap().len(), 1);
        assert_eq!(
            detect_clients(&mut runner, Some(ClientKind::Codex), paths)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            detect_clients(&mut runner, Some(ClientKind::VsCode), paths)
                .unwrap_err()
                .code,
            "SETUP_CLIENT_DETECT"
        );
    }
    #[test]
    fn setup_windows_acl_failure_details_are_bounded_and_escape_path_controls() {
        let detail = serde_json::to_vec(&json!({"path":"C:\\synthetic\npath", "sid":"S-1-1-0", "right":"Delete", "reason":"allow"})).unwrap();
        let diagnostic = windows_acl_failure(&detail);
        assert_eq!(diagnostic.code, "SETUP_PROTECTION");
        assert!(diagnostic.message.contains("S-1-1-0"));
        assert!(diagnostic.message.contains("Delete"));
        assert!(diagnostic.message.contains("\\n"));
        assert!(!diagnostic.message.contains('\n'));
        for invalid in [b"not json".to_vec(), serde_json::to_vec(&json!({"path":"C:\\synthetic", "sid":"S".repeat(257), "right":"Delete", "reason":"allow"})).unwrap()] {
            assert!(windows_acl_failure(&invalid).message.contains("without a bounded diagnostic"));
        }
    }
    #[test]
    fn setup_interactive_action_errors_resume_but_invalid_state_is_fatal() {
        assert!(fatal_interactive_error("SETUP_PLAN_INTEGRITY"));
        assert!(!fatal_interactive_error("SETUP_DISCOVERY_INPUT"));
        let temp = Temp::new();
        let path = temp.0.join("private/state.json");
        let selection = Selection {
            client: ClientKind::External,
            host_id: Some("invalid host".into()),
            cloud: Cloud::Us1,
            server_name: "cs-mcp".into(),
            registration: RegistrationMode::HostManaged {},
        };
        assert!(
            interactive_action(&path, SetupAction::Select { selection })
                .unwrap()
                .is_none()
        );
        assert!(
            interactive_action(
                &path,
                SetupAction::ImportDiscovery {
                    file: temp.0.join("absent-report").to_string_lossy().into_owned()
                }
            )
            .unwrap()
            .is_none()
        );
        assert!(
            interactive_action(&path, SetupAction::Status {})
                .unwrap()
                .is_some()
        );
        let mut stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        stored["state_sha256"] = json!("0".repeat(64));
        fs::write(&path, serde_json::to_vec(&stored).unwrap()).unwrap();
        assert_eq!(
            interactive_action(&path, SetupAction::Status {})
                .unwrap_err()
                .code,
            "SETUP_STATE_INTEGRITY"
        );
    }
    #[test]
    fn setup_terminal_read_error_propagates() {
        let (requests, _reader) = mpsc::channel();
        let (sender, lines) = mpsc::channel();
        sender
            .send(Err(err("SETUP_INPUT", "Synthetic terminal failure.")))
            .unwrap();
        assert_eq!(
            prompt(&TerminalInput { requests, lines }, "")
                .unwrap_err()
                .code,
            "SETUP_INPUT"
        );
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
        detection_scopes: Vec<Option<ClientKind>>,
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
            let claude = command
                .args
                .starts_with(&["mcp".into(), "add".into(), "--scope".into()]);
            let codex = command.args.starts_with(&["mcp".into(), "add".into()]) && !claude;
            if !codex && !claude {
                assert_eq!(command.args.first().map(String::as_str), Some("--add-mcp"));
            }
            self.native_calls += 1;
            self.commands.push(command.clone());
            self.saw_durable_pending =
                load_state(&self.state_path)?.phase == SetupPhase::CommandPending;
            if !self.no_mutation {
                let mut value: Value = if self.config.exists() {
                    serde_json::from_slice(&fs::read(&self.config).unwrap()).unwrap()
                } else if claude {
                    json!({"mcpServers":{}})
                } else {
                    json!({"servers":{}})
                };
                let entry: Value = if claude {
                    assert_eq!(
                        &command.args[2..6],
                        ["--scope", "user", "--transport", "http"]
                    );
                    json!({"name":command.args[6],"type":"http","url":command.args[7]})
                } else if codex {
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
                value[if claude { "mcpServers" } else { "servers" }][&name] = Value::Object(entry);
                fs::write(&self.config, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
            }
            Ok(CommandOutput {
                success: !self.fail_after_mutation,
                stdout: b"synthetic-sensitive-native-output-never-persist".to_vec(),
            })
        }
        fn detect(
            &mut self,
            selected: Option<ClientKind>,
        ) -> Result<Vec<ClientObservation>, Diagnostic> {
            self.detection_scopes.push(selected);
            Ok(self
                .observations
                .iter()
                .filter(|observation| selected.is_none_or(|client| observation.client == client))
                .cloned()
                .collect())
        }
        fn config_path(&self, client: ClientKind) -> Result<PathBuf, Diagnostic> {
            assert!(
                client.is_native(),
                "host-managed setup must not inspect native config"
            );
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
                    host_id: None,
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
                detection_scopes: Vec::new(),
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
                    host_id: None,
                    cloud: Cloud::Us1,
                    server_name: "cs-mcp".into(),
                    registration,
                })
                .unwrap();
            h
        }
        fn claude() -> Self {
            let mut h = Self::new();
            h.state.clients[0].client = ClientKind::ClaudeCode;
            h.runner.observations = h.state.clients.clone();
            h.state
                .select(Selection {
                    client: ClientKind::ClaudeCode,
                    host_id: None,
                    cloud: Cloud::Us1,
                    server_name: "cs-mcp".into(),
                    registration: RegistrationMode::Cimd {
                        metadata_url: "https://claude.ai/oauth/claude-code-client-metadata".into(),
                        allowlist_confirmed: true,
                    },
                })
                .unwrap();
            h
        }
        fn copilot(client: ClientKind) -> Self {
            let mut h = Self::new();
            h.runner.observations.clear();
            h.state.clients.clear();
            h.state
                .select(Selection {
                    client,
                    host_id: None,
                    cloud: Cloud::Us1,
                    server_name: "cs-mcp".into(),
                    registration: manual_registration("synthetic-copilot-id"),
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
                host_id: None,
                server_name: "cs-mcp".into(),
                endpoint: Cloud::Us1.endpoint().into(),
                authenticated: true,
                effective_oauth_client_id: None,
                effective_config_source: None,
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
    fn setup_selected_preview_and_apply_preserve_other_clients_without_probing_them() {
        let mut h = Harness::new();
        let mut unrelated = h.state.clients[0].clone();
        unrelated.client = ClientKind::Codex;
        h.state.clients.push(unrelated.clone());
        let hash = h.preview();
        assert_eq!(h.state.clients.len(), 2);
        assert!(h.state.clients.contains(&unrelated));
        h.apply(hash).unwrap();
        assert_eq!(h.runner.detection_scopes, vec![Some(ClientKind::VsCode); 2]);
        assert!(h.state.clients.contains(&unrelated));
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
    #[test]
    fn setup_external_import_never_uses_native_adapter() {
        let mut h = Harness::new();
        h.state
            .select(Selection {
                client: ClientKind::External,
                host_id: Some("synthetic-host".into()),
                cloud: Cloud::Us1,
                server_name: "cs-mcp".into(),
                registration: RegistrationMode::HostManaged {},
            })
            .unwrap();
        h.runner.observations.clear();
        h.action(SetupAction::Detect {}).unwrap();
        assert_eq!(h.state.phase, SetupPhase::Selected);
        for action in [
            SetupAction::Preview {
                operation: PlanOperation::Configure,
            },
            SetupAction::Apply {
                confirmation_sha256: "a".repeat(64),
            },
            SetupAction::ConfirmAuthentication {},
        ] {
            assert_eq!(h.action(action).unwrap_err().code, "SETUP_HOST_MANAGED");
        }
        let report_path = h._temp.0.join("synthetic-host-report.json");
        fs::write(
            &report_path,
            serde_json::to_vec(&HostToolDiscovery {
                schema_version: SCHEMA_VERSION,
                client: ClientKind::External,
                host_id: Some("synthetic-host".into()),
                server_name: "cs-mcp".into(),
                endpoint: Cloud::Us1.endpoint().into(),
                authenticated: true,
                effective_oauth_client_id: None,
                effective_config_source: None,
                tools: vec![DiscoveryTool {
                    name: SEARCH_TOOL.into(),
                    available: true,
                }],
            })
            .unwrap(),
        )
        .unwrap();
        h.action(SetupAction::ImportDiscovery {
            file: report_path.to_string_lossy().into_owned(),
        })
        .unwrap();
        assert_eq!(h.state.phase, SetupPhase::ToolDiscoveryVerified);
        assert_eq!(h.runner.native_calls, 0);
        assert!(!h.runner.config.exists());
    }
    #[test]
    fn setup_claude_native_add_and_session_identity_survive_unrelated_config_change() {
        let mut h = Harness::claude();
        let hash = h.preview();
        assert_eq!(h.state.plan.as_ref().unwrap().commands.len(), 1);
        h.apply(hash).unwrap();
        assert!(h.runner.saw_durable_pending);
        assert_eq!(h.state.phase, SetupPhase::AuthenticationRequired);
        let mut config: Value =
            serde_json::from_slice(&fs::read(&h.runner.config).unwrap()).unwrap();
        assert_eq!(
            config["mcpServers"]["cs-mcp"],
            json!({"type":"http","url":Cloud::Us1.endpoint()})
        );
        config["unrelatedPreference"] = json!(true);
        fs::write(&h.runner.config, serde_json::to_vec(&config).unwrap()).unwrap();
        let report_path = h._temp.0.join("claude-report.json");
        let mut report = HostToolDiscovery {
            schema_version: SCHEMA_VERSION,
            client: ClientKind::ClaudeCode,
            host_id: None,
            server_name: "cs-mcp".into(),
            endpoint: Cloud::Us1.endpoint().into(),
            authenticated: true,
            effective_oauth_client_id: Some("wrong-id".into()),
            effective_config_source: Some("user".into()),
            tools: vec![DiscoveryTool {
                name: SEARCH_TOOL.into(),
                available: true,
            }],
        };
        fs::write(&report_path, serde_json::to_vec(&report).unwrap()).unwrap();
        assert_eq!(
            h.action(SetupAction::ImportDiscovery {
                file: report_path.to_string_lossy().into_owned()
            })
            .unwrap_err()
            .code,
            "SETUP_DISCOVERY_IDENTITY"
        );
        report.effective_oauth_client_id =
            Some("https://claude.ai/oauth/claude-code-client-metadata".into());
        fs::write(&report_path, serde_json::to_vec(&report).unwrap()).unwrap();
        h.action(SetupAction::ImportDiscovery {
            file: report_path.to_string_lossy().into_owned(),
        })
        .unwrap();
        assert_eq!(h.state.phase, SetupPhase::ToolDiscoveryVerified);
    }
    #[test]
    fn setup_copilot_surfaces_write_shared_native_config_without_executable() {
        for client in [
            ClientKind::CopilotCli,
            ClientKind::CopilotApp,
            ClientKind::CopilotVscode,
        ] {
            let mut h = Harness::copilot(client);
            let hash = h.preview();
            let plan = h.state.plan.as_ref().unwrap();
            assert!(plan.client.is_none());
            assert!(plan.commands.is_empty());
            let write_hash = plan.config_write_sha256.clone().unwrap();
            assert!(plan.backup_path.is_none());
            h.apply(hash).unwrap();
            assert_eq!(h.runner.native_calls, 0);
            verify_private_file(&h.runner.config).unwrap();
            let bytes = fs::read(&h.runner.config).unwrap();
            assert_eq!(sha256_hex(&bytes), write_hash);
            assert_eq!(bytes.last(), Some(&b'\n'));
            let config: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                config["mcpServers"]["cs-mcp"],
                json!({"type":"http","url":Cloud::Us1.endpoint(),"tools":[SEARCH_TOOL],"oauthClientId":"synthetic-copilot-id"})
            );
            assert_eq!(h.state.phase, SetupPhase::AuthenticationRequired);
        }
    }
    #[test]
    fn setup_existing_copilot_config_is_preserved_and_verified_after_host_update() {
        let mut h = Harness::copilot(ClientKind::CopilotCli);
        let mut config = json!({"mcpServers":{"unrelated":{"type":"http","url":"https://synthetic.example/mcp","tools":["*"]}},"preference":true});
        let original = serde_json::to_vec(&config).unwrap();
        fs::write(&h.runner.config, &original).unwrap();
        assert_eq!(
            h.action(SetupAction::Preview {
                operation: PlanOperation::Configure
            })
            .unwrap_err()
            .code,
            "SETUP_EXISTING_CONFIG"
        );
        assert_eq!(fs::read(&h.runner.config).unwrap(), original);
        config["mcpServers"]["cs-mcp"] =
            copilot_entry(h.state.selection.as_ref().unwrap()).unwrap();
        let updated = serde_json::to_vec(&config).unwrap();
        fs::write(&h.runner.config, &updated).unwrap();
        let hash = h.preview();
        assert!(h.state.plan.as_ref().unwrap().config_write_sha256.is_none());
        h.apply(hash).unwrap();
        assert_eq!(fs::read(&h.runner.config).unwrap(), updated);
        assert_eq!(h.state.phase, SetupPhase::AuthenticationRequired);
        fs::write(&h.runner.config, b"{\"mcpServers\":{},\"mcpServers\":{}}").unwrap();
        assert_eq!(
            h.action(SetupAction::Preview {
                operation: PlanOperation::Configure
            })
            .unwrap_err()
            .code,
            "SETUP_NATIVE_CONFIG"
        );
    }
    #[test]
    fn setup_copilot_create_never_replaces_a_racing_file() {
        let temp = Temp::new();
        let path = temp.0.join("mcp-config.json");
        let private = temp.0.join("private");
        fs::write(&path, b"synthetic host update").unwrap();
        assert_eq!(
            write_new_copilot_config(&path, b"reviewed cqlmatic config\n", &private)
                .unwrap_err()
                .code,
            "SETUP_BASELINE_CHANGED"
        );
        assert_eq!(fs::read(&path).unwrap(), b"synthetic host update");
    }
    #[test]
    fn setup_copilot_new_file_stages_outside_host_directory() {
        let temp = Temp::new();
        let host = temp.0.join("host");
        fs::create_dir(&host).unwrap();
        let path = host.join("mcp-config.json");
        let private = temp.0.join("private");
        let bytes = b"reviewed cqlmatic config\n";
        write_new_copilot_config(&path, bytes, &private).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        verify_private_file(&path).unwrap();
        assert_eq!(fs::read_dir(&private).unwrap().count(), 0);
        assert_eq!(fs::read_dir(&host).unwrap().count(), 2); // config and its writer lock
    }
    #[test]
    fn setup_copilot_rejects_collision_and_stale_preview_without_write() {
        let mut h = Harness::copilot(ClientKind::CopilotCli);
        let conflicting = json!({"mcpServers":{"cs-mcp":{"type":"http","url":Cloud::Us1.endpoint(),"tools":[SEARCH_TOOL],"oauthClientId":"wrong-id"}}});
        let original = serde_json::to_vec(&conflicting).unwrap();
        fs::write(&h.runner.config, &original).unwrap();
        assert_eq!(
            h.action(SetupAction::Preview {
                operation: PlanOperation::Configure
            })
            .unwrap_err()
            .code,
            "SETUP_COLLISION"
        );
        assert_eq!(fs::read(&h.runner.config).unwrap(), original);
        fs::remove_file(&h.runner.config).unwrap();
        let hash = h.preview();
        fs::write(&h.runner.config, b"{\"mcpServers\":{}}").unwrap();
        assert_eq!(h.apply(hash).unwrap_err().code, "SETUP_BASELINE_CHANGED");
        assert_eq!(fs::read(&h.runner.config).unwrap(), b"{\"mcpServers\":{}}");
        assert_eq!(h.runner.native_calls, 0);
    }
    #[test]
    fn setup_checks_known_workspace_and_managed_shadows() {
        let temp = Temp::new();
        let nested = temp.0.join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(
            temp.0.join(".mcp.json"),
            b"{\"mcpServers\":{\"cs-mcp\":{}}}",
        )
        .unwrap();
        assert_eq!(
            check_portable_shadow_from(&nested, "cs-mcp", true)
                .unwrap_err()
                .code,
            "SETUP_CONFIG_SHADOW"
        );
        fs::remove_file(temp.0.join(".mcp.json")).unwrap();
        fs::write(
            temp.0.join("managed-settings.json"),
            b"{\"managedMcpServers\":{\"cs-mcp\":{}}}",
        )
        .unwrap();
        assert_eq!(
            check_claude_managed_at(&temp.0, "cs-mcp").unwrap_err().code,
            "SETUP_CONFIG_SHADOW"
        );
        fs::remove_file(temp.0.join("managed-settings.json")).unwrap();
        fs::write(temp.0.join("managed-mcp.json"), b"{\"mcpServers\":{}}").unwrap();
        assert_eq!(
            check_claude_managed_at(&temp.0, "cs-mcp").unwrap_err().code,
            "SETUP_CONFIG_SHADOW"
        );
    }
    #[test]
    fn setup_claude_project_shadow_applies_only_to_active_project() {
        let temp = Temp::new();
        let old = temp.0.join("old");
        let active = temp.0.join("active");
        let nested = active.join("nested");
        fs::create_dir(&old).unwrap();
        fs::create_dir_all(&nested).unwrap();
        let old_key = old.to_string_lossy().into_owned();
        let config = json!({"projects": {old_key: {"mcpServers": {"cs-mcp": {}}}}});
        check_claude_project_shadow(&config, &nested, "cs-mcp").unwrap();
        let active_key = active.to_string_lossy().into_owned();
        let config = json!({"projects": {active_key: {"mcpServers": {"cs-mcp": {}}}}});
        assert_eq!(
            check_claude_project_shadow(&config, &nested, "cs-mcp")
                .unwrap_err()
                .code,
            "SETUP_CONFIG_SHADOW"
        );
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
    #[test]
    fn private_read_verification_does_not_create_directories() {
        let temp = Temp::new();
        let missing = temp.0.join("missing/nested");
        assert!(verify_directory(&missing).is_err());
        assert!(!private_input_exists(&missing.join("state.json")).unwrap());
        assert!(!temp.0.join("missing").exists());
    }

    #[cfg(windows)]
    #[test]
    fn setup_new_private_directory_protects_every_parent_at_creation() {
        let temp = Temp::new();
        const BROAD_PARENT: &str = r#"$ErrorActionPreference='Stop'; $p=$env:CQLMATIC_TEST_DIRECTORY; $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User; $a=New-Object System.Security.AccessControl.DirectorySecurity; $a.SetOwner($sid); $a.SetAccessRuleProtection($true,$false); $inherit=[Security.AccessControl.InheritanceFlags]'ContainerInherit,ObjectInherit'; $prop=[Security.AccessControl.PropagationFlags]::None; $allow=[Security.AccessControl.AccessControlType]::Allow; foreach($who in @($sid,[Security.Principal.SecurityIdentifier]::new('S-1-5-18'),[Security.Principal.SecurityIdentifier]::new('S-1-1-0'))){$a.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($who,'FullControl',$inherit,$prop,$allow))}; [IO.Directory]::SetAccessControl($p,$a)"#;
        let status = powershell()
            .unwrap()
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                BROAD_PARENT,
            ])
            .env("CQLMATIC_TEST_DIRECTORY", &temp.0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        let unsafe_state = temp.0.join("outer/inner/state.json");
        let diagnostic = with_state(&unsafe_state, |_, _, _| Ok(())).unwrap_err();
        assert_eq!(diagnostic.code, "SETUP_PROTECTION");
        assert!(
            diagnostic
                .message
                .contains(&format!("{:?}", temp.0.to_string_lossy()))
        );
        assert!(diagnostic.message.contains("S-1-1-0"));
        assert!(diagnostic.message.contains("Delete"));
        assert!(!unsafe_state.exists());
        assert!(!temp.0.join("outer").exists());
        let clean = Temp::new();
        let parent = clean.0.join("outer");
        let leaf = parent.join("inner");
        protect_directory(&leaf).unwrap();
        protect_directory(&parent).unwrap();
        let candidate = leaf.join("candidate");
        fs::write(&candidate, b"synthetic").unwrap();
        verify_private_file(&candidate).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn setup_windows_executable_identity_and_read_sharing_are_stable() {
        let temp = Temp::new();
        let path = temp.0.join("client.exe");
        let other = temp.0.join("other.exe");
        fs::write(&path, b"synthetic executable").unwrap();
        fs::write(&other, b"synthetic executable").unwrap();
        let (file, fingerprint) = open_executable(&path).unwrap();
        let (_, other_fingerprint) = open_executable(&other).unwrap();
        assert_ne!(fingerprint.identity, other_fingerprint.identity);
        assert!(fs::OpenOptions::new().write(true).open(&path).is_err());
        assert!(fs::rename(&other, &path).is_err());
        assert_eq!(
            hash_executable(&path, file, fingerprint).unwrap(),
            sha256_hex(b"synthetic executable")
        );
    }
    #[cfg(windows)]
    #[test]
    fn setup_windows_acl_postcheck_preserves_a_newly_unsafe_directory() {
        let temp = Temp::new();
        let path = temp.0.join("private");
        let script = WINDOWS_ACL_CHECK.replace(
            "[IO.Directory]::CreateDirectory($p,$a) | Out-Null",
            "[IO.Directory]::CreateDirectory($p,$a) | Out-Null; $a.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new('S-1-1-0'),'FullControl',$inherit,$prop,$allow)); [IO.Directory]::SetAccessControl($p,$a)",
        );
        let mut command = powershell().unwrap();
        command
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &script,
            ])
            .env("CQLMATIC_SETUP_PATH", &path)
            .env("CQLMATIC_SETUP_ACL_MODE", "create-directory");
        let output = run_bounded_command(command, false, 20, 32 * 1024).unwrap();
        assert!(!output.success);
        let diagnostic = windows_acl_failure(&output.stdout);
        assert!(diagnostic.message.contains("S-1-1-0"));
        assert!(diagnostic.message.contains("FullControl"));
        assert!(path.is_dir());
    }
    #[cfg(windows)]
    #[test]
    fn setup_windows_helper_output_is_capped() {
        let mut command = powershell().unwrap();
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::Write(('x' * 65))",
        ]);
        assert_eq!(
            run_bounded_command(command, false, 20, 64)
                .unwrap_err()
                .code,
            "SETUP_NATIVE_OUTPUT"
        );
    }
    #[cfg(windows)]
    #[test]
    fn setup_accepts_inherit_only_ancestor_ace() {
        let temp = Temp::new();
        const INHERIT_ONLY: &str = r#"$ErrorActionPreference='Stop'; $p=$env:CQLMATIC_TEST_DIRECTORY; $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User; $a=New-Object System.Security.AccessControl.DirectorySecurity; $a.SetOwner($sid); $a.SetAccessRuleProtection($true,$false); $inherit=[Security.AccessControl.InheritanceFlags]'ContainerInherit,ObjectInherit'; $allow=[Security.AccessControl.AccessControlType]::Allow; foreach($who in @($sid,[Security.Principal.SecurityIdentifier]::new('S-1-5-18'))){$a.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($who,'FullControl',$inherit,[Security.AccessControl.PropagationFlags]::None,$allow))}; $a.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new('S-1-5-11'),'Modify',$inherit,[Security.AccessControl.PropagationFlags]::InheritOnly,$allow)); [IO.Directory]::SetAccessControl($p,$a)"#;
        let status = powershell()
            .unwrap()
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                INHERIT_ONLY,
            ])
            .env("CQLMATIC_TEST_DIRECTORY", &temp.0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        protect_directory(&temp.0.join("private")).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn setup_resolves_drive_relative_state_paths() {
        let drive = env::current_dir()
            .unwrap()
            .to_string_lossy()
            .chars()
            .next()
            .unwrap();
        let path = absolute(PathBuf::from(format!("{drive}:private\\state.json"))).unwrap();
        assert!(path.is_absolute());
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn setup_refuses_macos_acl_grants_on_ancestors_and_files() {
        use std::os::unix::fs::PermissionsExt;
        let temp = Temp::new();
        let ancestor = temp.0.join("ancestor");
        fs::create_dir(&ancestor).unwrap();
        fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            Command::new("/bin/chmod")
                .args(["+a", "nobody allow search,add_subdirectory,delete_child"])
                .arg(&ancestor)
                .status()
                .unwrap()
                .success()
        );
        let child = ancestor.join("private");
        assert_eq!(
            protect_directory(&child).unwrap_err().code,
            "SETUP_PROTECTION"
        );
        assert!(!child.exists());

        let file = temp.0.join("private-file");
        fs::write(&file, b"synthetic").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            Command::new("/bin/chmod")
                .args(["+a", "nobody allow read"])
                .arg(&file)
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(
            verify_private_file(&file).unwrap_err().code,
            "SETUP_PROTECTION"
        );

        let denied = temp.0.join("denied");
        let child = denied.join("child");
        fs::create_dir(&denied).unwrap();
        fs::create_dir(&child).unwrap();
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = check_macos_acl(&child);
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(unreadable.unwrap_err().code, "SETUP_PROTECTION");

        let deny_only = temp.0.join("deny-only");
        fs::create_dir(&deny_only).unwrap();
        assert!(
            Command::new("/bin/chmod")
                .args(["+a", "nobody deny delete"])
                .arg(&deny_only)
                .status()
                .unwrap()
                .success()
        );
        check_macos_acl(&deny_only).unwrap();
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
        fs::set_permissions(&temp.0, fs::Permissions::from_mode(0o777)).unwrap();
        let unsafe_child = temp.0.join("unsafe");
        assert_eq!(
            protect_directory(&unsafe_child).unwrap_err().code,
            "SETUP_PROTECTION"
        );
        assert!(!unsafe_child.exists());
    }
}
