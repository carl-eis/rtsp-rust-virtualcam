<#
.SYNOPSIS
    Checks that every test stream from start.ps1 is reachable, using ffprobe.

.PARAMETER Server
    host:port of the mediamtx RTSP server.

.EXAMPLE
    ./probe.ps1
    ./probe.ps1 -Server 192.168.1.20:8554
#>
[CmdletBinding()]
param(
    [string]$Server = '127.0.0.1:8554'
)

$ErrorActionPreference = 'Stop'
$bin = Join-Path $PSScriptRoot 'bin'
if (Test-Path $bin) { $env:PATH = "$bin;$env:PATH" }
if (-not (Get-Command ffprobe -ErrorAction SilentlyContinue)) {
    Write-Error 'ffprobe not found on PATH (it ships with ffmpeg).'
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
    $out = & ffprobe -v error -rtsp_transport tcp -timeout 5000000 `
        -select_streams v:0 -show_entries stream=codec_name,width,height,avg_frame_rate `
        -of csv=p=0 $url 2>&1
    $ok = $LASTEXITCODE -eq 0
    if ($null -eq $s.Expect) {
        $pass = -not $ok
        $detail = if ($pass) { 'refused' } else { 'UNEXPECTEDLY READABLE' }
    } else {
        $pass = $ok -and ("$out" -like "$($s.Expect),*")
        $detail = "$out".Trim()
    }
    $status = if ($pass) { 'OK  ' } else { $failed++; 'FAIL' }
    Write-Host ("{0} {1,-45} {2}" -f $status, $label, $detail)
}

if ($failed) { Write-Host "$failed check(s) failed."; exit 1 }
Write-Host 'All test streams reachable.'
