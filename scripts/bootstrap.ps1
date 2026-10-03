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

.EXAMPLE
    iwr -useb https://raw.githubusercontent.com/nidara-duo/rake/main/scripts/bootstrap.ps1 | iex
    .\bootstrap.ps1 -Source ..\target\release\rake.exe
#>

param(
    [string]$Source = ""
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

function Get-LatestAssetUrl {
    param([string]$AssetName)

    $release = Invoke-RestMethod -Uri "$ApiUrl/latest" -UseBasicParsing -ErrorAction Stop
    $asset = $release.assets | Where-Object { $_.name -eq $AssetName } | Select-Object -First 1
    if (-not $asset) {
        Write-Err "Release $($release.tag_name) has no asset named $AssetName"
        exit 1
    }
    Write-Ok "Latest: $($release.tag_name)"
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
        Write-Err "Binary not found: $ExeSource"
        exit 1
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

try {
    Write-Step "Installing Rake to $InstallRoot"

    if ($Source) {
        Write-Step "Using local binary $Source"
        Install-Exe $Source
    } else {
        $assetName = "rake-$(Get-ArchSuffix).zip"
        Write-Step "Fetching $assetName"
        $url = Get-LatestAssetUrl $assetName

        $zip = Join-Path $env:TEMP $assetName
        Write-Step "Downloading"
        $ProgressPreference = "SilentlyContinue"
        Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing -ErrorAction Stop

        Write-Step "Verifying checksum"
        $expected = Get-Checksum $url
        if (-not $expected) {
            Write-Err "No usable SHA-256 published for this release; refusing to install"
            Remove-Item -LiteralPath $zip -Force -ErrorAction SilentlyContinue
            exit 1
        }
        $actual = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLower()
        if ($actual -ne $expected.ToLower()) {
            Write-Err "Checksum mismatch"
            Write-Err "  Expected: $expected"
            Write-Err "  Actual:   $actual"
            Remove-Item -LiteralPath $zip -Force -ErrorAction SilentlyContinue
            exit 1
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
} catch {
    Write-Err $_.Exception.Message
    exit 1
}
