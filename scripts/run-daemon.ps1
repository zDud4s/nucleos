# Run the daemon from a COPY of its binaries, so no cargo target dir is ever held open.
#
# Why: `cargo run -p nucleos-core` keeps <target>/debug/nucleos-core.exe (and the
# *-sidecar.exe beside it) open for as long as the daemon lives, and on Windows every other
# cargo build or test that relinks that path then fails with "os error 5". That pushed sessions
# onto private target dirs, which is what saturated CPU and disk on 2026-10-01. Running a copy
# leaves the target dir rebuildable while the daemon runs.
#
# What is copied, and why all of it: only the sidecars are resolved relative to current_exe()
# (core/src/sidecar.rs, binary_in: $NUCLEOS_SIDECAR_DIR/<name>-sidecar.exe, else beside the
# exe). So every *-sidecar.exe goes next to the daemon, and NUCLEOS_SIDECAR_DIR is cleared for
# the child so they resolve beside the copy and not in the target dir.
#
# Why the runtime path is STABLE (no versioned copy dirs): a primary daemon re-registers the
# "NucleOS Daemon" logon task to current_exe() (autostart), and MCP configs record current_exe()
# too. Started from the copy, both point at the copy, which is the point -- but only if the path
# does not change from one run to the next.
#
# Why the child starts with cwd = repo root: `cargo run` runs the daemon with the cwd it was typed
# in, and the daemon reads it (main.rs legacy .ai/ migration, health.rs worktree free-space probe).
#
# heavy-broker: skip -- the heavy-command hook must not run this under scripts/heavy.py: the
# daemon it starts would hold a broker token and a target slot for as long as it lives, and
# inherit that slot's CARGO_TARGET_DIR into every build it runs.
#
# This script never kills anything. If the daemon is already running from the copy, it refuses
# (exit 3) and says so; stopping it is the owner's decision.
#
# Exit codes: 2 bad arguments, 3 the daemon is already running (or the port is taken),
# 4 build output missing; a build failure exits with the build's own code; otherwise the
# daemon's exit code (0 under -NoStart).
#
# Usage: powershell -File scripts/run-daemon.ps1 [-RunDir D] [-TargetDir T] [-BuildProfile debug|release]
#        [-Port N] [-NoBuild] [-NoStart] [daemon args...]
param(
    [string]$RunDir,
    [string]$TargetDir,
    [ValidateSet('debug', 'release')][string]$BuildProfile = 'debug',
    [int]$Port = 0,
    [switch]$NoBuild,
    [switch]$NoStart,
    [Parameter(ValueFromRemainingArguments = $true)][string[]]$DaemonArgs
)

$ErrorActionPreference = 'Stop'
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

function Fail([int]$Code, [string]$Message) {
    [Console]::Error.WriteLine("run-daemon: $Message")
    exit $Code
}

# --- 2a. the runtime dir ---------------------------------------------------------------------
if ([string]::IsNullOrEmpty($RunDir)) {
    $RunDir = Join-Path (Split-Path -Parent $RepoRoot) '.nucleos-run'
}
elseif (-not [IO.Path]::IsPathRooted($RunDir)) {
    Fail 2 "-RunDir must be an absolute path (got '$RunDir')"
}

# --- 2b. the target dir, and the RunDir must be outside it -------------------------------------
if ([string]::IsNullOrEmpty($TargetDir)) {
    if (-not [string]::IsNullOrEmpty($env:CARGO_TARGET_DIR)) {
        $TargetDir = $env:CARGO_TARGET_DIR
    }
    else {
        Push-Location $RepoRoot
        try {
            $TargetDir = (cargo metadata --format-version 1 --no-deps | ConvertFrom-Json).target_directory
        }
        finally { Pop-Location }
    }
}
$TargetDir = [IO.Path]::GetFullPath($TargetDir).TrimEnd('\', '/')
$RunDir = [IO.Path]::GetFullPath($RunDir).TrimEnd('\', '/')
$sep = [string][IO.Path]::DirectorySeparatorChar
if ($RunDir -ieq $TargetDir -or $RunDir.StartsWith($TargetDir + $sep, [StringComparison]::OrdinalIgnoreCase)) {
    Fail 2 "-RunDir ($RunDir) must be outside the cargo target dir ($TargetDir)"
}

# --- 2c. the port ----------------------------------------------------------------------------
if ($Port -eq 0) {
    $parsed = 0
    if ([int]::TryParse([string]$env:NUCLEOS_PORT, [ref]$parsed) -and $parsed -gt 0) { $Port = $parsed }
    else { $Port = 8791 }   # daemon_client::DEFAULT_PORT
}

# --- 2d. is the copy running? A running image cannot be opened with FileShare.None -------------
$held = @()
if (Test-Path -LiteralPath $RunDir) {
    $candidates = @(Get-ChildItem -LiteralPath $RunDir -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -ieq 'nucleos-core.exe' -or $_.Name -like '*-sidecar.exe' })
    foreach ($f in $candidates) {
        try {
            $h = [IO.File]::Open($f.FullName, 'Open', 'ReadWrite', 'None')
            $h.Close()
        }
        catch [IO.IOException] { $held += $f.FullName }
    }
}
if ($held.Count -gt 0) {
    $pids = @()
    foreach ($p in @(Get-Process -ErrorAction SilentlyContinue)) {
        $path = $null
        try { $path = $p.Path } catch { }
        if ($path -and ($held | Where-Object { $_ -ieq $path })) { $pids += $p.Id }
    }
    $who = if ($pids.Count -gt 0) { " (pid " + ($pids -join ', ') + ")" } else { "" }
    Fail 3 ("the daemon is running from $RunDir$who; stop it first -- Ctrl+C its terminal, " +
        "or schtasks /End /TN ""NucleOS Daemon"". Nothing was built or copied.")
}

# --- 2e. is something already answering on the port? -----------------------------------------
if (-not $NoStart) {
    $answers = $false
    $client = New-Object Net.Sockets.TcpClient
    try {
        if ($client.ConnectAsync('127.0.0.1', $Port).Wait(500) -and $client.Connected) { $answers = $true }
    }
    catch { $answers = $false }
    finally { $client.Dispose() }
    if ($answers) {
        Fail 3 ("something already answers on 127.0.0.1:$Port -- a daemon started another way " +
            "(cargo run?) must be stopped first. Nothing was built or copied.")
    }
}

$out = Join-Path $TargetDir $BuildProfile

# --- 3. build ----------------------------------------------------------------------------------
if (-not $NoBuild) {
    Push-Location $RepoRoot
    try {
        $cargoArgs = @('build', '-p', 'nucleos-core')
        if ($BuildProfile -eq 'release') { $cargoArgs += '--release' }
        & cargo @cargoArgs
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

        $had = $env:NUCLEOS_SIDECAR_DIR
        $env:NUCLEOS_SIDECAR_DIR = $out
        try {
            # Git's bash by absolute path: `bash` on PATH here is WSL, a different OS.
            & 'C:/Program Files/Git/bin/bash.exe' scripts/build-sidecars.sh $BuildProfile
            $code = $LASTEXITCODE
        }
        finally {
            if ($null -eq $had) { Remove-Item Env:NUCLEOS_SIDECAR_DIR -ErrorAction SilentlyContinue }
            else { $env:NUCLEOS_SIDECAR_DIR = $had }
        }
        if ($code -ne 0) { exit $code }
    }
    finally { Pop-Location }
}

# --- 4. stage ----------------------------------------------------------------------------------
$core = Join-Path $out 'nucleos-core.exe'
if (-not (Test-Path -LiteralPath $core)) {
    Fail 4 "build output missing: $core (build first, or drop -NoBuild)"
}
New-Item -ItemType Directory -Force -Path $RunDir | Out-Null
$sidecars = @(Get-ChildItem -LiteralPath $out -Filter '*-sidecar.exe' -File)
Copy-Item -LiteralPath $core -Destination $RunDir -Force
foreach ($s in $sidecars) { Copy-Item -LiteralPath $s.FullName -Destination $RunDir -Force }

$srcDir = Join-Path $RepoRoot 'sidecars'
if (Test-Path -LiteralPath $srcDir) {
    foreach ($d in @(Get-ChildItem -LiteralPath $srcDir -Directory)) {
        if (Test-Path -LiteralPath (Join-Path $d.FullName 'go.mod')) {
            if (-not (Test-Path -LiteralPath (Join-Path $out ($d.Name + '-sidecar.exe')))) {
                [Console]::Error.WriteLine("run-daemon: WARNING: no $($d.Name)-sidecar.exe in $out; the supervisor will report it")
            }
        }
    }
}

$head = ''
try {
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $head = [string](& git -C $RepoRoot rev-parse HEAD 2>$null)
    if ($LASTEXITCODE -ne 0) { $head = '' }
    $ErrorActionPreference = $prev
}
catch { $head = '' }
$stamp = [DateTime]::UtcNow.ToString('yyyy-MM-ddTHH:mm:ssZ')
$note = @("repo: $RepoRoot", "commit: $head", "source: $out", "built: $stamp")
Set-Content -LiteralPath (Join-Path $RunDir 'built-from.txt') -Value $note -Encoding ASCII
$count = 1 + $sidecars.Count + 1
Write-Output "staged $count files into $RunDir"
if ($NoStart) { exit 0 }

# --- 5. start (foreground, like `cargo run`) ---------------------------------------------------
Remove-Item Env:NUCLEOS_SIDECAR_DIR -ErrorAction SilentlyContinue
Push-Location $RepoRoot
& (Join-Path $RunDir 'nucleos-core.exe') @DaemonArgs
$code = $LASTEXITCODE
Pop-Location
exit $code
