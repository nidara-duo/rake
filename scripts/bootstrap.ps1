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

.PARAMETER Source
    Path to a locally built rake.exe. When set, no release download happens —
    the binary is packaged straight into the install payload. Useful for
    exercising the self-management flow without spending bandwidth.

.EXAMPLE
    .\bootstrap.ps1 install
    .\bootstrap.ps1 update
    .\bootstrap.ps1 uninstall
    .\bootstrap.ps1 update -Source ..\target\release\rake.exe
#>

param(
    [Parameter(Position = 0)]
    [ValidateSet("install", "update", "uninstall")]
    [string]$Action = "install",

    [string]$Source = ""
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

function Add-ToPath {
    param([string]$Dir)
    $scope = "User"
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
    if (-not $current) { return }

    # Compare case-insensitively: PATH entries are Windows paths.
    $entries = $current -split ";" | Where-Object { $_ -ne "" -and $_ -ine $Dir }
    if ($entries.Count -eq ($current -split ";" | Where-Object { $_ -ne "" }).Count) {
        Write-Ok "'$Dir' was not in PATH"
        return
    }

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
    $url = "$ApiUrl/latest"
    try {
        $release = Invoke-RestMethod -Uri $url -UseBasicParsing -ErrorAction Stop
        return $release
    } catch {
        try {
            $all = Invoke-RestMethod -Uri $ApiUrl -UseBasicParsing -ErrorAction Stop
            $stable = $all | Where-Object { -not $_.prerelease -and $_.tag_name -match '^v\d+\.\d+\.\d+$' } | Select-Object -First 1
            if ($stable) { return $stable }
        } catch {}
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

# ─── Self-replacement ─────────────────────────────────────────────────────────
#
# Windows holds an exclusive lock on a running image: the running rake.exe can be
# neither overwritten nor deleted, and `Expand-Archive -Force` over it fails with
# "Access to the path ... is denied". Renaming it aside is permitted, which frees the
# original name for writing. The renamed file stays locked until this command exits,
# so what happens next depends on whether there will be another run to clean up:
#
#   update    — leave it behind as <binary>.old and let the next Rake startup sweep
#               it. This is what Scoop does with its old version directory.
#   uninstall — nothing will ever run again, so a detached helper has to finish it.
#
# `MoveFileEx` with MOVEFILE_DELAY_UNTIL_REBOOT would defer the delete to the OS, but
# it requires administrator rights and writes HKLM\...\PendingFileRenameOperations —
# unacceptable for a per-user tool, so it is not used. `FileDispositionInfoEx` with
# POSIX semantics would allow deleting an open image but was refused on the target
# system (win32err=24).

function Move-RunningExeAside {
    # Renames the running binary out of the way so a replacement can be written at the
    # original name. Returns the path it was moved to, or $null when there was nothing
    # to move.
    if (-not (Test-Path $ExePath)) { return $null }

    $stale = "$BinDir\rake.exe.old"

    # An earlier replacement may have left this name occupied. Normally it is already
    # gone — every Rake startup sweeps it — but an antivirus scanner can still be
    # holding a binary written moments ago, so retry briefly instead of failing. The
    # fixed name is deliberate: Scoop avoids collisions by appending an incrementing
    # suffix, which only works because it also has a directory scan to clean up
    # afterwards. Here that scan would run on every invocation, so a name that cannot
    # collide is worth more than tolerating one and reporting it clearly.
    for ($attempt = 1; $attempt -le 5; $attempt++) {
        if (-not (Test-Path $stale)) { break }
        try {
            Remove-Item -LiteralPath $stale -Force -ErrorAction Stop
        } catch {
            Start-Sleep -Milliseconds (200 * $attempt)
        }
    }

    if (Test-Path $stale) {
        # Thrown rather than exited so the caller's rollback path still runs.
        throw "Cannot replace $stale because another process is using it. Close any running Rake (or wait for an antivirus scan to finish) and try again."
    }

    Move-Item -LiteralPath $ExePath -Destination $stale -Force
    Write-Ok "Moved existing rake.exe aside as $(Split-Path -Leaf $stale)"
    return $stale
}

function Remove-EmptyDir {
    param([string]$Path)
    if (-not (Test-Path $Path)) { return }
    if (Get-ChildItem $Path -ErrorAction SilentlyContinue) { return }
    Remove-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
}

function Start-DelayedCleanup {
    # The renamed binary stays locked until this script — and the rake.exe that started
    # it — has exited, so a detached helper finishes the job afterwards. Sweeping the
    # leftover on the next startup, the way Scoop handles its old directory, would be
    # simpler, but it is not available here: uninstalling removes the very binary that
    # would do the sweeping, so no next run ever comes.
    param(
        [Parameter(Mandatory = $true)][string]$Stale,
        [Parameter(Mandatory = $true)][string]$BinDir,
        [Parameter(Mandatory = $true)][string]$InstallRoot
    )

    $script = Join-Path $env:TEMP ("rake-cleanup-{0}.cmd" -f $PID)
    $body = @'
@echo off
setlocal
set "OLD=%OLD%"
set "BIN=%BIN%"
set "ROOT=%ROOT%"

rem Wait for the owning rake.exe to exit, then delete the binary it renamed aside.
for /L %%i in (1,1,30) do (
    if exist "%OLD%" (
        ping -n 2 127.0.0.1 >nul
        del /f /q "%OLD%" >nul 2>&1
    )
)

call :prune "%BIN%"
call :prune "%ROOT%"
del /f /q "%~f0"
exit /b 0

:prune
if not exist "%~1" exit /b 0
rem "dir /b" prints one line per entry, so any output means the directory is not empty.
rem Note that "if not exist <dir>\*" is not a valid emptiness test — it matches the
rem directory itself even when empty — hence this loop.
for /f %%i in ('dir /b /a "%~1" 2^>nul') do exit /b 0
rmdir "%~1" >nul 2>&1
exit /b 0
'@
    $body = $body.Replace('%OLD%', $Stale).Replace('%BIN%', $BinDir).Replace('%ROOT%', $InstallRoot)

    try {
        Set-Content -LiteralPath $script -Value $body -Encoding ASCII -ErrorAction Stop
        Start-Process -FilePath "cmd.exe" -ArgumentList "/c", $script -WindowStyle Hidden -ErrorAction Stop | Out-Null
        Write-Ok "Removing $(Split-Path -Leaf $Stale) as soon as this command exits"
    } catch {
        Write-Host "  Note: could not schedule cleanup; remove $Stale manually." -ForegroundColor Yellow
    }
}

function Restore-MovedExe {
    param([string]$StalePath)
    if ($StalePath -and (Test-Path $StalePath)) {
        Move-Item -LiteralPath $StalePath -Destination $ExePath -Force
        Write-Ok "Restored previous rake.exe"
    }
}

function New-LocalPayload {
    # Packages a locally built binary into the same zip layout the CI release produces,
    # so both payload sources feed an identical install path.
    if (-not (Test-Path $Source)) {
        throw "Local source not found: $Source"
    }

    $stage = "$TempDir\stage"
    Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
    Ensure-Directory $stage

    Copy-Item -LiteralPath $Source -Destination "$stage\rake.exe" -Force
    if ($PSCommandPath -and (Test-Path $PSCommandPath)) {
        Copy-Item -LiteralPath $PSCommandPath -Destination "$stage\bootstrap.ps1" -Force
    }

    $payload = "$TempDir\rake-local.zip"
    Compress-Archive -Path "$stage\rake.exe", "$stage\bootstrap.ps1" -DestinationPath $payload -Force
    return $payload
}

function Resolve-Payload {
    if ($Source) {
        Write-Step "Using local payload from $Source"
        Ensure-Directory $TempDir
        return New-LocalPayload
    }

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

    return $zipPath
}

function Install-Binary {
    param([string]$ZipPath)

    Ensure-Directory $BinDir
    Write-Step "Extracting $ZipPath → $BinDir"

    $stale = Move-RunningExeAside
    try {
        Expand-Archive -Path $ZipPath -DestinationPath $BinDir -Force
    } catch {
        Restore-MovedExe $stale
        throw
    }

    if (-not (Test-Path $ExePath)) {
        Restore-MovedExe $stale
        throw "rake.exe not found after extraction"
    }

    Write-Ok "Installed $ExePath"

    if ($stale) {
        # Nothing to do here: the previous binary is unlocked the moment this command
        # returns, and the next Rake startup sweeps it.
        Write-Host "  $(Split-Path -Leaf $stale) will be removed on the next Rake run."
    }
}

function Ensure-Directory {
    param([string]$Path)
    if (-not (Test-Path $Path)) {
        New-Item -ItemType Directory -Path $Path -Force | Out-Null
    }
}

# ─── Actions ─────────────────────────────────────────────────────────────────

function Action-Install {
    Write-Step "Installing Rake to $InstallRoot"

    Clean-Temp
    Ensure-Directory $TempDir

    $payload = Resolve-Payload

    # Install
    Install-Binary $payload

    # PATH
    Add-ToPath $BinDir

    # Cleanup
    Clean-Temp

    if ($Source) {
        Write-Step "Rake installed successfully from local source!"
    } else {
        Write-Step "Rake installed successfully!"
    }
    Write-Host "  Binary: $ExePath"
    Write-Host "  Run 'rake --help' to get started."
}

function Action-Update {
    Write-Step "Updating Rake"

    if (-not (Test-Path $ExePath)) {
        Write-Error "Rake is not installed at $ExePath. Run 'rake self install' first."
        exit 1
    }

    Clean-Temp
    Ensure-Directory $TempDir

    $payload = Resolve-Payload

    Write-Step "Replacing rake.exe in place"
    try {
        Install-Binary $payload
    } catch {
        Write-Error "Update failed: $_"
        Clean-Temp
        exit 1
    }

    Clean-Temp
    Write-Step "Rake updated successfully!"
    if (-not $Source) {
        Write-Host "  Run 'rake --version' to confirm."
    }
}

function Action-Uninstall {
    Write-Step "Uninstalling Rake"

    # When invoked via `rake self uninstall`, this script runs as a child of the very
    # rake.exe being removed, so the binary is locked and cannot be deleted outright.
    # Rename it aside — permitted even while running — and let a detached helper delete
    # it once we exit. Reporting honestly matters more than reporting optimistically.
    $stale = $null
    if (Test-Path $ExePath) {
        $stale = Move-RunningExeAside
    } else {
        Write-Ok "Rake is not installed"
    }

    # bootstrap.ps1 is not running, so it goes immediately.
    if (Test-Path "$BinDir\bootstrap.ps1") {
        Remove-Item -LiteralPath "$BinDir\bootstrap.ps1" -Force -ErrorAction SilentlyContinue
    }

    # PATH cleanup — always, even if the binary is still finishing removal
    Remove-FromPath $BinDir

    if ($stale) {
        Start-DelayedCleanup -Stale $stale -BinDir $BinDir -InstallRoot $InstallRoot
        Write-Step "Rake has been uninstalled."
        Write-Host "  rake.exe is still running; it and its directories will be removed"
        Write-Host "  as soon as this command exits."
    } else {
        # Nothing was running, so nothing is locked and cleanup can finish here.
        Remove-EmptyDir $BinDir
        if (Test-Path $InstallRoot) {
            if (Get-ChildItem $InstallRoot -Recurse -ErrorAction SilentlyContinue) {
                Write-Host ""
                Write-Host "Note: $InstallRoot still contains files (apps, cache, etc.)."
                Write-Host "Remove them manually if no longer needed."
            } else {
                Remove-Item -LiteralPath $InstallRoot -Force -ErrorAction SilentlyContinue
                Write-Ok "Removed $InstallRoot"
            }
        }
        Write-Step "Rake has been uninstalled."
    }
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
