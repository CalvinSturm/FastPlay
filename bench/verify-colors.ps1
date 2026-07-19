<#
.SYNOPSIS
    Pixel-verify FastPlay's video color pipeline via GPU backbuffer readback.

.DESCRIPTION
    Measures the render pipeline at the correct tap point: the backbuffer
    AFTER VideoProcessorBlt (and overlay draw) but BEFORE Present, using the
    app's own staging-texture readback (capture_bgra_texture, the Ctrl+S
    screenshot path). This excludes DWM/compositor processing entirely —
    unlike window-capture methods (PrintWindow, CopyFromScreen), which for an
    HDR swapchain would measure the compositor's 8-bit tone-mapped composite
    instead of our color-space plumbing.

    Flow: generate a tagged SMPTE-bars clip and an ffmpeg reference decode of
    frame 1, play the clip in FastPlay, trigger a screenshot through the
    WM_APP_SAVE_SCREENSHOT automation hook (WM_APP+1; PostMessage-able
    cross-process, unlike Ctrl+S which needs live modifier-key state), then
    compare the 7 top bars of the captured backbuffer against the reference
    in raw 8-bit code values.

    Comparison is deliberately dumb: raw per-channel code-value deltas with a
    stated tolerance, no linearization. Calibration: a healthy SDR pipeline
    measures max delta ~2; the pre-fix limited-range bug measured 10-15.

.EXAMPLE
    pwsh -File bench/verify-colors.ps1
    pwsh -File bench/verify-colors.ps1 -Exe target\release\fastplay.exe
#>
[CmdletBinding()]
param(
    [string]$Exe = "$PSScriptRoot\..\target\debug\fastplay.exe",
    [string]$Ffmpeg = "ffmpeg",
    [string]$WorkDir = "$env:TEMP\fastplay-color-verify",
    [int]$Tolerance = 4,
    # Seconds to let playback reach a steadily presented frame.
    [int]$SettleSeconds = 3
)

$ErrorActionPreference = "Stop"
$WM_APP_SAVE_SCREENSHOT = 0x8001  # WM_APP + 1, see window_proc in src/ffi/dxgi.rs
$WM_CLOSE = 0x0010

if (-not (Test-Path $Exe)) { throw "fastplay binary not found: $Exe (build first)" }
if (-not (Get-Command $Ffmpeg -ErrorAction SilentlyContinue)) {
    throw "ffmpeg not found on PATH (needed to generate the clip and reference)"
}
New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null

# --- Test material: BT.709 limited-range SMPTE bars + ffmpeg reference ------
$clip = Join-Path $WorkDir "bars709.mp4"
$ref = Join-Path $WorkDir "ref709.png"
if (-not (Test-Path $clip)) {
    & $Ffmpeg -y -hide_banner -loglevel error `
        -f lavfi -i "smptebars=duration=30:size=1280x720:rate=30" `
        -vf "scale=out_color_matrix=bt709:out_range=tv,format=yuv420p" `
        -c:v libx264 -preset veryfast `
        -colorspace bt709 -color_primaries bt709 -color_trc bt709 -color_range tv $clip
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed generating $clip" }
}
if (-not (Test-Path $ref)) {
    & $Ffmpeg -y -hide_banner -loglevel error -i $clip `
        -vf "scale=in_color_matrix=bt709:in_range=tv:out_range=full,format=rgb24" `
        -frames:v 1 $ref
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed generating $ref" }
}

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class FpWin {
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lp);
    public delegate bool EnumProc(IntPtr hWnd, IntPtr lp);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr hWnd);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern int GetWindowText(IntPtr hWnd, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hWnd, uint msg, IntPtr wp, IntPtr lp);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
}
"@

function Find-FastPlayWindow([uint32]$TargetPid) {
    # Match by PID (a dev machine routinely has other FastPlay windows open,
    # and the debug build's console window also carries a title) AND require
    # the "<file> - FastPlay" playing title: the bare idle "FastPlay" title
    # means the open has not completed yet, and capturing then reads the
    # background instead of the video (the cold-start flake this fixes).
    $found = [IntPtr]::Zero
    $cb = [FpWin+EnumProc]{ param($h, $lp)
        if (-not [FpWin]::IsWindowVisible($h)) { return $true }
        $procId = 0
        [FpWin]::GetWindowThreadProcessId($h, [ref]$procId) | Out-Null
        if ($procId -ne $TargetPid) { return $true }
        $len = [FpWin]::GetWindowTextLength($h)
        if ($len -eq 0) { return $true }
        $sb = New-Object System.Text.StringBuilder ($len + 1)
        [FpWin]::GetWindowText($h, $sb, $sb.Capacity) | Out-Null
        if ($sb.ToString().EndsWith(" - FastPlay")) {
            (Get-Variable found -Scope 1).Value = $h
            return $false
        }
        return $true
    }
    [FpWin]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
    $found
}

$screenshotDir = Join-Path $env:USERPROFILE "Pictures\FastPlay"
$launchTime = Get-Date

$proc = Start-Process -FilePath $Exe -ArgumentList "`"$clip`"" -PassThru
$hwnd = [IntPtr]::Zero
$bmpPath = $null
try {
    foreach ($attempt in 1..30) {
        Start-Sleep -Milliseconds 500
        $hwnd = Find-FastPlayWindow $proc.Id
        if ($hwnd -ne [IntPtr]::Zero) { break }
    }
    if ($hwnd -eq [IntPtr]::Zero) { throw "FastPlay render window not found" }
    Start-Sleep -Seconds $SettleSeconds

    # Trigger the backbuffer readback (post-Blt, pre-Present) and wait for
    # the BMP the app writes to the screenshot directory.
    [FpWin]::PostMessageW($hwnd, $WM_APP_SAVE_SCREENSHOT, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
    foreach ($attempt in 1..40) {
        Start-Sleep -Milliseconds 250
        $candidate = Get-ChildItem $screenshotDir -Filter "fastplay-screenshot-*.bmp" -ErrorAction SilentlyContinue |
            Where-Object { $_.LastWriteTime -gt $launchTime } |
            Sort-Object LastWriteTime -Descending | Select-Object -First 1
        if ($candidate) { $bmpPath = $candidate.FullName; break }
    }
    if (-not $bmpPath) { throw "no screenshot BMP appeared in $screenshotDir" }

    # --- Sample the 7 top bars in the captured backbuffer -------------------
    $bmp = New-Object System.Drawing.Bitmap $bmpPath
    $refImg = New-Object System.Drawing.Bitmap $ref
    try {
        # Aspect-fit rect of the 1280x720 video inside the backbuffer.
        $w = $bmp.Width; $h = $bmp.Height
        $scale = [Math]::Min($w / 1280.0, $h / 720.0)
        $vw = 1280.0 * $scale; $vh = 720.0 * $scale
        $ox = ($w - $vw) / 2.0; $oy = ($h - $vh) / 2.0

        $names = @("gray", "yellow", "cyan", "green", "magenta", "red", "blue")
        $maxDelta = 0
        "{0,-8} {1,-16} {2,-16} {3}" -f "bar", "backbuffer", "reference", "max|d|"
        foreach ($i in 0..6) {
            $rx = [int](1280.0 * ($i + 0.5) / 7.0); $ry = [int](720.0 * 0.30)
            $rp = $refImg.GetPixel($rx, $ry)
            $cx = [int]($ox + $vw * ($i + 0.5) / 7.0); $cy = [int]($oy + $vh * 0.30)
            $cp = $bmp.GetPixel($cx, $cy)
            $d = [Math]::Max([Math]::Max(
                    [Math]::Abs($cp.R - $rp.R),
                    [Math]::Abs($cp.G - $rp.G)),
                [Math]::Abs($cp.B - $rp.B))
            if ($d -gt $maxDelta) { $maxDelta = $d }
            "{0,-8} {1,-16} {2,-16} {3}" -f $names[$i],
                "($($cp.R),$($cp.G),$($cp.B))", "($($rp.R),$($rp.G),$($rp.B))", $d
        }
        ""
        if ($maxDelta -le $Tolerance) {
            "PASS: backbuffer readback within +/-$Tolerance of ffmpeg reference (max delta $maxDelta)"
            $script:exitCode = 0
        }
        else {
            "FAIL: max per-channel delta $maxDelta exceeds tolerance $Tolerance"
            $script:exitCode = 1
        }
    }
    finally {
        $bmp.Dispose(); $refImg.Dispose()
    }
}
finally {
    if ($hwnd -ne [IntPtr]::Zero) {
        [FpWin]::PostMessageW($hwnd, $WM_CLOSE, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
    }
    if ($proc -and -not $proc.WaitForExit(5000)) { $proc.Kill() }
    if ($bmpPath -and (Test-Path $bmpPath)) { Remove-Item $bmpPath -Force }
}
exit $script:exitCode
