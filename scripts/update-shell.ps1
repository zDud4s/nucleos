# Rebuild the shell in release and replace the copy the owner runs from the runtime dir.
#
# Kept apart from run-daemon.ps1 on purpose: starting the daemon must not pay for a shell
# build, and a frontend change must not restart the daemon. Run this only when the shell
# changed.
#
# Why release: the release shell.exe embeds the built frontend (tauri.conf.json frontendDist
# ../dist, produced by beforeBuildCommand `npm run build`), so a .tsx/.css change only reaches
# the copy through a rebuild. --no-bundle skips the MSI/NSIS installers; only the exe is needed.
#
# The build runs while the old shell is still open -- it writes to shell/src-tauri/target, not
# to the copy -- so the window is only closed for the copy itself. This script does not close
# it on its own: a running copy refuses (exit 3) unless -Restart says to close it.
#
# Exit codes: 3 the shell is running and -Restart was not given, 4 build output missing,
# 5 the copy stayed locked (retries and the rename-aside both failed);
# a build failure exits with the build's own code; otherwise 0.
#
# Usage: powershell -File scripts/update-shell.ps1 [-RunDir D] [-Restart] [-NoBuild] [-NoStart]
param(
    [string]$RunDir,
    [switch]$Restart,
    [switch]$NoBuild,
    [switch]$NoStart
)

$ErrorActionPreference = 'Stop'
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$ShellDir = Join-Path $RepoRoot 'shell'
$Built = Join-Path $ShellDir 'src-tauri\target\release\shell.exe'

function Fail([int]$Code, [string]$Message) {
    [Console]::Error.WriteLine("update-shell: $Message")
    exit $Code
}

function Get-FullPathOrNull([string]$Path) {
    if ([string]::IsNullOrEmpty($Path)) { return $null }
    try { return [IO.Path]::GetFullPath($Path) } catch { return $null }
}

# The nucleos-shell processes running from $Target. Win32_Process.ExecutablePath is read first:
# Get-Process .Path comes from MainModule and is null for a process this session may not open
# (e.g. an elevated one). Paths are normalized (GetFullPath folds '..' and '/') and compared
# case-insensitively. A nucleos-shell process whose path neither source can read is still
# returned, flagged PathKnown = $false: it is a match by name only.
function Get-RunningCopy([string]$Target) {
    $found = @()
    $procs = @(Get-CimInstance -ClassName Win32_Process -Filter "Name = 'nucleos-shell.exe'" -ErrorAction SilentlyContinue)
    foreach ($p in $procs) {
        $path = Get-FullPathOrNull $p.ExecutablePath
        if ($null -eq $path) {
            $gp = Get-Process -Id $p.ProcessId -ErrorAction SilentlyContinue
            if ($null -ne $gp) { $path = Get-FullPathOrNull $gp.Path }
        }
        if ($null -eq $path) {
            $found += [pscustomobject]@{ ProcessId = [int]$p.ProcessId; PathKnown = $false }
        }
        elseif ([string]::Equals($path, $Target, [StringComparison]::OrdinalIgnoreCase)) {
            $found += [pscustomobject]@{ ProcessId = [int]$p.ProcessId; PathKnown = $true }
        }
    }
    return , $found
}

# True once the process is gone. WaitForExit needs SYNCHRONIZE access; where that is refused
# (an elevated process) it falls back to polling.
function Wait-ProcessExit([int]$ProcessId, [int]$TimeoutMs) {
    $proc = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
    if ($null -eq $proc) { return $true }
    try { return $proc.WaitForExit($TimeoutMs) }
    catch {
        $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMs)
        while ([DateTime]::UtcNow -lt $deadline) {
            if ($null -eq (Get-Process -Id $ProcessId -ErrorAction SilentlyContinue)) { return $true }
            Start-Sleep -Milliseconds 200
        }
        return $null -eq (Get-Process -Id $ProcessId -ErrorAction SilentlyContinue)
    }
}

# Copy-Item with a short backoff (~5 s in all): an exited process can keep the image locked for
# a moment, and an antivirus scan of the fresh file can too. Returns the last error, or $null.
function Copy-WithRetry([string]$Source, [string]$Destination) {
    $lastError = $null
    foreach ($delayMs in @(0, 500, 1000, 1500, 2000)) {
        if ($delayMs -gt 0) { Start-Sleep -Milliseconds $delayMs }
        try {
            Copy-Item -LiteralPath $Source -Destination $Destination -Force -ErrorAction Stop
            return $null
        }
        catch { $lastError = $_ }
    }
    return $lastError
}

if ([string]::IsNullOrEmpty($RunDir)) {
    # The dev home, never the parent of the checkout: that holds one folder per project.
    $DevHome = if ($env:NUCLEOS_DEV_HOME) { $env:NUCLEOS_DEV_HOME } else { Join-Path "$env:SystemDrive\" 'nucleos-dev' }
    $RunDir = Join-Path $DevHome 'run'
}
elseif (-not [IO.Path]::IsPathRooted($RunDir)) {
    Fail 2 "-RunDir must be an absolute path (got '$RunDir')"
}
$RunDir = [IO.Path]::GetFullPath($RunDir).TrimEnd('\', '/')
$Copy = Join-Path $RunDir 'nucleos-shell.exe'

# --- 1. build, with the old shell still open --------------------------------------------------
if (-not $NoBuild) {
    Push-Location $ShellDir
    try {
        npm run tauri build -- --no-bundle
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }
    finally { Pop-Location }
}
if (-not (Test-Path -LiteralPath $Built)) {
    Fail 4 "no build output at $Built"
}

# --- 2. the running copy: only this one, matched by path, never another shell.exe ------------
$running = Get-RunningCopy $Copy
if ($running.Count -gt 0) {
    $pids = ($running | ForEach-Object { $_.ProcessId }) -join ', '
    $unknown = @($running | Where-Object { -not $_.PathKnown } | ForEach-Object { $_.ProcessId })
    $byName = if ($unknown.Count -gt 0) { "; pid $($unknown -join ', ') matched by name only, its path could not be read" } else { '' }
    if (-not $Restart) {
        Fail 3 "the shell is running from $Copy (pid $pids$byName); close it, or pass -Restart to close it here"
    }
    Write-Host "update-shell: closing the shell (pid $pids$byName)"
    foreach ($r in $running) {
        $proc = Get-Process -Id $r.ProcessId -ErrorAction SilentlyContinue
        if ($null -ne $proc) { try { $proc.CloseMainWindow() | Out-Null } catch { } }
    }
    foreach ($r in $running) {
        if (Wait-ProcessExit $r.ProcessId 10000) { continue }
        Stop-Process -Id $r.ProcessId -Force -ErrorAction SilentlyContinue
        # Stop-Process returns before the process is gone and the image lock released.
        if (-not (Wait-ProcessExit $r.ProcessId 10000)) {
            Write-Warning "update-shell: pid $($r.ProcessId) did not exit; the copy may stay locked"
        }
    }
}

# --- 3. replace the copy ----------------------------------------------------------------------
New-Item -ItemType Directory -Force -Path $RunDir | Out-Null
# Where a locked copy is moved aside. One left by an earlier run is deleted when its process has
# exited; a failure here only means it is still in use.
$Old = "$Copy.old"
Remove-Item -LiteralPath $Old -Force -ErrorAction SilentlyContinue
$copyError = Copy-WithRetry $Built $Copy
if ($null -ne $copyError -and (Test-Path -LiteralPath $Copy)) {
    # Still locked. Windows refuses to overwrite a running image but allows renaming it, so move
    # the locked exe aside and copy into the freed name.
    try {
        Remove-Item -LiteralPath $Old -Force -ErrorAction SilentlyContinue
        Move-Item -LiteralPath $Copy -Destination $Old -Force
        Write-Host "update-shell: $Copy was locked; moved it aside to $Old"
        $copyError = Copy-WithRetry $Built $Copy
    }
    catch { $copyError = $_ }
}
if ($null -ne $copyError) {
    $holders = Get-RunningCopy $Copy
    $who = if ($holders.Count -gt 0) {
        "held by nucleos-shell pid $(($holders | ForEach-Object { $_.ProcessId }) -join ', ')"
    } else {
        'no nucleos-shell process found holding it (an antivirus scan, a debugger or another tool may)'
    }
    Fail 5 "could not replace $Copy, $who`: $($copyError.Exception.Message)"
}
# shell.exe links WebView2Loader.dll dynamically; without it beside the copy the process dies at
# load with 0xC0000135 (STATUS_DLL_NOT_FOUND) and no window ever appears.
$Loader = Join-Path (Split-Path -Parent $Built) 'WebView2Loader.dll'
if (Test-Path -LiteralPath $Loader) {
    Copy-Item -LiteralPath $Loader -Destination $RunDir -Force
}

$commit = (git -C $RepoRoot rev-parse HEAD).Trim()
$dirty = if ((git -C $RepoRoot status --porcelain -- shell) -ne $null) { ' (plus uncommitted changes under shell/)' } else { '' }
$note = @(
    "repo: $RepoRoot"
    "commit: $commit$dirty"
    "source: $Built"
    "built: $((Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ'))"
) -join "`n"
Set-Content -LiteralPath (Join-Path $RunDir 'shell-built-from.txt') -Value $note -Encoding ASCII
Write-Host "update-shell: $Copy updated from $commit$dirty"

# --- 4. start it ------------------------------------------------------------------------------
if ($NoStart) { exit 0 }
Start-Process -FilePath $Copy -WorkingDirectory $RunDir
exit 0
