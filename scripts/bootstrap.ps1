<#
.SYNOPSIS
    Bootstrap installer / updater for Rake (Scoop-compatible package manager).

.DESCRIPTION
    Installs, updates, or uninstalls Rake itself. Downloads release assets
    from GitHub, verifies SHA-256 checksums, and manages the installation
    directory and PATH.

.PARAMETER Action
    install   – Download and install the latest stable Rake.
    update    – Update Rake to the latest stable version (replaces binary).
    uninstall – Remove Rake and clean up.

.EXAMPLE
    .\bootstrap.ps1 install
    .\bootstrap.ps1 update
    .\bootstrap.ps1 uninstall
#>

param(
    [Parameter(Position = 0)]
    [ValidateSet("install", "update", "uninstall")]
    [string]$Action = "install"
)

# ─── Configuration ───────────────────────────────────────────────────────────

$Repo = "nidara-duo/rake"
$ApiUrl = "https://api.github.com/repos/$Repo/releases"
$InstallRoot = "$env:LOCALAPPDATA\rake"
$BinDir = "$InstallRoot\bin"
$ExePath = "$BinDir\rake.exe"
$TempDir = "$env:TEMP\rake-bootstrap"

# ─── Helpers ─────────────────────────────────────────────────────────────────

function Write-Step {
    param([string]$Message)
    Write-Host "==> $Message" -ForegroundColor Cyan
}

function Write-Error {
    param([string]$Message)
    Write-Host "ERROR: $Message" -ForegroundColor Red
}

function Write-Ok {
    param([string]$Message)
    Write-Host "  OK $Message" -ForegroundColor Green
}

function Clean-Temp {
    if (Test-Path $TempDir) {
        Remove-Item -Recurse -Force $TempDir -ErrorAction SilentlyContinue
    }
}

function Assert-Admin {
    $id = [System.Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object System.Security.Principal.WindowsPrincipal($id)
    if (-not $principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)) {
        Write-Error "This action requires administrator privileges."
        exit 1
    }
}

function Add-ToPath {
    param([string]$Dir)
    $scope = if ($Action -eq "install" -or $Action -eq "update") { "User" } else { "User" }
    $current = [Environment]::GetEnvironmentVariable("PATH", $scope)
    if ($current -split ";" -notcontains $Dir) {
        $newPath = if ($current.EndsWith(";")) { "$current$Dir" } else { "$current;$Dir" }
        [Environment]::SetEnvironmentVariable("PATH", $newPath, $scope)
        Write-Ok "Added '$Dir' to PATH"
    }
}

function Remove-FromPath {
    param([string]$Dir)
    $scope = "User"
    $current = [Environment]::GetEnvironmentVariable("PATH", $scope)
    $entries = $current -split ";" | Where-Object { $_ -ne "" -and $_ -ne $Dir }
    $newPath = $entries -join ";"
    [Environment]::SetEnvironmentVariable("PATH", $newPath, $scope)
    Write-Ok "Removed '$Dir' from PATH"
}

function Get-ArchSuffix {
    $arch = if ([Environment]::Is64BitOperatingSystem) {
        if ([Environment]::GetEnvironmentVariable("PROCESSOR_ARCHITECTURE") -eq "ARM64") {
            "aarch64"
        } else {
            "x86_64"
        }
    } else {
        "i686"
    }
    return "$arch-pc-windows-msvc"
}

function Get-LatestRelease {
    param([switch]$PreRelease)
    $url = "$ApiUrl/latest"
    try {
        $release = Invoke-RestMethod -Uri $url -UseBasicParsing -ErrorAction Stop
        return $release
    } catch {
        if (-not $PreRelease) {
            try {
                $all = Invoke-RestMethod -Uri $ApiUrl -UseBasicParsing -ErrorAction Stop
                $stable = $all | Where-Object { -not $_.prerelease -and $_.tag_name -match '^v\d+\.\d+\.\d+$' } | Select-Object -First 1
                if ($stable) { return $stable }
            } catch {}
        }
        return $null
    }
}

function Get-Asset {
    param([object]$Release, [string]$Suffix)
    $assetName = "rake-$Suffix.zip"
    $asset = $Release.assets | Where-Object { $_.name -eq $assetName }
    if (-not $asset) { return $null }
    return @{
        Name     = $asset.name
        Url      = $asset.browser_download_url
        Size     = $asset.size
    }
}

function Get-Checksum {
    param([object]$Release, [string]$ZipName)
    $sumFile = "$ZipName.sha256"
    $asset = $Release.assets | Where-Object { $_.name -eq $sumFile }
    if (-not $asset) { return $null }
    try {
        $content = Invoke-RestMethod -Uri $asset.browser_download_url -UseBasicParsing -ErrorAction Stop
        $content = $content.Trim()
        if ($content -match '^([a-f0-9]{64})\s') {
            return $matches[1]
        }
        if ($content -match '^([a-f0-9]{64})$') {
            return $matches[1]
        }
        return $null
    } catch {
        return $null
    }
}

function Download-File {
    param([string]$Url, [string]$OutFile)
    Write-Step "Downloading $Url"
    $ProgressPreference = "SilentlyContinue"
    Invoke-WebRequest -Uri $Url -OutFile $OutFile -UseBasicParsing -ErrorAction Stop
    if (-not (Test-Path $OutFile)) {
        throw "Download failed: $OutFile not created"
    }
}

function Verify-Checksum {
    param([string]$FilePath, [string]$ExpectedHash)
    if (-not $ExpectedHash) {
        Write-Error "No checksum available for verification, skipping"
        return $false
    }
    $actual = (Get-FileHash $FilePath -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $ExpectedHash.ToLower()) {
        Write-Error "Checksum mismatch!"
        Write-Error "  Expected: $ExpectedHash"
        Write-Error "  Actual:   $actual"
        return $false
    }
    Write-Ok "Checksum verified"
    return $true
}

function Install-Binary {
    param([string]$ZipPath)
    Ensure-Directory $BinDir
    Write-Step "Extracting $ZipPath → $BinDir"
    Expand-Archive -Path $ZipPath -DestinationPath $BinDir -Force
    if (-not (Test-Path $ExePath)) {
        throw "rake.exe not found after extraction"
    }
    Write-Ok "Installed $ExePath"
}

function Ensure-Directory {
    param([string]$Path)
    if (-not (Test-Path $Path)) {
        New-Item -ItemType Directory -Path $Path -Force | Out-Null
    }
}

function Backup-Exe {
    $backup = "$BinDir\rake.exe.old"
    if (Test-Path $ExePath) {
        Copy-Item -Path $ExePath -Destination $backup -Force
        return $backup
    }
    return $null
}

function Restore-Backup {
    param([string]$BackupPath)
    if ($BackupPath -and (Test-Path $BackupPath)) {
        Move-Item -Path $BackupPath -Destination $ExePath -Force
        Write-Ok "Restored previous version"
    }
}

# ─── Actions ─────────────────────────────────────────────────────────────────

function Action-Install {
    Write-Step "Installing Rake to $InstallRoot"

    Clean-Temp
    Ensure-Directory $TempDir

    # Resolve arch
    $suffix = Get-ArchSuffix
    Write-Step "Target architecture: $suffix"

    # Fetch latest release
    Write-Step "Querying latest release from $Repo"
    $release = Get-LatestRelease
    if (-not $release) {
        Write-Error "Could not find any stable release for $Repo"
        exit 1
    }
    Write-Ok "Latest: $($release.tag_name)"

    # Locate asset
    $asset = Get-Asset $release $suffix
    if (-not $asset) {
        Write-Error "No asset found for architecture '$suffix' in release $($release.tag_name)"
        exit 1
    }

    $zipPath = "$TempDir\$($asset.Name)"

    # Download archive
    Download-File -Url $asset.Url -OutFile $zipPath

    # Verify checksum
    Write-Step "Verifying checksum"
    $expectedHash = Get-Checksum $release $asset.Name
    if (-not (Verify-Checksum -FilePath $zipPath -ExpectedHash $expectedHash)) {
        Clean-Temp
        exit 1
    }

    # Install
    Ensure-Directory $BinDir
    Install-Binary $zipPath

    # Copy bootstrap alongside the binary
    if ($PSCommandPath -and (Test-Path $PSCommandPath)) {
        Copy-Item -Path $PSCommandPath -Destination "$BinDir\bootstrap.ps1" -Force
        Write-Ok "Copied bootstrap.ps1 alongside rake.exe"
    }

    # PATH
    Add-ToPath $BinDir

    # Cleanup
    Clean-Temp

    Write-Step "Rake $($release.tag_name) installed successfully!"
    Write-Host "  Binary: $ExePath"
    Write-Host "  Run 'rake --help' to get started."
}

function Action-Update {
    Write-Step "Updating Rake"

    if (-not (Test-Path $ExePath)) {
        Write-Error "Rake is not installed. Run 'bootstrap.ps1 install' first."
        exit 1
    }

    Clean-Temp
    Ensure-Directory $TempDir

    # Resolve arch
    $suffix = Get-ArchSuffix
    Write-Step "Target architecture: $suffix"

    # Fetch latest release
    Write-Step "Querying latest release from $Repo"
    $release = Get-LatestRelease
    if (-not $release) {
        Write-Error "Could not find any stable release for $Repo"
        exit 1
    }
    Write-Ok "Latest: $($release.tag_name)"

    # Locate asset
    $asset = Get-Asset $release $suffix
    if (-not $asset) {
        Write-Error "No asset found for architecture '$suffix' in release $($release.tag_name)"
        exit 1
    }

    $zipPath = "$TempDir\$($asset.Name)"

    # Download archive
    Download-File -Url $asset.Url -OutFile $zipPath

    # Verify checksum
    Write-Step "Verifying checksum"
    $expectedHash = Get-Checksum $release $asset.Name
    if (-not (Verify-Checksum -FilePath $zipPath -ExpectedHash $expectedHash)) {
        Clean-Temp
        exit 1
    }

    # Backup current binary
    Write-Step "Safely replacing rake.exe"
    $backup = Backup-Exe

    try {
        Install-Binary $zipPath
        if ($PSCommandPath -and (Test-Path $PSCommandPath)) {
            Copy-Item -Path $PSCommandPath -Destination "$BinDir\bootstrap.ps1" -Force
        }

        # Remove backup on success
        if ($backup -and (Test-Path $backup)) {
            Remove-Item -Path $backup -Force
        }

        Clean-Temp
        Write-Step "Rake updated to $($release.tag_name)!"
    } catch {
        Write-Error "Update failed: $_"
        Restore-Backup $backup
        Clean-Temp
        exit 1
    }
}

function Action-Uninstall {
    Write-Step "Uninstalling Rake"

    if (-not (Test-Path $ExePath)) {
        Write-Ok "Rake is not installed"
    } else {
        Remove-Item -Path $ExePath -Force
        Write-Ok "Removed $ExePath"
    }

    # Remove bootstrap.ps1 from bin dir
    $bsInBin = "$BinDir\bootstrap.ps1"
    if (Test-Path $bsInBin) {
        Remove-Item -Path $bsInBin -Force
    }

    # Remove bin dir if empty
    if (Test-Path $BinDir) {
        $remaining = Get-ChildItem $BinDir -ErrorAction SilentlyContinue
        if (-not $remaining) {
            Remove-Item -Path $BinDir -Force
            Write-Ok "Removed $BinDir"
        }
    }

    # PATH cleanup
    Remove-FromPath $BinDir

    # Ask about full install root removal
    if (Test-Path $InstallRoot) {
        $remaining = Get-ChildItem $InstallRoot -Recurse -ErrorAction SilentlyContinue
        if ($remaining) {
            Write-Host ""
            Write-Host "Note: $InstallRoot still contains files (apps, cache, etc.)."
            Write-Host "Remove them manually if no longer needed."
        } else {
            Remove-Item -Path $InstallRoot -Force
            Write-Ok "Removed $InstallRoot"
        }
    }

    Write-Step "Rake has been uninstalled."
}

# ─── Entry Point ─────────────────────────────────────────────────────────────

try {
    switch ($Action) {
        "install"   { Action-Install }
        "update"    { Action-Update }
        "uninstall" { Action-Uninstall }
    }
} catch {
    Write-Error "Unexpected error: $_"
    Clean-Temp
    exit 1
}
