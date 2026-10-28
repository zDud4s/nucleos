# Run the daemon from a COPY of its binaries, so no cargo target dir is ever held open.
#
# Why: `cargo run -p nucleos-daemon` keeps <target>/debug/nucleos-core.exe (and the
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
# (exit 3) and says so; stopping it is the owner's decision. When only `--mcp-tools` servers hold
# the copy (stdio MCP servers other Claude sessions spawned; the daemon is then already stopped),
# it does not refuse and does not touch them: Windows lets a running image be RENAMED though not
# overwritten or deleted, so each held file is moved aside to <name>.old-<stamp>-<pid> in the same
# dir and fresh files are staged. The servers keep running from the renamed image, new sessions get
# the new exe, and a later run sweeps the *.old-* files nothing holds any more. If a rename fails,
# it falls back to the exit-3 refusal.
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
    # The dev home, never the parent of the checkout: that holds one folder per project.
    $DevHome = if ($env:NUCLEOS_DEV_HOME) { $env:NUCLEOS_DEV_HOME } else { Join-Path "$env:SystemDrive\" 'nucleos-dev' }
    $RunDir = Join-Path $DevHome 'run'
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
$moveAside = @()   # held files that only --mcp-tools servers hold: renamed at stage time
$toolPids = @()
if (Test-Path -LiteralPath $RunDir) {
    # Images moved aside by an earlier run: delete the ones nothing holds any more, quietly.
    foreach ($old in @(Get-ChildItem -LiteralPath $RunDir -File -Filter '*.exe.old-*' -ErrorAction SilentlyContinue)) {
        try { Remove-Item -LiteralPath $old.FullName -Force -ErrorAction Stop } catch { }
    }
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
$refusal = $null
if ($held.Count -gt 0) {
    # The same exe also runs as `--mcp-tools`, the stdio MCP server a Claude session spawns. It
    # holds the image just as the daemon does, but it is not the daemon: its image is moved aside
    # (see the header) instead of refusing.
    $daemons = @()
    $tools = @()
    foreach ($p in @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue)) {
        $path = $p.ExecutablePath
        if (-not $path -or -not ($held | Where-Object { $_ -ieq $path })) { continue }
        if ([string]$p.CommandLine -match '(^|\s)--mcp-tools(\s|$)') { $tools += $p } else { $daemons += $p }
    }
    # Every held file needs a --mcp-tools owner; one held by something unidentified is refused.
    $unowned = @($held | Where-Object { $h1 = $_; -not ($tools | Where-Object { $_.ExecutablePath -ieq $h1 }) })
    if ($daemons.Count -eq 0 -and $tools.Count -gt 0 -and $unowned.Count -eq 0) {
        $moveAside = $held
        $toolPids = @($tools | ForEach-Object { $_.ProcessId })
    }
    else {
        $who = if ($daemons.Count -gt 0) { " (pid " + (@($daemons | ForEach-Object { $_.ProcessId }) -join ', ') + ")" } else { "" }
        Fail 3 ("the daemon is running from $RunDir$who; stop it first -- Ctrl+C its terminal, " +
            "or schtasks /End /TN ""NucleOS Daemon"". Nothing was built or copied.")
    }
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
        $cargoArgs = @('build', '-p', 'nucleos-daemon')
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
if ($moveAside.Count -gt 0) {
    $suffix = ".old-" + (Get-Date -Format 'yyyyMMdd-HHmmss') + "-$PID"
    $moved = @()
    try {
        foreach ($m in $moveAside) {
            Rename-Item -LiteralPath $m -NewName ((Split-Path -Leaf $m) + $suffix) -ErrorAction Stop
            $moved += ((Split-Path -Leaf $m) + $suffix)
        }
    }
    catch {
        # Put back what was moved, so the refusal leaves the run dir as it found it.
        foreach ($n in $moved) {
            try { Rename-Item -LiteralPath (Join-Path $RunDir $n) -NewName $n.Substring(0, $n.Length - $suffix.Length) -ErrorAction Stop } catch { }
        }
        Fail 3 ("the daemon is not running, but $($toolPids.Count) MCP tool server(s) (--mcp-tools, pid " +
            ($toolPids -join ', ') + ") hold the copy in $RunDir and it could not be moved aside ($($_.Exception.Message)). " +
            "Stop them -- Stop-Process -Id " + ($toolPids -join ',') + " -- then reconnect those sessions with /mcp. Nothing was copied.")
    }
    [Console]::Error.WriteLine("run-daemon: $($toolPids.Count) MCP tool server(s) hold the copy; moved the held image(s) aside to " +
        ($moved -join ', ') + " and staging fresh")
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
# Started from a Claude Code session, the daemon would inherit its id and hand it to every gate it
# runs, and heavy.py would file the daemon's gates under that session.
Remove-Item Env:CLAUDE_CODE_SESSION_ID -ErrorAction SilentlyContinue
# Verification build output goes to the dev home, not ~/.nucleos: a profile path with a space in it
# breaks the windows-gnu linker (warm::HOME_ENV). An explicit value is kept.
if ([string]::IsNullOrEmpty($env:NUCLEOS_WARM_HOME)) {
    $env:NUCLEOS_WARM_HOME = if ($env:NUCLEOS_DEV_HOME) { $env:NUCLEOS_DEV_HOME } else { Join-Path "$env:SystemDrive\" 'nucleos-dev' }
}
Push-Location $RepoRoot
& (Join-Path $RunDir 'nucleos-core.exe') @DaemonArgs
$code = $LASTEXITCODE
Pop-Location
exit $code
