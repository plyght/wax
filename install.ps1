# Wax Windows installer
# One-liner: irm https://raw.githubusercontent.com/plyght/wax/master/install.ps1 | iex
# From clone: .\install.ps1
# Force release binary in clone: $env:WAX_USE_RELEASE = '1'; .\install.ps1
#
# Note: #Requires cannot be used here - it breaks Invoke-Expression (iex) pipelines.

$ErrorActionPreference = 'Stop'

if ($PSVersionTable.PSVersion.Major -lt 5) {
    throw 'PowerShell 5.1 or later is required.'
}
if ($PSVersionTable.PSVersion.Major -eq 5 -and $PSVersionTable.PSVersion.Minor -lt 1) {
    throw 'PowerShell 5.1 or later is required.'
}

$Repo = 'plyght/wax'
if (-not $env:USERPROFILE) {
    throw 'USERPROFILE is not set; cannot determine install directory.'
}
$installDir = if ($env:WAX_INSTALL_DIR) {
    $env:WAX_INSTALL_DIR
} else {
    Join-Path $env:USERPROFILE '.local\bin'
}

function Install-FromRepo {
    param([string]$Root)
    if (-not $Root) {
        throw 'Install-FromRepo: root path is empty.'
    }
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
        $assetNames = @($rel.assets | ForEach-Object { $_.name })
        if ($assetNames -notcontains $asset) {
            $available = $assetNames -join ', '
            throw ('Latest release ({0}) has no Windows binary ({1}). Available: {2}. Clone https://github.com/plyght/wax and run install.ps1 locally, or set WAX_VERSION to a release with Windows assets.' -f $version, $asset, $available)
        }
    }
    if ($version -notmatch '^v') {
        $version = 'v' + $version
    }

    $base = ('https://github.com/{0}/releases/download/{1}' -f $Repo, $version)
    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ('wax-install-' + [System.IO.Path]::GetRandomFileName())

    try {
        Write-Host ('Installing wax {0} ({1}) from GitHub Releases...' -f $version, $archLabel)
        $exeUri = ('{0}/{1}' -f $base, $asset)
        try {
            Invoke-WebRequest -Uri $exeUri -OutFile $tmp -UseBasicParsing
        } catch {
            $dlErr = $_.Exception.Message
            throw ('Failed to download {0} from {1}. Clone the repo and run install.ps1 locally, or set WAX_VERSION. Error: {2}' -f $asset, $exeUri, $dlErr)
        }

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
        if ($tmp -and (Test-Path -LiteralPath $tmp)) {
            Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
        }
    }
}

# iex/irm has no $PSScriptRoot - never Join-Path or Test-Path on an empty root.
$repoRoot = $PSScriptRoot
$invokedAsFile = $PSCommandPath -and ((Split-Path -Leaf $PSCommandPath) -eq 'install.ps1')
$useLocalBuild = $false

if ($invokedAsFile -and $repoRoot -and ($env:WAX_USE_RELEASE -ne '1')) {
    $cargoTomlPath = Join-Path $repoRoot 'Cargo.toml'
    if (Test-Path -LiteralPath $cargoTomlPath) {
        $tomlRaw = Get-Content -LiteralPath $cargoTomlPath -Raw
        $q = [char]34
        $needle = [string]::Concat('name = ', $q, 'waxpkg', $q)
        if ($tomlRaw.IndexOf($needle, [System.StringComparison]::Ordinal) -ge 0) {
            $useLocalBuild = $true
        }
    }
}

if ($useLocalBuild) {
    Install-FromRepo -Root $repoRoot
} else {
    Install-FromRelease
}