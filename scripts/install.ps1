# Install NucleOS on Windows (x64) from a GitHub release.
#
#   irm https://github.com/zDud4s/nucleos/releases/latest/download/install.ps1 | iex
#   & ([scriptblock]::Create((irm https://github.com/zDud4s/nucleos/releases/latest/download/install.ps1))) -Version 1.2.3
#
# The installer is fetched with Invoke-WebRequest, which leaves no Mark-of-the-Web on it, so
# SmartScreen does not block it. It is checked against the release's SHA256SUMS before it runs; a
# mismatch deletes it and exits non-zero. The installer runs silently (/S) for the current user,
# so close NucleOS first: an open app makes the silent install fail.
#
# Environment (all optional):
#   NUCLEOS_VERSION                  same as -Version
#   NUCLEOS_RELEASE_BASE             directory URL holding SHA256SUMS and the assets (tests, mirrors)
#   NUCLEOS_INSTALL_DRY_RUN=1        download and verify, run nothing
#   NUCLEOS_PROCESSOR_ARCHITECTURE   pretend to be another CPU (tests)
#
# Exit codes: 0 done; 1 download, checksum or install failure; 2 unsupported platform.
param([string]$Version = $env:NUCLEOS_VERSION)

$ErrorActionPreference = 'Stop'
# Without this Windows PowerShell 5.1 repaints a progress bar per chunk and downloads very slowly.
$ProgressPreference = 'SilentlyContinue'
try { [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 } catch { }

$Repo = 'zDud4s/nucleos'
# Keep this suffix in step with ASSETS in scripts/release-manifest.py (a test compares them).
$Suffix = '-setup.exe'

function Say([string]$m) { [Console]::Out.WriteLine($m) }
function Fail([string]$m, [int]$code) { [Console]::Error.WriteLine("install.ps1: $m"); return $code }

function Install-NucleOS {
    $arch = $env:NUCLEOS_PROCESSOR_ARCHITECTURE
    if (-not $arch) {
        $arch = $env:PROCESSOR_ARCHITEW6432
        if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    }
    # The x64 installer also runs on Windows on ARM through emulation.
    if ($arch -ne 'AMD64' -and $arch -ne 'ARM64') {
        return (Fail "unsupported architecture $arch (NucleOS has a Windows x64 installer)" 2)
    }

    $v = ("$Version").TrimStart('v')
    if ($env:NUCLEOS_RELEASE_BASE) { $base = $env:NUCLEOS_RELEASE_BASE.TrimEnd('/') }
    elseif ($v) { $base = "https://github.com/$Repo/releases/download/v$v" }
    else { $base = "https://github.com/$Repo/releases/latest/download" }
    Say "release: $base"

    $work = Join-Path $env:TEMP ("nucleos-install-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $work | Out-Null
    try {
        try { Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS" -OutFile (Join-Path $work 'SHA256SUMS') }
        catch { return (Fail "could not download $base/SHA256SUMS" 1) }

        $rows = @(Get-Content -LiteralPath (Join-Path $work 'SHA256SUMS') | ForEach-Object {
            $parts = $_.Trim() -split '\s+', 2
            if ($parts.Count -eq 2 -and $parts[1].EndsWith($Suffix)) { , $parts }
        })
        if ($rows.Count -eq 0) { return (Fail "SHA256SUMS has no entry ending in $Suffix" 1) }
        if ($rows.Count -gt 1) { return (Fail "SHA256SUMS has more than one entry ending in $Suffix" 1) }
        $want = $rows[0][0].ToLower()
        $name = $rows[0][1]

        Say "downloading $name"
        $file = Join-Path $work $name
        try { Invoke-WebRequest -UseBasicParsing -Uri "$base/$name" -OutFile $file }
        catch { return (Fail "could not download $base/$name" 1) }

        $got = (Get-FileHash -Algorithm SHA256 -LiteralPath $file).Hash.ToLower()
        if ($got -ne $want) {
            Remove-Item -LiteralPath $file -Force
            return (Fail "checksum mismatch for $name (expected $want, got $got); nothing was installed" 1)
        }
        Say "verified $name (sha256 $got)"

        if ($env:NUCLEOS_INSTALL_DRY_RUN -eq '1') { Say 'dry run: nothing installed'; return 0 }

        $p = Start-Process -FilePath $file -ArgumentList '/S' -Wait -PassThru
        if ($p.ExitCode -ne 0) { return (Fail "the installer exited with code $($p.ExitCode) (is NucleOS still running?)" 1) }
        Say 'installed NucleOS'
        return 0
    }
    finally {
        Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
    }
}

exit (Install-NucleOS)
