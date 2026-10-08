<#
.SYNOPSIS
    Starts the local RTSP test server (mediamtx) with ffmpeg test-pattern streams.

.DESCRIPTION
    Uses mediamtx.yml in this folder. mediamtx launches one ffmpeg publisher per stream and
    restarts it if it exits. Stop with Ctrl+C.

    Native mode needs mediamtx.exe and ffmpeg.exe on PATH (or in tools/test-rtsp/bin/).
    Docker mode needs only Docker Desktop and uses the bluenviron/mediamtx image with ffmpeg.

.PARAMETER Docker
    Run mediamtx and ffmpeg in a container instead of natively.

.EXAMPLE
    ./start.ps1
    ./start.ps1 -Docker
#>
[CmdletBinding()]
param(
    [switch]$Docker
)

$ErrorActionPreference = 'Stop'
$config = Join-Path $PSScriptRoot 'mediamtx.yml'

if ($Docker) {
    docker run --rm -it --name rtspcam-test-rtsp -p 8554:8554 `
        -v "${config}:/mediamtx.yml:ro" bluenviron/mediamtx:latest-ffmpeg
    exit $LASTEXITCODE
}

# Prefer binaries dropped into ./bin (git-ignored).
$bin = Join-Path $PSScriptRoot 'bin'
if (Test-Path $bin) { $env:PATH = "$bin;$env:PATH" }

$missing = @('mediamtx', 'ffmpeg') | Where-Object { -not (Get-Command $_ -ErrorAction SilentlyContinue) }
if ($missing) {
    Write-Error (@(
        "Not found on PATH: $($missing -join ', ')."
        'Install them, for example:'
        '  scoop install ffmpeg mediamtx'
        '  winget install Gyan.FFmpeg   (then download mediamtx from https://github.com/bluenviron/mediamtx/releases)'
        "or put mediamtx.exe / ffmpeg.exe in $bin, or run: ./start.ps1 -Docker"
    ) -join [Environment]::NewLine)
}

Write-Host 'Test streams (rtsp://127.0.0.1:8554/...): h264-720p, h264-1080p, h265-720p, mjpeg-720p, secure (rtspcam / rtspcam-test)'
Write-Host 'Press Ctrl+C to stop.'
& mediamtx $config
exit $LASTEXITCODE
