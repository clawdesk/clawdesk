# ClawDesk Installer for Windows
#
# Usage:
#   irm https://get.clawdesk.dev/install.ps1 | iex
#
# Environment variables:
#   $env:CLAWDESK_VERSION     — Pin a specific version
#   $env:CLAWDESK_INSTALL     — Custom install directory
#   $env:CLAWDESK_NO_DAEMON   — Skip daemon installation (set to 1)

$ErrorActionPreference = "Stop"

$REPO = "clawdesk/clawdesk"
$BINARY_NAME = "clawdesk"

# ---- Utility Functions -------------------------------------------------------

function Write-Info($msg)    { Write-Host "  info  " -ForegroundColor Blue -NoNewline; Write-Host $msg }
function Write-Success($msg) { Write-Host "    ✓   " -ForegroundColor Green -NoNewline; Write-Host $msg }
function Write-Warn($msg)    { Write-Host "  warn  " -ForegroundColor Yellow -NoNewline; Write-Host $msg }

# ---- Platform Detection ------------------------------------------------------

$Arch = if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq "Arm64") {
    "arm64"
} else {
    "amd64"
}
$Target = "windows-$Arch"
Write-Info "Detected platform: $Target"

# ---- Version Resolution ------------------------------------------------------

if ($env:CLAWDESK_VERSION) {
    $Version = $env:CLAWDESK_VERSION
    Write-Info "Using pinned version: $Version"
} else {
    Write-Info "Resolving latest version..."
    try {
        $Release = Invoke-RestMethod "https://api.github.com/repos/$REPO/releases/latest"
        $Version = $Release.tag_name -replace '^v', ''
    } catch {
        $Version = "0.1.0"
        Write-Warn "Could not resolve latest version, using $Version"
    }
    Write-Info "Latest version: $Version"
}

# ---- Download ----------------------------------------------------------------

$BinaryFile = "$BINARY_NAME-$Target.exe"
$DownloadUrl = "https://github.com/$REPO/releases/download/v$Version/$BinaryFile"
$ChecksumUrl = "https://github.com/$REPO/releases/download/v$Version/checksums.sha256"

$TmpDir = Join-Path $env:TEMP "clawdesk-install"
New-Item -ItemType Directory -Force -Path $TmpDir | Out-Null

Write-Info "Downloading $BinaryFile..."
try {
    Invoke-WebRequest -Uri $DownloadUrl -OutFile (Join-Path $TmpDir $BinaryFile) -UseBasicParsing
    Write-Success "Downloaded"
} catch {
    Write-Host "  error " -ForegroundColor Red -NoNewline
    Write-Host "Download failed: $DownloadUrl"
    exit 1
}

# Verify checksum.
try {
    Invoke-WebRequest -Uri $ChecksumUrl -OutFile (Join-Path $TmpDir "checksums.sha256") -UseBasicParsing
    $Expected = (Get-Content (Join-Path $TmpDir "checksums.sha256") | Where-Object { $_ -match $BinaryFile }) -split '\s+' | Select-Object -First 1
    if ($Expected) {
        $Actual = (Get-FileHash -Path (Join-Path $TmpDir $BinaryFile) -Algorithm SHA256).Hash.ToLower()
        if ($Actual -eq $Expected.ToLower()) {
            Write-Success "SHA-256 checksum verified"
        } else {
            Write-Host "  error " -ForegroundColor Red -NoNewline
            Write-Host "Checksum mismatch!"
            exit 1
        }
    }
} catch {
    Write-Warn "Checksum verification skipped"
}

# ---- Install -----------------------------------------------------------------

if ($env:CLAWDESK_INSTALL) {
    $InstallDir = $env:CLAWDESK_INSTALL
} else {
    $InstallDir = Join-Path $env:LOCALAPPDATA "ClawDesk"
}

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

$InstallPath = Join-Path $InstallDir "$BINARY_NAME.exe"

# Backup if upgrading.
if (Test-Path $InstallPath) {
    $ExistingVer = & $InstallPath --version 2>$null | Select-Object -Last 1
    Write-Info "Upgrading from $ExistingVer to $Version"
    Copy-Item $InstallPath "$InstallPath.bak" -Force
}

# Atomic-ish install.
Copy-Item (Join-Path $TmpDir $BinaryFile) $InstallPath -Force
Write-Success "Installed to $InstallPath"

# Add to PATH if not already there.
$UserPath = [Environment]::GetEnvironmentVariable("PATH", "User")
if ($UserPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable("PATH", "$InstallDir;$UserPath", "User")
    $env:PATH = "$InstallDir;$env:PATH"
    Write-Success "Added $InstallDir to PATH"
}

# ---- Shell Completions -------------------------------------------------------

try {
    & $InstallPath completions powershell > (Join-Path $InstallDir "clawdesk.ps1") 2>$null
    Write-Success "Installed PowerShell completions"
} catch { }

# ---- Daemon ------------------------------------------------------------------

if (-not $env:CLAWDESK_NO_DAEMON) {
    Write-Info "Installing background daemon..."
    try {
        & $InstallPath daemon install 2>$null
        Write-Success "Daemon service registered"
        & $InstallPath daemon start 2>$null
        Write-Success "Daemon started"
    } catch {
        Write-Warn "Daemon installation skipped (run 'clawdesk daemon install' manually)"
    }
}

# ---- Done --------------------------------------------------------------------

# Cleanup.
Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue

Write-Host ""
Write-Host "  Installation complete!" -ForegroundColor Green
Write-Host ""
if (-not (Test-Path (Join-Path $env:USERPROFILE ".clawdesk\config.toml"))) {
    Write-Host "  Next steps:"
    Write-Host "    1. clawdesk init       — Configure providers and channels"
    Write-Host "    2. clawdesk            — Start chatting"
} else {
    Write-Host "  Ready! Run 'clawdesk daemon status' to check the gateway."
}
Write-Host ""
