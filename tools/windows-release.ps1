param(
    [Parameter(Mandatory = $true)][string]$OutDir
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if (-not $IsWindows) { throw 'Native Windows is required.' }
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location $repo
if (-not $env:RUNNER_TEMP -or -not $env:CARGO_HOME) { throw 'Isolated RUNNER_TEMP and CARGO_HOME are required.' }
$env:CARGO_HOME = [IO.Path]::GetFullPath($env:CARGO_HOME)
if ($env:RUSTFLAGS -or $env:CARGO_ENCODED_RUSTFLAGS) { throw 'Global Rust flags would override target-only release flags.' }
$output = [IO.Path]::GetFullPath($OutDir)
if ($output.StartsWith($repo + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -or $output -eq $repo) {
    throw 'Artifact output must be outside the source tree.'
}
if (Test-Path $output) { throw 'Artifact output must be new.' }
$scratch = Join-Path $env:RUNNER_TEMP ('cqlmatic-check-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $scratch | Out-Null
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
    if ($text -match '(?i)[a-z]:[\\/][^\s"<>|]{3,}|/(Users|home|private|Volumes)/') {
        throw 'Private path remains in command output; output withheld.'
    }
    [IO.File]::AppendAllText($buildLog, $text)
    if ($code -ne 0) {
        Write-Output $text
        throw "Required Windows command failed with exit $code."
    }
}

try {
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
    if ((Invoke-Cli @('--version')).Trim() -ne 'cqlmatic 0.1.0') { throw 'Unexpected CLI version.' }
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
    Remove-Item -Recurse -Force $scratch
}
