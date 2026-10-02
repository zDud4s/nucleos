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
# Exit codes: 3 the shell is running and -Restart was not given, 4 build output missing;
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

if ([string]::IsNullOrEmpty($RunDir)) {
    $RunDir = Join-Path (Split-Path -Parent $RepoRoot) '.nucleos-run'
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
$running = @(Get-Process -Name 'nucleos-shell' -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -ieq $Copy })
if ($running.Count -gt 0) {
    if (-not $Restart) {
        $pids = ($running | ForEach-Object { $_.Id }) -join ', '
        Fail 3 "the shell is running from $Copy (pid $pids); close it, or pass -Restart to close it here"
    }
    $running | ForEach-Object { $_.CloseMainWindow() | Out-Null }
    $running | ForEach-Object {
        if (-not $_.WaitForExit(10000)) { Stop-Process -Id $_.Id -Force }
    }
}

# --- 3. replace the copy ----------------------------------------------------------------------
New-Item -ItemType Directory -Force -Path $RunDir | Out-Null
Copy-Item -LiteralPath $Built -Destination $Copy -Force

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
