<#
.SYNOPSIS
    First-time installer for Rake.

.DESCRIPTION
    Fetches the latest Rake release, verifies its SHA-256 checksum, and puts the
    executable on PATH. Everything after that is Rake's own job: `rake self update`
    and `rake self uninstall` are implemented in Rust and need no script.

    This file exists only to bootstrap the very first binary, before there is a
    Rake to ask. It is therefore not shipped inside release archives.

.PARAMETER Source
    Install from a local rake.exe instead of downloading a release.

.PARAMETER Version
    Install a specific release tag, e.g. v0.1.4-alpha.1. Without it the latest
    stable release is used.

.PARAMETER Prerelease
    Install the most recent pre-release instead of the latest stable one.
    GitHub's `/releases/latest` deliberately excludes pre-releases, so this is
    the only way to opt into one.

.EXAMPLE
    # stable
    & ([scriptblock]::Create((iwr -useb https://raw.githubusercontent.com/nidara-duo/rake/main/scripts/bootstrap.ps1)))

    # newest pre-release
    & ([scriptblock]::Create((iwr -useb https://raw.githubusercontent.com/nidara-duo/rake/main/scripts/bootstrap.ps1))) -Prerelease

    # exact tag
    & ([scriptblock]::Create((iwr -useb https://raw.githubusercontent.com/nidara-duo/rake/main/scripts/bootstrap.ps1))) -Version v0.1.4-alpha.1

    # from a local build
    .\bootstrap.ps1 -Source ..\target\release\rake.exe
#>

param(
    [string]$Source = "",
    [string]$Version = "",
    [switch]$Prerelease
)

# ─── Configuration ───────────────────────────────────────────────────────────

$Repo = "nidara-duo/rake"
$ApiUrl = "https://api.github.com/repos/$Repo/releases"
$InstallRoot = "$env:LOCALAPPDATA\rake"
$BinDir = "$InstallRoot\bin"
$ExePath = "$BinDir\rake.exe"

# ─── Output ───────────────────────────────────────────────────────────────────

function Write-Step { param([string]$m) Write-Host "==> $m" -ForegroundColor Cyan }
function Write-Err  { param([string]$m) Write-Host "ERROR: $m" -ForegroundColor Red }
function Write-Ok   { param([string]$m) Write-Host "  OK $m" -ForegroundColor Green }

# Refuse the install by throwing rather than calling `exit`.
#
# This script is documented to be run with `Invoke-Expression`, which executes in
# the caller's session. `exit` would close the user's terminal window instead of
# reporting why the install was refused, which is how a checksum mismatch — the
# one error worth reading carefully — would become invisible.
#
# A terminating error keeps the message visible, still propagates to the caller,
# and still yields exit code 1 under `powershell -Command`, so CI and
# `if ($LASTEXITCODE -ne 0)` keep working.
function Stop-Install {
    param([string]$m)
    Write-Err $m
    throw $m
}

# ─── Helpers ──────────────────────────────────────────────────────────────────

function Get-ArchSuffix {
    if ([Environment]::Is64BitOperatingSystem) {
        if ([Environment]::GetEnvironmentVariable("PROCESSOR_ARCHITECTURE") -eq "ARM64") {
            return "aarch64-pc-windows-msvc"
        }
        return "x86_64-pc-windows-msvc"
    }
    return "i686-pc-windows-msvc"
}

function Resolve-Release {
    # Returns the release whose asset we want.
    #
    # Three modes: an exact tag, the newest pre-release, or whatever
    # /releases/latest gives — which never includes pre-releases, so asking for
    # one has to go through /releases and filter.
    param([string]$AssetName)

    if ($Version) {
        $release = Invoke-RestMethod -Uri "$ApiUrl/tags/$Version" -UseBasicParsing -ErrorAction Stop
        $label = $Version
    }
    elseif ($Prerelease) {
        $all = Invoke-RestMethod -Uri $ApiUrl -UseBasicParsing -ErrorAction Stop
        # A pre-release is anything whose tag carries a SemVer pre-release suffix,
        # whether or not GitHub's own flag is set. That flag is written once at
        # release-creation time, so a release published before the workflow
        # started setting it keeps prerelease=false forever — the tag is the
        # intent, the flag is derived metadata that can lag.
        $release = $all |
            Where-Object { -not $_.draft -and ($_.prerelease -or $_.tag_name -match '-') } |
            Sort-Object -Property published_at -Descending |
            Select-Object -First 1
        if (-not $release) {
            Stop-Install "No pre-release found on $Repo"
        }
        $label = "pre-release $($release.tag_name)"
    }
    else {
        $release = Invoke-RestMethod -Uri "$ApiUrl/latest" -UseBasicParsing -ErrorAction Stop
        $label = $release.tag_name
    }

    $asset = $release.assets | Where-Object { $_.name -eq $AssetName } | Select-Object -First 1
    if (-not $asset) {
        Stop-Install "Release $label has no asset named $AssetName"
    }

    Write-Ok "Selected: $label"
    return $asset.browser_download_url
}

function Get-Checksum {
    param([string]$Url)

    try {
        $text = Invoke-RestMethod -Uri "$Url.sha256" -UseBasicParsing -ErrorAction Stop
        $token = ($text.Trim() -split '\s+')[0]
        if ($token -match '^[a-f0-9]{64}$') { return $token }
    } catch {
        # fall through to the refusal below
    }
    return $null
}

function Install-Exe {
    param([string]$ExeSource)

    if (-not (Test-Path $ExeSource)) {
        Stop-Install "Binary not found: $ExeSource"
    }

    New-Item -ItemType Directory -Path $BinDir -Force | Out-Null

    # Rake may already be installed. It cannot be overwritten while running, but it can
    # be renamed aside; Rake removes the leftover on its next start.
    if (Test-Path $ExePath) {
        $stale = "$BinDir\rake.exe.old"
        if (Test-Path $stale) { Remove-Item -LiteralPath $stale -Force -ErrorAction SilentlyContinue }
        Move-Item -LiteralPath $ExePath -Destination $stale -Force
        Write-Ok "Existing rake.exe moved aside as $(Split-Path -Leaf $stale)"
    }

    Copy-Item -LiteralPath $ExeSource -Destination $ExePath -Force
    Write-Ok "Installed $ExePath"

    # Releases published before self-management moved into the executable shipped a
    # PowerShell bootstrap alongside it and `rake self` refused to run without it. Keep
    # the script when the payload still contains one, so installing an older release does
    # not leave it unable to update itself. Newer payloads no longer include it.
    $alongside = Join-Path (Split-Path -Parent $ExeSource) "bootstrap.ps1"
    if (Test-Path $alongside) {
        Copy-Item -LiteralPath $alongside -Destination "$BinDir\bootstrap.ps1" -Force
        Write-Ok "Kept bootstrap.ps1 for this release"
    }
}

function Add-ToPath {
    $scope = "User"
    $current = [Environment]::GetEnvironmentVariable("PATH", $scope)
    if (-not $current) { return }
    if (@($current -split ";") | Where-Object { $_.Trim().TrimEnd("\") -ieq $BinDir.TrimEnd("\") }) {
        return
    }
    $new = if ($current.EndsWith(";")) { "$current$BinDir" } else { "$current;$BinDir" }
    [Environment]::SetEnvironmentVariable("PATH", $new, $scope)
    Write-Ok "Added '$BinDir' to PATH"
}

# ─── Entry point ──────────────────────────────────────────────────────────────

function Invoke-Main {
    Write-Step "Installing Rake to $InstallRoot"

    if ($Source) {
        Write-Step "Using local binary $Source"
        Install-Exe $Source
    } else {
        $assetName = "rake-$(Get-ArchSuffix).zip"
        Write-Step "Fetching $assetName"
        $url = Resolve-Release $assetName

        $zip = Join-Path $env:TEMP $assetName
        Write-Step "Downloading"
        $ProgressPreference = "SilentlyContinue"
        Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing -ErrorAction Stop

        Write-Step "Verifying checksum"
        $expected = Get-Checksum $url
        if (-not $expected) {
            Remove-Item -LiteralPath $zip -Force -ErrorAction SilentlyContinue
            Stop-Install "No usable SHA-256 published for this release; refusing to install"
        }
        $actual = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLower()
        if ($actual -ne $expected.ToLower()) {
            Remove-Item -LiteralPath $zip -Force -ErrorAction SilentlyContinue
            Stop-Install "Checksum mismatch`n  Expected: $expected`n  Actual:   $actual"
        }
        Write-Ok "Checksum verified"

        Write-Step "Extracting rake.exe"
        $stage = Join-Path $env:TEMP "rake-bootstrap-extract"
        Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
        Expand-Archive -LiteralPath $zip -DestinationPath $stage -Force
        Remove-Item -LiteralPath $zip -Force -ErrorAction SilentlyContinue

        $exe = Join-Path $stage "rake.exe"
        Install-Exe $exe
        Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
    }

    Add-ToPath

    Write-Step "Rake installed."
    Write-Host "  Binary: $ExePath"
    Write-Host "  Open a new terminal, then run 'rake --help'."
}

# Run unless this file is being dot-sourced, which is how the functions above get
# exercised without touching the machine: dot-sourcing defines them and returns.
#
# Verified against all four invocation styles: running the file, `.`-importing it,
# `iwr | iex`, and `& ([scriptblock]::Create(...))` all run the installer, and only
# dot-sourcing skips it.
if ($MyInvocation.InvocationName -ne '.') {
    try {
        Invoke-Main
    } catch {
        Write-Err $_.Exception.Message
        # Re-throw rather than exiting: `exit` would close the caller's terminal
        # when the script is run with Invoke-Expression. The re-thrown error still
        # makes `powershell -Command` return 1, so scripted callers keep working.
        throw
    }
}
