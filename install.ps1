# Wax Windows installer
# One-liner: irm https://raw.githubusercontent.com/plyght/wax/master/install.ps1 | iex
# From clone: .\install.ps1
# Force release binary in clone: $env:WAX_USE_RELEASE = '1'; .\install.ps1
#
# Note: #Requires cannot be used here - it breaks Invoke-Expression (iex) pipelines.

$ErrorActionPreference = 'Stop'

# GitHub requires TLS 1.2; Windows PowerShell 5.1 defaults to older protocols.
try {
    $tls = [Net.ServicePointManager]::SecurityProtocol
    if ($tls -band [Net.SecurityProtocolType]::Tls12 -eq 0) {
        [Net.ServicePointManager]::SecurityProtocol = $tls -bor [Net.SecurityProtocolType]::Tls12
    }
} catch {
    # Non-fatal on hosts where ServicePointManager is unavailable.
}

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

$script:WebHeaders = @{ 'User-Agent' = 'wax-install-ps1' }

function Get-WebText {
    param([string]$Uri)
    # OutFile matches the release binary download path; .Content is unreliable for
    # application/octet-stream responses under Windows PowerShell 5.1.
    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ('wax-dl-' + [System.IO.Path]::GetRandomFileName())
    try {
        Invoke-WebRequest -Uri $Uri -OutFile $tmp -UseBasicParsing -Headers $script:WebHeaders
        return Get-Content -LiteralPath $tmp -Raw -Encoding UTF8
    } finally {
        if (Test-Path -LiteralPath $tmp) {
            Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
        }
    }
}

function Read-ExpectedSha256 {
    param([string]$Uri)
    $raw = (Get-WebText -Uri $Uri).Trim()
    if ($raw.Length -ge 1 -and [int][char]$raw[0] -eq 0xFEFF) {
        $raw = $raw.Substring(1).Trim()
    }
    $hash = ($raw -split '\s+' | Where-Object { $_ } | Select-Object -First 1)
    if (-not $hash -or $hash -notmatch '^[0-9a-fA-F]{64}$') {
        throw ('Invalid checksum file at {0} (expected 64 hex chars, got: {1})' -f $Uri, $raw)
    }
    return $hash.ToLowerInvariant()
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
    Ensure-WaxPath
}

function Show-WaxPathInstructions {
    $dir = $installDir.TrimEnd('\')
    Write-Host ''
    Write-Host 'wax.exe is installed but not on your PATH. Add this folder:'
    Write-Host ('  {0}' -f $dir)
    Write-Host ''
    Write-Host 'Windows 11 / 10 (Settings):'
    Write-Host '  1. Press Win, search "environment variables"'
    Write-Host '  2. Open "Edit environment variables for your account"'
    Write-Host '  3. Select Path -> Edit -> New'
    Write-Host ('  4. Paste: {0}' -f $dir)
    Write-Host '  5. OK, then open a new PowerShell window'
    Write-Host ''
    Write-Host 'PowerShell (permanent, current user):'
    Write-Host ('  [Environment]::SetEnvironmentVariable(''Path'', [Environment]::GetEnvironmentVariable(''Path'',''User'') + '';' + $dir + ''', ''User'')')
    Write-Host ''
    Write-Host 'PowerShell (this session only):'
    Write-Host ('  $env:PATH += '';' + $dir)
    Write-Host ''
    Write-Host 'Command Prompt (permanent, current user):'
    Write-Host ('  setx PATH "%PATH%;{0}"' -f $dir)
}

function Ensure-WaxPath {
    $dir = $installDir.TrimEnd('\')
    $pathEntries = @($env:PATH -split ';' | ForEach-Object { $_.TrimEnd('\') })
    if ($dir -in $pathEntries) {
        return
    }

    $added = $false
    try {
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        if (-not $userPath) {
            $userPath = ''
        }
        $userEntries = @($userPath -split ';' | Where-Object { $_ } | ForEach-Object { $_.TrimEnd('\') })
        if ($dir -notin $userEntries) {
            $newUserPath = if ($userPath -and -not $userPath.EndsWith(';')) {
                $userPath + ';' + $dir
            } elseif ($userPath) {
                $userPath + $dir
            } else {
                $dir
            }
            [Environment]::SetEnvironmentVariable('Path', $newUserPath, 'User')
            if ($env:PATH) {
                $env:PATH = $env:PATH.TrimEnd(';') + ';' + $dir
            } else {
                $env:PATH = $dir
            }
            Write-Host ('Added {0} to your user PATH.' -f $dir)
            $added = $true
        }
    } catch {
        Write-Warning ('Could not update user PATH automatically: {0}' -f $_.Exception.Message)
    }

    if ($added) {
        if (-not (Get-Command wax -ErrorAction SilentlyContinue)) {
            Write-Host 'Open a new terminal, or run this in the current session:'
            Write-Host ('  $env:PATH += '';' + $dir)
        }
    } else {
        Show-WaxPathInstructions
    }
}

function Get-WaxWindowsAsset {
    # PROCESSOR_ARCHITEW6432 is set for WOW64 (32-bit PowerShell on 64-bit Windows).
    $procArch = if ($env:PROCESSOR_ARCHITEW6432) {
        $env:PROCESSOR_ARCHITEW6432
    } else {
        $env:PROCESSOR_ARCHITECTURE
    }

    switch ($procArch) {
        'AMD64' { return 'wax-windows-x64.exe' }
        'ARM64' { return 'wax-windows-arm64.exe' }
    }

    $osArch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    switch ($osArch) {
        'X64' { return 'wax-windows-x64.exe' }
        'Arm64' { return 'wax-windows-arm64.exe' }
    }

    $detail = ('PROCESSOR_ARCHITECTURE={0}; PROCESSOR_ARCHITEW6432={1}; OSArchitecture={2}' -f $env:PROCESSOR_ARCHITECTURE, $env:PROCESSOR_ARCHITEW6432, $osArch)
    throw ('Unsupported Windows CPU architecture for pre-built wax ({0}). Clone https://github.com/plyght/wax and run install.ps1 to build locally.' -f $detail)
}

function Install-FromRelease {
    if (-not [Environment]::Is64BitOperatingSystem) {
        Write-Error 'Wax pre-built Windows installers require 64-bit Windows.'
    }

    $asset = Get-WaxWindowsAsset

    $archLabel = if ($asset -match 'arm64') { 'windows/arm64' } else { 'windows/x64' }

    $version = $env:WAX_VERSION
    if (-not $version) {
        $releaseUri = ('https://api.github.com/repos/{0}/releases/latest' -f $Repo)
        $rel = Invoke-RestMethod -Uri $releaseUri -Headers $script:WebHeaders
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
            Invoke-WebRequest -Uri $exeUri -OutFile $tmp -UseBasicParsing -Headers $script:WebHeaders
        } catch {
            $dlErr = $_.Exception.Message
            throw ('Failed to download {0} from {1}. Clone the repo and run install.ps1 locally, or set WAX_VERSION. Error: {2}' -f $asset, $exeUri, $dlErr)
        }

        $shaUri = ('{0}/{1}.sha256' -f $base, $asset)
        $expected = $null
        try {
            $expected = Read-ExpectedSha256 -Uri $shaUri
        } catch {
            if ($env:WAX_NO_VERIFY -eq '1') {
                Write-Warning ('WAX_NO_VERIFY=1 set - installing {0} without checksum verification' -f $version)
            } else {
                throw ('Checksum verification failed for {0}: {1}' -f $shaUri, $_.Exception.Message)
            }
        }

        if ($expected) {
            $hash = (Get-FileHash -LiteralPath $tmp -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($hash -ne $expected) {
                throw ('SHA256 mismatch (expected {0}, got {1})' -f $expected, $hash)
            }
            Write-Host 'Checksum verified.'
        }

        New-Item -ItemType Directory -Force -Path $installDir | Out-Null
        $dest = Join-Path $installDir 'wax.exe'
        Move-Item -LiteralPath $tmp -Destination $dest -Force
        Write-Host ('Installed to {0}' -f $dest)

        Ensure-WaxPath
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