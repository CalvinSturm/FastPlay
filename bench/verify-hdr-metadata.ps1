<#
.SYNOPSIS
    Verify HDR10 static metadata flows from real HEVC SEI to SetHDRMetaData.

.DESCRIPTION
    Encodes a 10-bit PQ clip whose mastering-display and content-light SEI
    carry the exact MSDN DXGI_HDR_METADATA_HDR10 worked-example values
    (DCI-P3 primaries, D65 white point, 1000-nit max / 0.001-nit min
    mastering luminance, MaxCLL 2000, MaxFALL 500), plays it through the
    real player on the HDR path, and asserts from session.log that the
    metadata was extracted from the first decoded frame, converted, and
    applied to the HDR swapchain with the expected values:

        [hdr_metadata] applied: maxMastering=1000 minMastering(0.0001nit)=10 maxCLL=2000 maxFALL=500

    Structural oracle only — static metadata does not change our rendered
    pixels (DWM/display behavior may differ); the pixel oracles live in
    verify-hdr-passthrough.ps1. Requires an HDR-active display (the
    metadata path only runs on HdrPqOutput) and libx265 in ffmpeg.

.EXAMPLE
    pwsh -File bench/verify-hdr-metadata.ps1
#>
[CmdletBinding()]
param(
    [string]$Exe = "$PSScriptRoot\..\target\debug\fastplay.exe",
    [string]$Ffmpeg = "ffmpeg",
    [string]$WorkDir = "$env:TEMP\fastplay-metadata-verify",
    [int]$SettleSeconds = 5
)

$ErrorActionPreference = "Stop"
$WM_CLOSE = 0x0010
$logPath = Join-Path $env:APPDATA "FastPlay\session.log"

if (-not (Test-Path $Exe)) { throw "fastplay binary not found: $Exe (build first)" }
if (-not (Get-Command $Ffmpeg -ErrorAction SilentlyContinue)) { throw "ffmpeg not found on PATH" }
New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null

$clip = Join-Path $WorkDir "bars-pq-mastering.mp4"
if (-not (Test-Path $clip)) {
    # x265 SEI units: chromaticities x50000, luminance in 0.0001 nits -
    # the raw ST 2086 wire format ffmpeg decodes back into rationals.
    & $Ffmpeg -y -hide_banner -loglevel error `
        -f lavfi -i "smptebars=duration=8:size=1280x720:rate=30" `
        -vf "scale=out_color_matrix=bt2020:out_range=tv,format=yuv420p10le,setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=smpte2084:range=tv" `
        -c:v libx265 -preset fast `
        -x265-params "master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,10):max-cll=2000,500:repeat-headers=1" `
        $clip
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg/libx265 failed generating $clip" }
}

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class FpMetaWin {
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lp);
    public delegate bool EnumProc(IntPtr hWnd, IntPtr lp);
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hWnd, uint msg, IntPtr wp, IntPtr lp);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
}
"@

$proc = Start-Process -FilePath $Exe -ArgumentList "`"$clip`"" -PassThru
try {
    Start-Sleep -Seconds $SettleSeconds
    if ($proc.HasExited) { throw "player exited early (code $($proc.ExitCode))" }
}
finally {
    $targetId = [uint32]$proc.Id
    $cb = [FpMetaWin+EnumProc]{ param($h, $lp)
        $procId = 0
        [FpMetaWin]::GetWindowThreadProcessId($h, [ref]$procId) | Out-Null
        if ($procId -eq $targetId -and [FpMetaWin]::IsWindowVisible($h)) {
            [FpMetaWin]::PostMessageW($h, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
        }
        return $true
    }
    [FpMetaWin]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
    $proc.WaitForExit(8000) | Out-Null
    if (-not $proc.HasExited) { $proc.Kill() }
}

if (-not (Test-Path $logPath)) { throw "session.log not found at $logPath" }
$log = Get-Content $logPath -Raw
if ($log -notmatch [regex]::Escape("path=HdrPqOutput")) {
    throw "FAIL: open did not select HdrPqOutput (is the Windows HDR toggle on?)"
}
$expected = "[hdr_metadata] applied: maxMastering=1000 minMastering(0.0001nit)=10 maxCLL=2000 maxFALL=500"
if ($log -notmatch [regex]::Escape($expected)) {
    $seen = ($log -split "`n" | Select-String "hdr_metadata") -join "; "
    throw "FAIL: expected '$expected' in session.log; saw: $seen"
}
Write-Host "PASS: mastering/content-light SEI extracted, converted, and applied ($expected)"
exit 0
