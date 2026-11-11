param(
    [Parameter(Mandatory = $true)][string]$OutDir
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if (-not $IsWindows) { throw 'Native Windows is required.' }
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location $repo
if (-not $env:RUNNER_TEMP -or -not $env:CARGO_HOME -or -not $env:USERPROFILE) { throw 'Isolated RUNNER_TEMP, CARGO_HOME and USERPROFILE are required.' }
$env:CARGO_HOME = [IO.Path]::GetFullPath($env:CARGO_HOME)
if ($env:RUSTFLAGS -or $env:CARGO_ENCODED_RUSTFLAGS) { throw 'Global Rust flags would override target-only release flags.' }
$output = [IO.Path]::GetFullPath($OutDir)
if ($output.StartsWith($repo + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -or $output -eq $repo) {
    throw 'Artifact output must be outside the source tree.'
}
if (Test-Path $output) { throw 'Artifact output must be new.' }
$scratch = Join-Path $env:USERPROFILE ('cqlmatic-check-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $scratch | Out-Null
$priorTemp = @{ TMP = $env:TMP; TEMP = $env:TEMP }
$ownerToken = [IntPtr]::Zero
$ownerChanged = $false
$buildLog = Join-Path $scratch 'build.log'
$privatePrefixes = @($repo, $env:CARGO_HOME, $env:RUSTUP_HOME, $env:USERPROFILE, $env:RUNNER_TEMP) |
    Where-Object { $_ } | Sort-Object Length -Descending -Unique

function Invoke-Checked([string]$Program, [string[]]$Arguments) {
    $lines = & $Program @Arguments 2>&1
    $code = $LASTEXITCODE
    $text = ($lines | Out-String)
    foreach ($prefix in $privatePrefixes) {
        foreach ($variant in @($prefix, $prefix.Replace('\', '/'))) {
            $text = [regex]::Replace($text, [regex]::Escape($variant), '/build', 'IgnoreCase')
        }
    }
    $scanText = $text
    foreach ($prefix in @('git+https://', 'https://', 'http://', 'ftp://', 'file://')) {
        $scanText = $scanText.Replace($prefix, ' ')
    }
    if ($scanText -match '(?i)[a-z]:[\\/][^\s"<>|]{3,}|/(Users|home|private|Volumes)/') {
        throw 'Private path remains in command output; output withheld.'
    }
    [IO.File]::AppendAllText($buildLog, $text)
    if ($code -ne 0) {
        Write-Output $text
        throw "Required Windows command failed with exit $code."
    }
}

try {
    $currentSid = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $systemSid = [Security.Principal.SecurityIdentifier]::new('S-1-5-18')
    $acl = [Security.AccessControl.DirectorySecurity]::new()
    $acl.SetOwner($currentSid)
    $acl.SetAccessRuleProtection($true, $false)
    $inherit = [Security.AccessControl.InheritanceFlags]'ContainerInherit,ObjectInherit'
    foreach ($sid in @($currentSid, $systemSid)) {
        $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new(
            $sid, 'FullControl', $inherit, [Security.AccessControl.PropagationFlags]::None,
            [Security.AccessControl.AccessControlType]::Allow))
    }
    Set-Acl -LiteralPath $scratch -AclObject $acl
    $env:TMP = $scratch
    $env:TEMP = $scratch
    if (-not ('CqlmaticValidationOwner' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Security.Principal;

public static class CqlmaticValidationOwner {
    [DllImport("kernel32.dll")] static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool GetTokenInformation(IntPtr token, int kind, IntPtr data, int size, out int needed);
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool SetTokenInformation(IntPtr token, int kind, IntPtr data, int size);

    public static IntPtr Open(bool writable) {
        IntPtr token;
        if (!OpenProcessToken(GetCurrentProcess(), writable ? 0x88U : 0x08U, out token))
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Cannot open this process token.");
        return token;
    }
    public static SecurityIdentifier Sid(IntPtr token, int kind) {
        if (kind != 1 && kind != 4) throw new ArgumentException("Only token user and owner are supported.");
        int size;
        bool result = GetTokenInformation(token, kind, IntPtr.Zero, 0, out size);
        if (result || Marshal.GetLastWin32Error() != 122 || size < IntPtr.Size)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Cannot size this process token SID.");
        IntPtr data = Marshal.AllocHGlobal(size);
        try {
            if (!GetTokenInformation(token, kind, data, size, out size))
                throw new Win32Exception(Marshal.GetLastWin32Error(), "Cannot read this process token SID.");
            return new SecurityIdentifier(Marshal.ReadIntPtr(data));
        } finally { Marshal.FreeHGlobal(data); }
    }
    public static void SetOwner(IntPtr token, SecurityIdentifier sid) {
        byte[] bytes = new byte[sid.BinaryLength];
        sid.GetBinaryForm(bytes, 0);
        IntPtr binary = Marshal.AllocHGlobal(bytes.Length);
        try {
            Marshal.Copy(bytes, 0, binary, bytes.Length);
            IntPtr owner = Marshal.AllocHGlobal(IntPtr.Size);
            try {
                Marshal.WriteIntPtr(owner, binary);
                if (!SetTokenInformation(token, 4, owner, IntPtr.Size))
                    throw new Win32Exception(Marshal.GetLastWin32Error(), "Cannot set this process default owner.");
            } finally { Marshal.FreeHGlobal(owner); }
        } finally { Marshal.FreeHGlobal(binary); }
    }
    public static void Close(IntPtr token) { if (token != IntPtr.Zero) CloseHandle(token); }
}
'@
    }
    $queryToken = [CqlmaticValidationOwner]::Open($false)
    try {
        $tokenUser = [CqlmaticValidationOwner]::Sid($queryToken, 1)
        $originalOwner = [CqlmaticValidationOwner]::Sid($queryToken, 4)
    } finally { [CqlmaticValidationOwner]::Close($queryToken) }
    if ($tokenUser.Value -ne $currentSid.Value) { throw 'Validation requires the current process user without impersonation.' }
    if ($originalOwner.Value -ne $tokenUser.Value) {
        # Change only this process's TokenOwner; child processes copy its primary token.
        $ownerToken = [CqlmaticValidationOwner]::Open($true)
        if ([CqlmaticValidationOwner]::Sid($ownerToken, 4).Value -ne $originalOwner.Value) {
            throw 'Process default owner changed during validation preflight.'
        }
        [CqlmaticValidationOwner]::SetOwner($ownerToken, $tokenUser)
        $ownerChanged = $true
        if ([CqlmaticValidationOwner]::Sid($ownerToken, 4).Value -ne $tokenUser.Value) {
            throw 'Process default owner correction did not apply.'
        }
    }
    # Verify both inherited ACLs and the process default owner before running fixtures.
    $sentinel = Join-Path $scratch 'fixture-check'
    New-Item -ItemType Directory -Path $sentinel | Out-Null
    $sentinelFile = Join-Path $sentinel 'state.json'
    [IO.File]::WriteAllText($sentinelFile, '{}')
    foreach ($path in @($scratch, $sentinel, $sentinelFile)) {
        $actual = Get-Acl -LiteralPath $path
        if ($actual.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $currentSid.Value) {
            throw 'Native test fixture owner is not the current user.'
        }
        foreach ($rule in $actual.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
            if ($rule.AccessControlType -eq 'Allow' -and $rule.IdentityReference.Value -notin @($currentSid.Value, $systemSid.Value)) {
                throw 'Native test fixture is accessible to another principal.'
            }
        }
    }
    Remove-Item -Recurse -Force $sentinel
    Invoke-Checked 'rustup' @('toolchain', 'install', '1.99.1', '--profile', 'minimal', '--component', 'rustfmt', '--component', 'clippy', '--target', 'x86_64-pc-windows-msvc')
    Invoke-Checked 'cargo' @('+1.99.1', 'fetch', '--locked')
    Invoke-Checked 'cargo' @('+1.99.1', 'fmt', '--all', '--check')
    Invoke-Checked 'cargo' @('+1.99.1', 'clippy', '--offline', '--locked', '--all-targets', '--', '-D', 'warnings')
    Invoke-Checked 'cargo' @('+1.99.1', 'test', '--offline', '--locked')
    # Explicit targeting keeps the static CRT flag away from host build scripts and proc macros.
    if ($repo -match '\s' -or $env:CARGO_HOME -match '\s') { throw 'Build paths must contain no whitespace.' }
    $priorFlags = $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS
    $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = "-C target-feature=+crt-static -C debuginfo=0 -C strip=debuginfo -C link-arg=/DEBUG:NONE --remap-path-prefix=$repo=/cqlmatic --remap-path-prefix=$env:CARGO_HOME=/cargo"
    try {
        Invoke-Checked 'cargo' @('+1.99.1', 'build', '--release', '--offline', '--locked', '--target', 'x86_64-pc-windows-msvc')
    } finally {
        $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = $priorFlags
    }
    $targetRoot = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo 'target' }
    $binary = [IO.Path]::GetFullPath((Join-Path $targetRoot 'x86_64-pc-windows-msvc/release/cqlmatic.exe'))
    $fakeClients = Join-Path $scratch 'clients'
    New-Item -ItemType Directory -Path $fakeClients | Out-Null
    [IO.File]::WriteAllText((Join-Path $fakeClients 'codex.cmd'), @'
@echo off
if "%~1"=="--version" (
  echo codex-cli 1.0.0
) else (
  echo --url --oauth-client-registration cimd --oauth-client-id
)
'@)
    function Invoke-Cli([string[]]$Arguments, [int[]]$Accepted = @(0)) {
        $start = [Diagnostics.ProcessStartInfo]::new($binary)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.RedirectStandardInput = $true
        foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
        $start.Environment['PATH'] = $fakeClients
        foreach ($key in @('HOME', 'USERPROFILE', 'CODEX_HOME', 'APPDATA', 'LOCALAPPDATA')) {
            $start.Environment[$key] = Join-Path $scratch ('isolated-' + $key.ToLowerInvariant())
        }
        $process = [Diagnostics.Process]::Start($start)
        $process.StandardInput.Close()
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(30000)) {
            $process.Kill($true)
            throw 'Synthetic CLI check exceeded its deadline.'
        }
        if ($process.ExitCode -notin $Accepted) { throw 'Synthetic CLI check failed; raw output withheld.' }
        $null = $stderr.GetAwaiter().GetResult()
        return $stdout.GetAwaiter().GetResult()
    }
    if ((Invoke-Cli @('--version')).Trim() -ne 'cqlmatic 0.1.5') { throw 'Unexpected CLI version.' }
    $generated = Invoke-Cli @('generate', '--request', 'examples/generate.request.json', '--json') | ConvertFrom-Json
    if ($generated.outcome -ne 'ready') { throw 'Synthetic generation was not ready.' }
    foreach ($schema in @('generate', 'check', 'pack', 'discovery', 'result', 'assess', 'setup', 'inventory', 'status')) {
        $contract = Invoke-Cli @('--schema', $schema) | ConvertFrom-Json
        if ($contract.type -ne 'object') { throw 'Invalid schema root.' }
    }
    $setupState = Join-Path $scratch 'setup/state.json'
    $fresh = Invoke-Cli @('setup', '--state', $setupState, '--json') @(1) | ConvertFrom-Json
    if ($fresh.state.phase -ne 'new' -or (Test-Path $setupState)) { throw 'Fresh setup check failed.' }
    $detected = Invoke-Cli @('setup', '--state', $setupState, '--request', 'examples/setup/detect.request.json', '--json') @(1) | ConvertFrom-Json
    if (@($detected.state.clients).Count -ne 1 -or $detected.state.clients[0].client -ne 'codex' -or -not $detected.state.clients[0].supports_http) {
        throw 'Isolated synthetic client detection failed.'
    }
    $request = Join-Path $scratch 'request.json'
    $credentials = Join-Path $scratch 'absent-credentials.json'
    @{schema_version = 1; action = @{type = 'select'; selection = @{client = 'vs-code'; cloud = 'eu1'; server_name = 'synthetic-mcp'; registration = @{mode = 'api-client'; credentials_file = $credentials}}}} |
        ConvertTo-Json -Depth 8 | Set-Content -Encoding utf8NoBOM $request
    $selected = Invoke-Cli @('setup', '--state', $setupState, '--request', $request, '--json') @(1) | ConvertFrom-Json
    $resumed = Invoke-Cli @('setup', '--state', $setupState, '--json') @(1) | ConvertFrom-Json
    if ($selected.state.phase -ne 'advanced-pending' -or $resumed.state.phase -ne 'advanced-pending' -or (Test-Path $credentials)) {
        throw 'Synthetic setup resume check failed.'
    }
    $inventoryState = Join-Path $scratch 'inventory/state.json'
    @{schema_version = 1; action = @{kind = 'init'}} | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8NoBOM $request
    $null = Invoke-Cli @('env', 'inventory', '--state', $inventoryState, '--request', $request, '--json') | ConvertFrom-Json
    $inventory = Invoke-Cli @('status', '--state', $inventoryState, '--json') | ConvertFrom-Json
    if (-not (Test-Path $inventoryState) -or $inventory.full_map_complete -ne $false) { throw 'Synthetic inventory check failed.' }
    New-Item -ItemType Directory -Path $output | Out-Null
    Copy-Item $binary (Join-Path $output 'cqlmatic.exe')
    $commit = (& git rev-parse --verify HEAD).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Cannot identify source commit.' }
    if ($env:GITHUB_SHA -and $commit -ne $env:GITHUB_SHA) { throw 'Checkout differs from the workflow commit.' }
    $runner = Get-CimInstance Win32_OperatingSystem
    Invoke-Checked 'python' @('tools/package-release.py', 'inspect-windows', '--binary', (Join-Path $output 'cqlmatic.exe'),
        '--metadata', (Join-Path $output 'BUILD-WINDOWS.json'), '--build-log', $buildLog, '--commit', $commit,
        '--runner-os', $runner.Caption, '--runner-version', $runner.Version, '--runner-build', $runner.BuildNumber,
        '--runner-image', $env:ImageOS, '--runner-image-version', $env:ImageVersion)
    if (@(Get-ChildItem -File $output).Count -ne 2) { throw 'Unexpected Windows artifact files.' }
    Write-Output 'Native Windows checks, PE imports, and public artifact scans passed.'
} finally {
    try {
        if ($ownerChanged) {
            [CqlmaticValidationOwner]::SetOwner($ownerToken, $originalOwner)
            if ([CqlmaticValidationOwner]::Sid($ownerToken, 4).Value -ne $originalOwner.Value) {
                throw 'Process default owner restoration did not apply.'
            }
        }
    } finally {
        if ($ownerToken -ne [IntPtr]::Zero) { [CqlmaticValidationOwner]::Close($ownerToken) }
        $env:TMP = $priorTemp.TMP
        $env:TEMP = $priorTemp.TEMP
        Remove-Item -Recurse -Force $scratch
    }
}
