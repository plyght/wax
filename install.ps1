#Requires -Version 5.1
# Wax installer - from a clone: builds with cargo. Otherwise: GitHub Releases.
# Usage:
#   irm https://raw.githubusercontent.com/semitechnological/wax/winget-integration/install.ps1 | iex
#   .\install.ps1
#   $env:WAX_USE_RELEASE = '1'; .\install.ps1
#
# Style: single-quoted strings where possible; expand with -f. ASCII punctuation only.
# Save as UTF-8 with BOM for Windows PowerShell 5.1.
#
param()

$ErrorActionPreference = 'Stop'

$Repo = 'semitechnological/wax'
$installDir = if ($env:WAX_INSTALL_DIR) {
    $env:WAX_INSTALL_DIR
} else {
    Join-Path $env:USERPROFILE '.local\bin'
}

function Install-FromRepo {
    param([string]$Root)
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw 'cargo not in PATH - install Rust from https://rustup.rs/ or set WAX_USE_RELEASE=1 to download a release binary.'
    }
    Write-Host ('Building wax from local checkout ({0})...' -f $Root)
    Push-Location $Root
    try {
        cargo build --release
    } finally {
        Pop-Location
    }
    $built = Join-Path $Root 'target\release\wax.exe'
    if (-not (Test-Path -LiteralPath $built)) {
        throw ('Build finished but {0} not found.' -f $built)
    }
    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    $dest = Join-Path $installDir 'wax.exe'
    Copy-Item -LiteralPath $built -Destination $dest -Force
    Write-Host ('Installed to {0}' -f $dest)
    Hint-Path
}

function Hint-Path {
    $dirs = ($env:PATH -split ';' | ForEach-Object { $_.TrimEnd('\') })
    if ($installDir -notin $dirs) {
        Write-Host ''
        Write-Host 'Add this folder to your user PATH if wax.exe is not found:'
        Write-Host ('  {0}' -f $installDir)
    }
}

function Install-FromRelease {
    if (-not [Environment]::Is64BitOperatingSystem) {
        Write-Error 'Wax pre-built Windows installers require 64-bit Windows.'
    }

    $osArch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    $asset = switch ($osArch) {
        ([System.Runtime.InteropServices.Architecture]::X64) { 'wax-windows-x64.exe' }
        ([System.Runtime.InteropServices.Architecture]::Arm64) { 'wax-windows-arm64.exe' }
        default {
            throw ('Unsupported Windows CPU architecture for pre-built wax: {0} (clone the repo and run install.ps1 to build).' -f $osArch)
        }
    }

    $archLabel = if ($asset -match 'arm64') { 'windows/arm64' } else { 'windows/x64' }

    $version = $env:WAX_VERSION
    if (-not $version) {
        $releaseUri = ('https://api.github.com/repos/{0}/releases/latest' -f $Repo)
        $rel = Invoke-RestMethod -Uri $releaseUri -Headers @{ 'User-Agent' = 'wax-install-ps1' }
        $version = $rel.tag_name
    }
    if ($version -notmatch '^v') {
        $version = 'v' + $version
    }

    $base = ('https://github.com/{0}/releases/download/{1}' -f $Repo, $version)
    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ('wax-install-' + [System.IO.Path]::GetRandomFileName())

    try {
        Write-Host ('Installing wax {0} ({1}) from GitHub Releases...' -f $version, $archLabel)
        $exeUri = ('{0}/{1}' -f $base, $asset)
        Invoke-WebRequest -Uri $exeUri -OutFile $tmp -UseBasicParsing

        $expected = $null
        try {
            $shaUri = ('{0}/{1}.sha256' -f $base, $asset)
            $raw = (Invoke-WebRequest -Uri $shaUri -UseBasicParsing).Content.Trim()
            $expected = ($raw -split '\s+')[0]
        } catch {
            Write-Warning ('No .sha256 file for {0} - skipping integrity check' -f $version)
        }

        if ($expected) {
            $hash = (Get-FileHash -LiteralPath $tmp -Algorithm SHA256).Hash
            if ($hash.ToLowerInvariant() -ne $expected.ToLowerInvariant()) {
                throw ('SHA256 mismatch (expected {0}, got {1})' -f $expected, $hash)
            }
            Write-Host 'Checksum verified.'
        }

        New-Item -ItemType Directory -Force -Path $installDir | Out-Null
        $dest = Join-Path $installDir 'wax.exe'
        Move-Item -LiteralPath $tmp -Destination $dest -Force
        Write-Host ('Installed to {0}' -f $dest)

        Hint-Path
    } finally {
        if (Test-Path -LiteralPath $tmp) {
            Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
        }
    }
}

$repoRoot = $PSScriptRoot
$invokedAsThisScript = $PSCommandPath -and ((Split-Path -Leaf $PSCommandPath) -eq 'install.ps1')

$cargoTomlPath = Join-Path $repoRoot 'Cargo.toml'
$cargoTomlIsWaxpkg = $false
if (Test-Path -LiteralPath $cargoTomlPath) {
    $tomlRaw = Get-Content -LiteralPath $cargoTomlPath -Raw
    $q = [char]34
    $needle = [string]::Concat('name = ', $q, 'waxpkg', $q)
    $cargoTomlIsWaxpkg = $tomlRaw.IndexOf($needle, [System.StringComparison]::Ordinal) -ge 0
}

if (
    $invokedAsThisScript -and
    $repoRoot -and
    ($env:WAX_USE_RELEASE -ne '1') -and
    $cargoTomlIsWaxpkg
) {
    Install-FromRepo -Root $repoRoot
} else {
    Install-FromRelease
}
