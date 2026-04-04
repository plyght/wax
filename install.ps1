#Requires -Version 5.1
# Wax installer — from a clone: builds with cargo. Otherwise: GitHub Releases.
# Usage:
#   irm https://raw.githubusercontent.com/semitechnological/wax/master/install.ps1 | iex
#   .\install.ps1
#   $env:WAX_USE_RELEASE = '1'; .\install.ps1   # force release download from a clone
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
        throw 'cargo not in PATH — install Rust from https://rustup.rs/ or set WAX_USE_RELEASE=1 to download a release binary.'
    }
    Write-Host "Building wax from local checkout ($Root)…"
    Push-Location $Root
    try {
        cargo build --release
    } finally {
        Pop-Location
    }
    $built = Join-Path $Root 'target\release\wax.exe'
    if (-not (Test-Path -LiteralPath $built)) {
        throw "Build finished but $built not found."
    }
    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    $dest = Join-Path $installDir 'wax.exe'
    Copy-Item -LiteralPath $built -Destination $dest -Force
    Write-Host "Installed to $dest"
    Hint-Path
}

function Hint-Path {
    $dirs = ($env:PATH -split ';' | ForEach-Object { $_.TrimEnd('\') })
    if ($installDir -notin $dirs) {
        Write-Host ''
        Write-Host 'Add this folder to your user PATH if `wax` is not found:'
        Write-Host "  $installDir"
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
        default { throw "Unsupported Windows CPU architecture for pre-built wax: $osArch (clone the repo and run .\install.ps1 to build)." }
    }

    $archLabel = if ($asset -match 'arm64') { 'windows/arm64' } else { 'windows/x64' }

    $version = $env:WAX_VERSION
    if (-not $version) {
        $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -Headers @{ 'User-Agent' = 'wax-install-ps1' }
        $version = $rel.tag_name
    }
    if ($version -notmatch '^v') {
        $version = "v$version"
    }

    $base = "https://github.com/$Repo/releases/download/$version"
    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("wax-install-" + [System.IO.Path]::GetRandomFileName())

    try {
        Write-Host "Installing wax $version ($archLabel) from GitHub Releases…"
        Invoke-WebRequest -Uri "$base/$asset" -OutFile $tmp -UseBasicParsing

        $expected = $null
        try {
            $raw = (Invoke-WebRequest -Uri "$base/$asset.sha256" -UseBasicParsing).Content.Trim()
            $expected = ($raw -split '\s+')[0]
        } catch {
            Write-Warning "No .sha256 file for $version — skipping integrity check"
        }

        if ($expected) {
            $hash = (Get-FileHash -LiteralPath $tmp -Algorithm SHA256).Hash
            if ($hash.ToLowerInvariant() -ne $expected.ToLowerInvariant()) {
                throw "SHA256 mismatch (expected $expected, got $hash)"
            }
            Write-Host 'Checksum verified.'
        }

        New-Item -ItemType Directory -Force -Path $installDir | Out-Null
        $dest = Join-Path $installDir 'wax.exe'
        Move-Item -LiteralPath $tmp -Destination $dest -Force
        Write-Host "Installed to $dest"

        Hint-Path
    } finally {
        if (Test-Path -LiteralPath $tmp) {
            Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
        }
    }
}

$repoRoot = $PSScriptRoot
$invokedAsThisScript = $PSCommandPath -and ((Split-Path -Leaf $PSCommandPath) -eq 'install.ps1')
if (
    $invokedAsThisScript -and
    $repoRoot -and
    ($env:WAX_USE_RELEASE -ne '1') -and
    (Test-Path -LiteralPath (Join-Path $repoRoot 'Cargo.toml')) -and
    (Select-String -LiteralPath (Join-Path $repoRoot 'Cargo.toml') -SimpleMatch 'name = "waxpkg"' -Quiet)
) {
    Install-FromRepo -Root $repoRoot
} else {
    Install-FromRelease
}
