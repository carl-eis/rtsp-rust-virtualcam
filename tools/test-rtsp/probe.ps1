<#
.SYNOPSIS
    Checks that every test stream from start.ps1 is reachable, using ffprobe.

.PARAMETER Server
    host:port of the mediamtx RTSP server.

.PARAMETER Docker
    Run ffprobe inside the container started by `start.ps1 -Docker` (no local ffprobe needed).

.EXAMPLE
    ./probe.ps1
    ./probe.ps1 -Server 192.168.1.20:8554
    ./probe.ps1 -Docker
#>
[CmdletBinding()]
param(
    [string]$Server = '127.0.0.1:8554',
    [switch]$Docker
)

$ErrorActionPreference = 'Stop'
$bin = Join-Path $PSScriptRoot 'bin'
if (Test-Path $bin) { $env:PATH = "$bin;$env:PATH" }
if ($Docker) {
    # Inside the container mediamtx listens on its own loopback.
    $Server = '127.0.0.1:8554'
} elseif (-not (Get-Command ffprobe -ErrorAction SilentlyContinue)) {
    Write-Error 'ffprobe not found on PATH (it ships with ffmpeg). Use -Docker to run it in the test container.'
}

function Invoke-Probe([string]$Url) {
    $probeArgs = @('-v', 'error', '-rtsp_transport', 'tcp', '-timeout', '5000000',
        '-select_streams', 'v:0', '-show_entries', 'stream=codec_name,width,height,avg_frame_rate',
        '-of', 'csv=p=0', $Url)
    if ($Docker) {
        docker exec rtspcam-test-rtsp ffprobe @probeArgs 2>&1
    } else {
        & ffprobe @probeArgs 2>&1
    }
}

$streams = @(
    @{ Path = 'h264-720p';  Expect = 'h264'  },
    @{ Path = 'h264-1080p'; Expect = 'h264'  },
    @{ Path = 'h265-720p';  Expect = 'hevc'  },
    @{ Path = 'mjpeg-720p'; Expect = 'mjpeg' },
    @{ Path = 'secure';     Expect = 'h264'; Auth = 'rtspcam:rtspcam-test@' },
    # Must fail: proves the secure stream really requires credentials.
    @{ Path = 'secure';     Expect = $null; Label = 'secure (no credentials, should be refused)' }
)

$failed = 0
foreach ($s in $streams) {
    $url = "rtsp://$($s.Auth)$Server/$($s.Path)"
    $label = if ($s.Label) { $s.Label } else { $s.Path }
    $out = Invoke-Probe $url
    $ok = $LASTEXITCODE -eq 0
    if ($null -eq $s.Expect) {
        $pass = -not $ok
        $detail = if ($pass) { 'refused' } else { 'UNEXPECTEDLY READABLE' }
    } else {
        # ffprobe may print decoder warnings first; the CSV row is the last line.
        $row = "$($out | Select-Object -Last 1)".Trim()
        $pass = $ok -and ($row -like "$($s.Expect),*")
        $detail = $row
    }
    $status = if ($pass) { 'OK  ' } else { $failed++; 'FAIL' }
    Write-Host ("{0} {1,-45} {2}" -f $status, $label, $detail)
}

if ($failed) { Write-Host "$failed check(s) failed."; exit 1 }
Write-Host 'All test streams reachable.'
