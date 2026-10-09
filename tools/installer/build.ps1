<#
.SYNOPSIS
    Builds the release binaries and the installer (installer\out\RtspCam-<version>-setup.exe).

.DESCRIPTION
    Needs Inno Setup 6 (`winget install JRSoftware.InnoSetup`). Signing is optional: pass
    -SignCommand with a signtool command line using $f for the file, for example
    'signtool sign /fd sha256 /tr http://timestamp.digicert.com /td sha256 /a $f'. It signs the
    exe and the DLL before packaging, and the installer and uninstaller afterwards.

.EXAMPLE
    ./tools/installer/build.ps1
    ./tools/installer/build.ps1 -SkipBuild
#>
[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [string]$SignCommand
)

$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..\..')
$bin = Join-Path $root 'target\release'

$version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $version) { throw 'could not read the version from Cargo.toml' }

$iscc = @(
    (Get-Command iscc -ErrorAction SilentlyContinue).Source,
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $iscc) { throw 'Inno Setup 6 not found. Install it with: winget install JRSoftware.InnoSetup' }

if (-not $SkipBuild) {
    Push-Location $root
    try {
        cargo build --release --locked -p rtspcam-app -p rtspcam-vcam
        if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
    }
    finally { Pop-Location }
}

function Invoke-Sign([string]$file) {
    if (-not $SignCommand) { return }
    Invoke-Expression ('& ' + $SignCommand.Replace('$f', '"' + $file + '"'))
    if ($LASTEXITCODE -ne 0) { throw "signing $file failed" }
}
Invoke-Sign (Join-Path $bin 'rtspcam.exe')
Invoke-Sign (Join-Path $bin 'rtspcam_vcam.dll')

$isccArgs = @("/DAppVersion=$version", "/DBinDir=$bin")
if ($SignCommand) { $isccArgs += "/Ssigntool=$SignCommand"; $isccArgs += '/DSignTool=signtool' }
& $iscc @isccArgs (Join-Path $root 'installer\rtspcam.iss')
if ($LASTEXITCODE -ne 0) { throw 'Inno Setup failed' }
Write-Host "Built $(Join-Path $root "installer\out\RtspCam-$version-setup.exe")"
