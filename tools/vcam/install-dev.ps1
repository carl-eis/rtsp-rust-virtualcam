<#
.SYNOPSIS
    Installs the development build of the virtual camera media source for local testing.

.DESCRIPTION
    Copies rtspcam_vcam.dll (and its .pdb) from target\<profile> to C:\Program Files\RtspCam\
    and registers its CLSID in HKLM. The Frame Server service can only load the DLL from a
    folder it can read, which is why it isn't registered from target\.

    Needs administrator rights; the script asks for elevation (UAC) itself. The Frame Server
    services are stopped first so a loaded copy of the DLL can be replaced; Windows starts them
    again when a camera is next used.

    This is a developer convenience until the installer (Phase 7) exists.

.PARAMETER Configuration
    Cargo profile to install from: debug (default) or release.

.PARAMETER Uninstall
    Unregister the CLSID and delete the installed files.

.EXAMPLE
    ./tools/vcam/install-dev.ps1
    ./tools/vcam/install-dev.ps1 -Configuration release
    ./tools/vcam/install-dev.ps1 -Uninstall
#>
[CmdletBinding()]
param(
    [ValidateSet('debug', 'release')]
    [string]$Configuration = 'debug',
    [switch]$Uninstall
)

$ErrorActionPreference = 'Stop'

$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    $argList = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $PSCommandPath, '-Configuration', $Configuration)
    if ($Uninstall) { $argList += '-Uninstall' }
    $p = Start-Process pwsh -Verb RunAs -ArgumentList $argList -Wait -PassThru
    exit $p.ExitCode
}

$root = Resolve-Path (Join-Path $PSScriptRoot '..\..')
$build = Join-Path $root "target\$Configuration"
$cli = Join-Path $build 'rtspcam-cli.exe'
$dest = Join-Path $env:ProgramFiles 'RtspCam'
$dll = Join-Path $dest 'rtspcam_vcam.dll'

function Stop-FrameServer {
    foreach ($svc in 'FrameServerMonitor', 'FrameServer') {
        Stop-Service $svc -Force -ErrorAction SilentlyContinue
    }
}

try {
    if (-not (Test-Path $cli)) { throw "$cli not found. Run: cargo build -p rtspcam-cli -p rtspcam-vcam$(if ($Configuration -eq 'release') { ' --release' })" }

    if ($Uninstall) {
        if (Test-Path $dll) { & $cli vcam unregister --dll $dll }
        Stop-FrameServer
        Remove-Item $dest -Recurse -Force -ErrorAction SilentlyContinue
        Write-Host "Uninstalled from $dest"
        exit 0
    }

    $src = Join-Path $build 'rtspcam_vcam.dll'
    if (-not (Test-Path $src)) { throw "$src not found. Run: cargo build -p rtspcam-vcam$(if ($Configuration -eq 'release') { ' --release' })" }

    Stop-FrameServer
    New-Item -ItemType Directory -Force $dest | Out-Null
    Copy-Item $src $dest -Force
    $pdb = Join-Path $build 'rtspcam_vcam.pdb'
    if (Test-Path $pdb) { Copy-Item $pdb $dest -Force }
    # Files copied from a download location could carry Mark of the Web, which makes Frame Server refuse them.
    Get-ChildItem $dest | Unblock-File

    & $cli vcam register --dll $dll
    if ($LASTEXITCODE -ne 0) { throw 'registration failed' }
    Write-Host "Installed $dll"
    Write-Host "Logs: $env:ProgramData\RtspCam\logs\vcam.log"
}
catch {
    Write-Host "error: $_" -ForegroundColor Red
    if (-not $env:RTSPCAM_NO_PAUSE) { Read-Host 'Press Enter to close' }
    exit 1
}
