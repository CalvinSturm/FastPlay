<#
.SYNOPSIS
    Verify that subtitle overlays composite correctly on top of HDR video.

.DESCRIPTION
    HDR frames do not reach the screen the way SDR frames do. SDR is blitted
    to the backbuffer by the D3D11 video processor; HDR is tone-mapped by our
    own pixel shader (HdrToneMapRenderer), which binds the render target and
    clears it itself before drawing its quad. Overlays are drawn afterwards,
    onto the same render target. That ordering is the thing this checks: a
    regression here would either lose the subtitles or clobber the picture
    under them, and neither shows up in any unit test.

    Oracle: play the same clip twice, once with a sidecar .srt and once
    without, and read the backbuffer back through the app's own screenshot
    path (WM_APP+1), which captures AFTER overlays are composited.

    Three assertions per clip:
      1. the video actually rendered (the frame is not blank), so a pass
         cannot be produced by a black screen;
      2. the subtitle band CHANGED when the sidecar was present, so the
         overlay really composited;
      3. the picture above the band is UNCHANGED, so drawing the overlay did
         not disturb the video beneath it.

    Runs against an HLG clip and, as a control, an SDR clip — the SDR path is
    known-good, so if both fail the harness is at fault, not the HDR path.

.EXAMPLE
    pwsh -File bench\verify-subtitles-hdr.ps1
#>
[CmdletBinding()]
param(
    [string]$Exe = "$PSScriptRoot\..\target\debug\fastplay.exe",
    [string]$Ffmpeg = "ffmpeg",
    [string]$WorkDir = "$env:TEMP\fastplay-subs-hdr-verify",
    # Fraction of the frame height, measured from the bottom, that the
    # subtitle is allowed to occupy. Keep in step with the margin in
    # subtitle_quad_vertices (viewport_height / 18, min 24px) plus text height.
    [double]$BandFraction = 0.30,
    # A pixel counts as changed if any channel moves by more than this. Small
    # enough to catch text, large enough to ignore decode nondeterminism.
    [int]$ChangeThreshold = 24,
    [int]$SettleSeconds = 3
)

$ErrorActionPreference = "Stop"
$WM_APP_SAVE_SCREENSHOT = 0x8001  # WM_APP + 1, see window_proc in src/ffi/dxgi.rs
$WM_CLOSE = 0x0010
$script:exitCode = 0

if (-not (Test-Path $Exe)) { throw "fastplay binary not found: $Exe (build first)" }
if (-not (Get-Command $Ffmpeg -ErrorAction SilentlyContinue)) {
    throw "ffmpeg not found on PATH (needed to generate the clips)"
}
New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class FpSubWin {
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

# Match the render window by PID: a dev machine routinely has other FastPlay
# windows open, and the debug build's console window also carries a title.
function Find-FastPlayWindow([uint32]$TargetPid) {
    $found = [IntPtr]::Zero
    $cb = [FpSubWin+EnumProc]{ param($h, $lp)
        if (-not [FpSubWin]::IsWindowVisible($h)) { return $true }
        $procId = 0
        [FpSubWin]::GetWindowThreadProcessId($h, [ref]$procId) | Out-Null
        if ($procId -ne $TargetPid) { return $true }
        $len = [FpSubWin]::GetWindowTextLength($h)
        if ($len -eq 0) { return $true }
        $sb = New-Object System.Text.StringBuilder ($len + 1)
        [FpSubWin]::GetWindowText($h, $sb, $sb.Capacity) | Out-Null
        if ($sb.ToString().EndsWith(" - FastPlay")) {
            (Get-Variable found -Scope 1).Value = $h
            return $false
        }
        return $true
    }
    [FpSubWin]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
    $found
}

$screenshotDir = Join-Path $env:USERPROFILE "Pictures\FastPlay"

# Play $clip, trigger the backbuffer screenshot, return the captured bitmap path.
function Capture-Backbuffer([string]$Clip) {
    $launchTime = Get-Date
    $proc = Start-Process -FilePath $Exe -ArgumentList "`"$Clip`"" -PassThru
    $hwnd = [IntPtr]::Zero
    try {
        foreach ($attempt in 1..30) {
            Start-Sleep -Milliseconds 400
            $hwnd = Find-FastPlayWindow $proc.Id
            if ($hwnd -ne [IntPtr]::Zero) { break }
        }
        if ($hwnd -eq [IntPtr]::Zero) { throw "FastPlay render window not found for $Clip" }
        Start-Sleep -Seconds $SettleSeconds

        [FpSubWin]::PostMessageW($hwnd, $WM_APP_SAVE_SCREENSHOT, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
        foreach ($attempt in 1..40) {
            Start-Sleep -Milliseconds 250
            $shot = Get-ChildItem $screenshotDir -Filter "fastplay-screenshot-*.bmp" -ErrorAction SilentlyContinue |
                Where-Object { $_.LastWriteTime -gt $launchTime } |
                Sort-Object LastWriteTime -Descending | Select-Object -First 1
            if ($shot) { return $shot.FullName }
        }
        throw "no screenshot appeared in $screenshotDir for $Clip"
    }
    finally {
        if ($hwnd -ne [IntPtr]::Zero) {
            [FpSubWin]::PostMessageW($hwnd, $WM_CLOSE, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
        }
        if ($proc -and -not $proc.WaitForExit(5000)) { $proc.Kill() }
    }
}

# Count pixels differing by more than the threshold, split into the subtitle
# band (bottom) and the picture above it. Sampled on a grid: we need counts,
# not a per-pixel diff.
function Compare-Regions($PathA, $PathB, [double]$BandFraction) {
    $a = New-Object System.Drawing.Bitmap $PathA
    $b = New-Object System.Drawing.Bitmap $PathB
    try {
        if ($a.Width -ne $b.Width -or $a.Height -ne $b.Height) {
            throw "captures differ in size ($($a.Width)x$($a.Height) vs $($b.Width)x$($b.Height))"
        }
        $bandTop = [int]($a.Height * (1.0 - $BandFraction))
        $bandChanged = 0; $aboveChanged = 0; $nonBlack = 0
        for ($y = 0; $y -lt $a.Height; $y += 2) {
            for ($x = 0; $x -lt $a.Width; $x += 2) {
                $pa = $a.GetPixel($x, $y); $pb = $b.GetPixel($x, $y)
                if ($pa.R -gt 16 -or $pa.G -gt 16 -or $pa.B -gt 16) { $nonBlack++ }
                $d = [Math]::Max([Math]::Max(
                        [Math]::Abs($pa.R - $pb.R),
                        [Math]::Abs($pa.G - $pb.G)),
                    [Math]::Abs($pa.B - $pb.B))
                if ($d -gt $ChangeThreshold) {
                    if ($y -ge $bandTop) { $bandChanged++ } else { $aboveChanged++ }
                }
            }
        }
        [pscustomobject]@{
            BandChanged  = $bandChanged
            AboveChanged = $aboveChanged
            NonBlack     = $nonBlack
            Width        = $a.Width
            Height       = $a.Height
        }
    }
    finally { $a.Dispose(); $b.Dispose() }
}

$srt = @"
1
00:00:00,200 --> 00:00:30,000
HDR SUBTITLE OVERLAY TEST
second line renders too
"@

# --- Test material ----------------------------------------------------------
# Each case is generated twice under different stems, so one has a sidecar .srt
# beside it and the other does not (the loader keys off the media path).
$cases = @(
    @{
        Name   = "HLG (HDR)"
        Filter = "setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=arib-std-b67:range=tv"
        Codec  = @("-c:v", "libx265", "-profile:v", "main10", "-pix_fmt", "yuv420p10le",
                   "-x265-params", "colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc:range=limited",
                   "-tag:v", "hvc1")
        Ext    = "mov"
    },
    @{
        Name   = "BT.709 (SDR control)"
        Filter = "setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv"
        Codec  = @("-c:v", "libx264", "-preset", "veryfast", "-pix_fmt", "yuv420p")
        Ext    = "mp4"
    }
)

$failed = $false
foreach ($case in $cases) {
    $stem = ($case.Name -replace '[^A-Za-z0-9]', '')
    $withSubs = Join-Path $WorkDir "$stem-subs.$($case.Ext)"
    $noSubs = Join-Path $WorkDir "$stem-nosubs.$($case.Ext)"
    $matrix = if ($case.Ext -eq "mov") { "bt2020nc" } else { "bt709" }
    $pix = if ($case.Ext -eq "mov") { "yuv420p10le" } else { "yuv420p" }

    if (-not (Test-Path $withSubs)) {
        & $Ffmpeg -y -hide_banner -loglevel error `
            -f lavfi -i "smptebars=duration=30:size=1280x720:rate=30" `
            -vf "scale=out_color_matrix=${matrix}:out_range=tv,format=${pix},$($case.Filter)" `
            @($case.Codec) $withSubs
        if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed generating $withSubs" }
    }
    Copy-Item $withSubs $noSubs -Force
    # Sidecar for one stem only; the other must have none.
    Set-Content -Path ([IO.Path]::ChangeExtension($withSubs, "srt")) -Value $srt -Encoding UTF8
    Remove-Item ([IO.Path]::ChangeExtension($noSubs, "srt")) -Force -ErrorAction SilentlyContinue

    Write-Output ""
    Write-Output "=== $($case.Name) ==="
    $shotWith = Capture-Backbuffer $withSubs
    $shotWithout = Capture-Backbuffer $noSubs
    try {
        $r = Compare-Regions $shotWith $shotWithout $BandFraction
        $sampled = [int]($r.Width / 2) * [int]($r.Height / 2)
        Write-Output ("backbuffer {0}x{1}; sampled {2} px" -f $r.Width, $r.Height, $sampled)
        Write-Output ("  picture rendered (non-black px) : {0}" -f $r.NonBlack)
        Write-Output ("  subtitle band changed px        : {0}" -f $r.BandChanged)
        Write-Output ("  picture above band changed px   : {0}" -f $r.AboveChanged)

        # 1. Something was actually on screen — a black frame must not pass.
        if ($r.NonBlack -lt ($sampled / 10)) {
            Write-Output "  FAIL: frame is essentially blank; the video did not render"
            $failed = $true
        }
        # 2. The subtitle composited over the video.
        elseif ($r.BandChanged -lt 200) {
            Write-Output "  FAIL: subtitle band did not change; the overlay never composited"
            $failed = $true
        }
        # 3. Drawing the overlay left the picture above it alone.
        elseif ($r.AboveChanged -gt ($sampled / 100)) {
            Write-Output "  FAIL: picture above the subtitle changed; the overlay disturbed the video"
            $failed = $true
        }
        else {
            Write-Output "  PASS: subtitle composited over the picture, video intact beneath it"
        }
    }
    finally {
        foreach ($p in @($shotWith, $shotWithout)) {
            if ($p -and (Test-Path $p)) { Remove-Item $p -Force }
        }
    }
}

Write-Output ""
if ($failed) {
    Write-Output "RESULT: FAIL"
    $script:exitCode = 1
}
else {
    Write-Output "RESULT: PASS (subtitles composite correctly on both the HDR shader path and the SDR path)"
}
exit $script:exitCode
