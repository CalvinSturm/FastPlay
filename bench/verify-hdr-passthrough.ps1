<#
.SYNOPSIS
    End-to-end verification of HDR PQ passthrough (HdrPqOutput) through the
    real player on an HDR-active display.

.DESCRIPTION
    Two stages:

    1. Runs bench/verify-tonemap.ps1 with a passthrough tolerance. On an
       HDR-active display its PQ/PQ-DIM/HLG clips route to HdrPqOutput: the
       shader writes PQ onto the 10-bit chain and the screenshot hook
       converts the R10G10B10A2 readback back to SDR through the audited
       CPU model — so the same reference model applies, composed, with only
       the 10-bit PQ round-trip quantization added (measured max delta 1;
       tolerance 3). Then asserts from the reported session log that the LAST case
       actually took path=HdrPqOutput and swapped the chain — without this
       oracle, a silently broken gate falling back to tone-map would still
       pass the pixel comparison.

    2. Resize-during-HDR-playback: plays a PQ clip, captures, resizes the
       window (production ResizeBuffers on the HDR chain + SetColorSpace1
       re-commit), captures again, and requires the seven bar centers to
       match across the resize (proportional sampling; bars are
       scale-invariant at their centers).

    On an SDR-only display stage 1's oracle fails by design — this bench is
    specifically the HDR-machine matrix entry; verify-tonemap alone covers
    SDR machines.

.EXAMPLE
    pwsh -File bench/verify-hdr-passthrough.ps1
#>
[CmdletBinding()]
param(
    [string]$Exe = "$PSScriptRoot\..\target\debug\fastplay.exe",
    [string]$Ffmpeg = "ffmpeg",
    [string]$WorkDir = "$env:TEMP\fastplay-passthrough-verify",
    [int]$Tolerance = 3,
    [int]$SettleSeconds = 3,
    # Max per-channel delta between pre- and post-resize captures.
    [int]$ResizeTolerance = 2
)

$ErrorActionPreference = "Stop"
$W = 1280; $H = 720
$WM_APP_SAVE_SCREENSHOT = 0x8001
$WM_CLOSE = 0x0010
# verify-tonemap launches the app, so it reports back which run's log to read.
# Resolving "the newest session-*.log" here instead would race any other
# FastPlay instance that exited while this script was running.
$runLogOut = Join-Path $WorkDir "last-run-log.txt"

if (-not (Test-Path $Exe)) { throw "fastplay binary not found: $Exe (build first)" }
if (-not (Get-Command $Ffmpeg -ErrorAction SilentlyContinue)) { throw "ffmpeg not found on PATH" }
New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null

# ---- Stage 1: composed-model pixel check + path oracle ----------------------
Write-Host "=== stage 1: verify-tonemap through the passthrough path ==="
& pwsh -File (Join-Path $PSScriptRoot "verify-tonemap.ps1") -Exe $Exe -Ffmpeg $Ffmpeg -Tolerance $Tolerance -RunLogOut $runLogOut
if ($LASTEXITCODE -ne 0) { throw "verify-tonemap failed under passthrough (exit $LASTEXITCODE)" }

# The reported log is the last case's run (HLG).
if (-not (Test-Path $runLogOut)) { throw "verify-tonemap did not report a run log at $runLogOut" }
$logPath = (Get-Content $runLogOut -Raw).Trim()
if (-not $logPath -or -not (Test-Path $logPath)) { throw "reported session log not found: $logPath" }
$log = Get-Content $logPath -Raw
if ($log -notmatch [regex]::Escape("path=HdrPqOutput")) {
    throw "ORACLE FAILED: last open did not select HdrPqOutput (is the Windows HDR toggle on?)"
}
if ($log -match [regex]::Escape("path=HdrToSdrToneMapRequired")) {
    throw "ORACLE FAILED: an open fell back to tone-map on the HDR display"
}
if ($log -notmatch [regex]::Escape("kind change Some(Sdr) -> Hdr10Pq")) {
    throw "ORACLE FAILED: the swapchain never swapped to the HDR10 kind"
}
Write-Host "  ORACLE OK: HdrPqOutput selected, chain swapped Sdr -> Hdr10Pq`n"

# ---- Stage 2: resize during HDR playback ------------------------------------
Write-Host "=== stage 2: resize during HDR playback ==="
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class FpPassWin {
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lp);
    public delegate bool EnumProc(IntPtr hWnd, IntPtr lp);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr hWnd);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern int GetWindowText(IntPtr hWnd, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hWnd, uint msg, IntPtr wp, IntPtr lp);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr hWnd, int x, int y, int w, int h, bool repaint);
}
"@

function Find-FastPlayWindow([uint32]$TargetPid) {
    $found = [IntPtr]::Zero
    $cb = [FpPassWin+EnumProc]{ param($h, $lp)
        if (-not [FpPassWin]::IsWindowVisible($h)) { return $true }
        $procId = 0
        [FpPassWin]::GetWindowThreadProcessId($h, [ref]$procId) | Out-Null
        if ($procId -ne $TargetPid) { return $true }
        $len = [FpPassWin]::GetWindowTextLength($h)
        if ($len -eq 0) { return $true }
        $sb = New-Object System.Text.StringBuilder ($len + 1)
        [FpPassWin]::GetWindowText($h, $sb, $sb.Capacity) | Out-Null
        if ($sb.ToString().EndsWith(" - FastPlay")) {
            (Get-Variable found -Scope 1).Value = $h
            return $false
        }
        return $true
    }
    [FpPassWin]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
    $found
}

$clip = Join-Path $WorkDir "bars-pq-resize.mp4"
if (-not (Test-Path $clip)) {
    & $Ffmpeg -y -hide_banner -loglevel error `
        -f lavfi -i "smptebars=duration=20:size=${W}x${H}:rate=30" `
        -vf "scale=out_color_matrix=bt2020:out_range=tv,format=yuv420p,setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=smpte2084:range=tv" `
        -c:v libx264 -preset veryfast $clip
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed generating $clip" }
}

$screenshotDir = Join-Path $env:USERPROFILE "Pictures\FastPlay"
$shots = @()

function Trigger-Screenshot([IntPtr]$Hwnd, [DateTime]$After) {
    [FpPassWin]::PostMessageW($Hwnd, $WM_APP_SAVE_SCREENSHOT, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
    foreach ($attempt in 1..40) {
        Start-Sleep -Milliseconds 250
        $shot = Get-ChildItem $screenshotDir -Filter "fastplay-screenshot-*.bmp" -ErrorAction SilentlyContinue |
            Where-Object { $_.LastWriteTime -gt $After } |
            Sort-Object LastWriteTime -Descending | Select-Object -First 1
        if ($shot) { return $shot.FullName }
    }
    throw "no screenshot appeared"
}

function Sample-Bars([string]$BmpPath) {
    $bmp = [System.Drawing.Bitmap]::FromFile($BmpPath)
    $samples = @()
    foreach ($i in 0..6) {
        $sx = [int]($bmp.Width * ($i + 0.5) / 7.0); $sy = [int]($bmp.Height * 0.30)
        $px = $bmp.GetPixel($sx, $sy)
        $samples += , @([int]$px.R, [int]$px.G, [int]$px.B)
    }
    $bmp.Dispose()
    return , $samples
}

$proc = Start-Process -FilePath $Exe -ArgumentList "`"$clip`"" -PassThru
$failed = $false
try {
    $hwnd = [IntPtr]::Zero
    foreach ($attempt in 1..30) {
        Start-Sleep -Milliseconds 400
        $hwnd = Find-FastPlayWindow $proc.Id
        if ($hwnd -ne [IntPtr]::Zero) { break }
    }
    if ($hwnd -eq [IntPtr]::Zero) { throw "FastPlay render window not found" }
    Start-Sleep -Seconds $SettleSeconds

    $t0 = Get-Date
    $before = Sample-Bars (Trigger-Screenshot $hwnd $t0)

    # Production resize on the live HDR chain (ResizeBuffers + color-space
    # re-commit), then capture again.
    [FpPassWin]::MoveWindow($hwnd, 60, 60, 900, 620, $true) | Out-Null
    Start-Sleep -Seconds 2
    $t1 = Get-Date
    $shot2 = Trigger-Screenshot $hwnd $t1
    $shots += $shot2
    $after = Sample-Bars $shot2

    $maxDelta = 0
    foreach ($i in 0..6) {
        foreach ($c in 0..2) {
            $d = [Math]::Abs($before[$i][$c] - $after[$i][$c])
            if ($d -gt $maxDelta) { $maxDelta = $d }
        }
    }
    if ($maxDelta -le $ResizeTolerance) {
        Write-Host "  PASS: bars identical across resize (max delta $maxDelta)"
    } else {
        Write-Host "  FAIL: bars diverged across resize (max delta $maxDelta > $ResizeTolerance)"
        $failed = $true
    }
    if ($proc.HasExited) {
        Write-Host "  FAIL: player exited during the resize sequence"
        $failed = $true
    }
}
finally {
    $hwndClose = Find-FastPlayWindow $proc.Id
    if ($hwndClose -ne [IntPtr]::Zero) {
        [FpPassWin]::PostMessageW($hwndClose, $WM_CLOSE, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
    }
    $proc.WaitForExit(8000) | Out-Null
    if (-not $proc.HasExited) { $proc.Kill() }
    Get-ChildItem $screenshotDir -Filter "fastplay-screenshot-*.bmp" -ErrorAction SilentlyContinue |
        Where-Object { $_.LastWriteTime -gt (Get-Date).AddMinutes(-5) } |
        Remove-Item -Force -ErrorAction SilentlyContinue
}

if ($failed) {
    Write-Host "`nRESULT: FAIL"
    exit 1
}
Write-Host "`nRESULT: PASS (passthrough pixel-exact end-to-end; HDR chain survives resize)"
exit 0
