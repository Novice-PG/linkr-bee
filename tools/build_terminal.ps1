<#
.SYNOPSIS
  Build the Rust terminal (tools/linkr-cli) on Windows.

.DESCRIPTION
  The Windows counterpart of tools/build_terminal.sh: verifies the crate
  (fmt, clippy, tests), builds linkr.exe for the host or an explicit -Target
  triple, and copies the binary plus a SHA256SUMS file into
  dist\linkr-terminal-<slug>\. Add -Bundle to wrap the result in the
  self-extracting dist\linkr-bee-terminal.ps1 through
  tools\build_terminal_bundle.py (needs Python 3).

.PARAMETER Target
  Rust target triple, e.g. aarch64-pc-windows-msvc. The triple's std must be
  installed first: rustup target add <triple>.

.PARAMETER Dev
  Build the dev profile (target\debug) instead of release.

.PARAMETER NoVerify
  Skip cargo fmt --check, clippy and the test suite.

.PARAMETER Bundle
  Also generate dist\linkr-bee-terminal.ps1 from the built executable.

.PARAMETER Python
  Python interpreter used for -Bundle. Detected from python3/python/py when
  omitted.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File tools\build_terminal.ps1

.EXAMPLE
  tools\build_terminal.ps1 -Bundle

.EXAMPLE
  tools\build_terminal.ps1 -Target aarch64-pc-windows-msvc -NoVerify
#>
[CmdletBinding()]
param(
    [string]$Target = "",
    [switch]$Dev,
    [switch]$NoVerify,
    [switch]$Bundle,
    [string]$Python = ""
)

$ErrorActionPreference = 'Stop'
# cargo and the terminal write diagnostics to stderr; never let that turn into
# a terminating error under $ErrorActionPreference = 'Stop'.
$PSNativeCommandUseErrorActionPreference = $false

$repoDir = Split-Path -Parent $PSScriptRoot
$cliDir = Join-Path (Join-Path $repoDir 'tools') 'linkr-cli'

function Assert-Cargo {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw "cargo not found; install Rust from https://rustup.rs"
    }
}

function Find-Python {
    if ($Python) { return $Python }
    foreach ($candidate in @('python3', 'python', 'py')) {
        $command = Get-Command $candidate -ErrorAction SilentlyContinue
        if (-not $command) { continue }
        $probe = & $candidate --version 2>&1
        if ($LASTEXITCODE -eq 0 -and ("$probe" -match 'Python\s+3')) { return $candidate }
    }
    return $null
}

function Invoke-Cargo {
    param([string[]]$CargoArgs)
    Push-Location $cliDir
    try {
        Write-Host "==> cargo $($CargoArgs -join ' ')"
        cargo @CargoArgs
        if ($LASTEXITCODE -ne 0) { throw "cargo $($CargoArgs -join ' ') failed ($LASTEXITCODE)" }
    }
    finally {
        Pop-Location
    }
}

Assert-Cargo

if (-not $NoVerify) {
    Invoke-Cargo @('fmt', '--check')
    Invoke-Cargo @('clippy', '--all-targets', '--', '-D', 'warnings')
    Invoke-Cargo @('test')
}

$buildArgs = @('build')
if (-not $Dev) { $buildArgs += '--release' }
if ($Target) { $buildArgs += @('--target', $Target) }
Invoke-Cargo $buildArgs

$profileDir = if ($Dev) { 'debug' } else { 'release' }
$targetDir = if ($Target) { Join-Path (Join-Path $cliDir 'target') $Target } else { Join-Path $cliDir 'target' }
$binary = Join-Path (Join-Path $targetDir $profileDir) 'linkr.exe'
if (-not (Test-Path -LiteralPath $binary)) {
    throw "cargo finished but the executable is missing: $binary"
}

$slug = if ($Target) { $Target.ToLowerInvariant() } else { "windows-$env:PROCESSOR_ARCHITECTURE".ToLowerInvariant() }
$outDir = Join-Path (Join-Path $repoDir 'dist') "linkr-terminal-$slug"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$outBinary = Join-Path $outDir 'linkr.exe'
Copy-Item -LiteralPath $binary -Destination $outBinary -Force

$hash = (Get-FileHash -LiteralPath $outBinary -Algorithm SHA256).Hash.ToLowerInvariant()
Set-Content -LiteralPath (Join-Path $outDir 'SHA256SUMS') -Value "$hash  linkr.exe" -Encoding ascii

# Read the version back from the binary we are about to ship. `2>&1` keeps the
# probe quiet when the executable cannot run on this host (cross builds).
$version = ""
$firstLine = (& $outBinary --version 2>&1 | Select-Object -First 1)
if ($firstLine) {
    $fields = "$firstLine" -split '\s+'
    if ($fields.Count -gt 1) { $version = $fields[1] }
}

if ($Bundle) {
    $pythonExe = Find-Python
    if (-not $pythonExe) {
        throw "-Bundle needs Python 3; install it or pass -Python <interpreter>"
    }
    $bundleScript = Join-Path (Join-Path $repoDir 'tools') 'build_terminal_bundle.py'
    $bundleOutput = Join-Path (Join-Path $repoDir 'dist') 'linkr-bee-terminal.ps1'
    $bundleArgs = @('--exe', $outBinary, '--output', $bundleOutput)
    if ($version) { $bundleArgs += @('--version', $version) }
    Write-Host "==> $pythonExe build_terminal_bundle.py"
    & $pythonExe $bundleScript @bundleArgs
    if ($LASTEXITCODE -ne 0) { throw "bundle generation failed ($LASTEXITCODE)" }
    Write-Host "Bundle: $bundleOutput"
}

$label = if ($version) { "($version)" } else { "" }
Write-Host "Built: $outBinary $label"
