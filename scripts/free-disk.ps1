<#
.SYNOPSIS
  Frees disk by deleting Rust/Tauri build output on this machine.

.DESCRIPTION
  Targets, all of them regenerable by the next build:
    - every C:\Projects\.cargo-target* directory (the shared tree, the test
      tree and the per-branch trees CLAUDE.md asks to delete once a branch lands)
    - in every git worktree of this repo: shell\src-tauri\target, target-test,
      and target when it is a real directory (the main checkout's target is a
      junction to the shared tree and is skipped as a link)

  A directory that holds the executable of a running process (the daemon and
  its sidecars live in .cargo-target\debug) is pruned instead of deleted: the
  files at the top of each profile directory (debug\, release\) are kept, so
  the running binaries survive, and deps\, build\, incremental\, .fingerprint\
  and the rest go. The next cargo build rebuilds from scratch. A directory
  running a test binary (an executable under deps\) is left alone entirely.

  A directory a live rustc is building into is skipped, since deleting under a
  build corrupts it; the rest are still cleaned. -Force disables that check.
  -Exclude takes a comma list or an array, to also protect a directory by hand
  (cargo between two rustc invocations, or a test run, is not visible).

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\free-disk.ps1 -DryRun
  powershell -ExecutionPolicy Bypass -File scripts\free-disk.ps1
#>
param(
    [switch]$DryRun,
    [switch]$Force,
    # Directories to leave untouched, e.g. the target of a build running elsewhere.
    [string[]]$Exclude = @()
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot

function Get-DirSize([string]$path) {
    $sum = (Get-ChildItem -LiteralPath $path -Recurse -Force -File -ErrorAction SilentlyContinue |
        Measure-Object Length -Sum).Sum
    if ($null -eq $sum) { 0 } else { $sum }
}

function Format-GB([double]$bytes) { '{0:N1} GB' -f ($bytes / 1GB) }

# `powershell -File` hands a comma list over as ONE string, so split it here.
$Exclude = @($Exclude | ForEach-Object { $_ -split ',' } | Where-Object { $_.Trim() } |
    ForEach-Object { $_.Trim().TrimEnd('\').Replace('/', '\') })

# Command lines of live rustc processes. rustc is handed its target directory
# (--out-dir / -L / -C incremental), so a directory named in one is being built.
$rustcLines = @(Get-CimInstance Win32_Process -Filter "Name='rustc.exe'" -ErrorAction SilentlyContinue |
    ForEach-Object { $_.CommandLine.Replace('/', '\') })

# Candidate directories.
$candidates = @()
$candidates += Get-ChildItem 'C:\Projects' -Directory -Force -Filter '.cargo-target*' -ErrorAction SilentlyContinue |
    ForEach-Object { $_.FullName }

$worktrees = git -C $repo worktree list --porcelain |
    Where-Object { $_ -like 'worktree *' } |
    ForEach-Object { ($_.Substring(9)) -replace '/', '\' }
foreach ($wt in $worktrees) {
    foreach ($sub in 'shell\src-tauri\target', 'target-test', 'target') {
        $candidates += Join-Path $wt $sub
    }
}

$targets = $candidates | Select-Object -Unique | Where-Object {
    (Test-Path -LiteralPath $_) -and -not (Get-Item -LiteralPath $_ -Force).LinkType
} | Where-Object {
    $dir = $_.TrimEnd('\')
    -not ($Exclude | Where-Object { $_ -ieq $dir })
} | Where-Object {
    $dir = $_.TrimEnd('\')
    $building = (-not $Force) -and @($rustcLines | Where-Object { $_.IndexOf($dir + '\', [StringComparison]::OrdinalIgnoreCase) -ge 0 }).Count -gt 0
    if ($building) { Write-Host ("skip              {0}  (rustc is building into it)" -f $_) -ForegroundColor Yellow }
    -not $building
}

# Executables of running processes; a directory holding one is pruned, not deleted.
$running = Get-Process | Where-Object { $_.Path } | ForEach-Object { $_.Path }

$freeBefore = (Get-PSDrive C).Free
$total = 0

foreach ($dir in $targets) {
    $prefix = $dir.TrimEnd('\') + '\'
    $busy = @($running | Where-Object { $_.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase) })
    $size = Get-DirSize $dir
    $total += $size

    if ($busy.Count -eq 0) {
        Write-Host ("delete  {0,9}  {1}" -f (Format-GB $size), $dir)
        if (-not $DryRun) {
            Remove-Item -LiteralPath $dir -Recurse -Force -ErrorAction SilentlyContinue
        }
        continue
    }

    # A process running from deps\ is a test binary: a test run is live there, and
    # pruning would pull its build out from under it. Leave the whole directory.
    if ($busy | Where-Object { $_ -like '*\deps\*' }) {
        Write-Host ("skip    {0,9}  {1}  (test run in progress)" -f (Format-GB $size), $dir) -ForegroundColor Yellow
        $total -= $size
        continue
    }

    Write-Host ("prune   {0,9}  {1}  (in use: {2})" -f (Format-GB $size), $dir,
        (($busy | ForEach-Object { Split-Path -Leaf $_ }) -join ', '))
    if ($DryRun) { continue }
    foreach ($entry in Get-ChildItem -LiteralPath $dir -Force) {
        if ($entry.PSIsContainer -and $entry.Name -in 'debug', 'release') {
            # Keep the profile's top-level files (the final binaries); drop its subdirectories.
            Get-ChildItem -LiteralPath $entry.FullName -Directory -Force |
                Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
        } elseif ($entry.PSIsContainer) {
            Remove-Item -LiteralPath $entry.FullName -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

if ($DryRun) {
    Write-Host ("`nDry run: up to {0} across {1} directories." -f (Format-GB $total), @($targets).Count)
} else {
    $freed = (Get-PSDrive C).Free - $freeBefore
    Write-Host ("`nFreed {0}. Free on C: {1}." -f (Format-GB $freed), (Format-GB (Get-PSDrive C).Free)) -ForegroundColor Green
}
