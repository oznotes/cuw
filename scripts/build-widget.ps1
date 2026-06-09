# Builds the gpui widget (release) in a correctly-initialized VS dev shell.
#
# The widget links a CMake/Ninja-built C++ dep (gpui), so it needs the MSVC
# toolchain env AND the CMake/Ninja that ship *bundled* inside VS2022 (not on
# PATH by default). Bare `cargo build` from a non-interactive shell fails —
# either `vswhere`/`link.exe` aren't found, or CMake is missing. This script
# resolves all of that from vswhere, so the build is reproducible from any shell.
#
# Usage:  pwsh -File scripts/build-widget.ps1            # build only
#         pwsh -File scripts/build-widget.ps1 -Deploy    # build + copy to Desktop + relaunch
[CmdletBinding()]
param([switch]$Deploy)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Test-Path $vswhere)) { throw "vswhere.exe not found at $vswhere" }

$vs = & $vswhere -latest -products * `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -property installationPath
if (-not $vs) { throw "No VS install with the VC tools component found." }
Write-Host "VS:    $vs"

# MSVC env (link.exe, headers, libs) for x64.
Import-Module "$vs\Common7\Tools\Microsoft.VisualStudio.DevShell.dll"
Enter-VsDevShell -VsInstallPath $vs -SkipAutomaticLocation -DevCmdArguments '-arch=x64 -host_arch=x64' | Out-Null

# Bundled CMake + Ninja (not on PATH otherwise).
$cmakeBin = "$vs\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin"
$ninjaBin = "$vs\Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja"
$env:PATH = "$cmakeBin;$ninjaBin;$env:PATH"
Write-Host "cmake: $((Get-Command cmake).Source)"
Write-Host "ninja: $((Get-Command ninja).Source)"
Write-Host "rustc: $(rustc --version)"

Set-Location $repo
Write-Host "`n=== cargo build -p claude-usage-widget --release --locked ===`n"
cargo build -p claude-usage-widget --release --locked
if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }

$exe = Join-Path $repo 'target\release\claude-usage-widget.exe'
$size = [math]::Round((Get-Item $exe).Length / 1MB, 1)
Write-Host "`nBUILD OK -> $exe  (${size} MB)"

if ($Deploy) {
    Get-Process claude-usage-widget -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Seconds 1
    $dest = "$env:USERPROFILE\Desktop\claude-usage-widget.exe"
    Copy-Item $exe $dest -Force
    Start-Process $dest
    Write-Host "DEPLOYED -> $dest  (relaunched)"
}
