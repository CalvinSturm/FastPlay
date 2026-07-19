<#
.SYNOPSIS
    Golden-pixel verification of FastPlay's HDR->SDR tone-map shader
    (HdrToneMapRenderer, src/ffi/d3d11.rs).

.DESCRIPTION
    Generates synthetic PQ and HLG BT.2020 clips, plays each through the real
    FastPlay tone-map path, captures the actual SDR backbuffer via the app's
    own screenshot hook (WM_APP+1, which reads back after the draw), and
    compares the 7 SMPTE bar centers against an independently expressed
    double-precision CPU model of the shader: BT.2020 NCL YCbCr -> R'G'B' ->
    PQ EOTF or HLG inverse-OETF + OOTF -> diffuse-white normalization
    (203 cd/m2) -> knee/shoulder tone curve -> BT.2020 -> BT.709 -> sRGB.
    The model consumes the exact NV12 bytes ffmpeg decodes from each clip, so
    the decoder cannot disagree with itself.

    Three cases:
      PQ      - full-brightness bars; every colored bar lands beyond the tone
                curve's compression and must saturate to exact 0/255 channels
                (the script asserts the model itself predicts saturation, so
                this case cannot silently stop covering the clipped region).
      PQ-DIM  - the same bars darkened into the unclipped midtone range,
                exercising the PQ EOTF's linear region and the knee.
      HLG     - full-brightness bars through the HLG inverse OETF + OOTF.

    Audited baseline (2026-07-14, RTX 3080 Ti; see
    docs/audits/2026-07-14-hdr-final-audit.md): PQ max delta 0, PQ-DIM max
    delta 1, HLG max delta 1 -- default tolerance is 1/255.

    Requirements: a debug build (target/debug/fastplay.exe -- the release
    build is windows-subsystem and behaves differently under scripting),
    ffmpeg on PATH, and a desktop session. Matches the conventions of the
    other bench/verify-*.ps1 scripts: drives the real player, asserts on
    pixels, exits non-zero on failure, not part of cargo build.

.EXAMPLE
    pwsh -File bench\verify-tonemap.ps1
    pwsh -File bench\verify-tonemap.ps1 -KeepArtifacts   # retain clips/shots
#>
[CmdletBinding()]
param(
    [string]$Exe = "$PSScriptRoot\..\target\debug\fastplay.exe",
    [string]$Ffmpeg = "ffmpeg",
    [string]$WorkDir = "$env:TEMP\fastplay-tonemap-verify",
    # Max per-channel delta (of 255) between backbuffer and CPU model. The
    # audited baseline measured 0 (PQ), 1 (PQ-DIM), 1 (HLG).
    [int]$Tolerance = 1,
    [int]$SettleSeconds = 3,
    # Keep generated clips, decoded NV12 dumps, and captured screenshots for
    # diagnosis instead of deleting them at the end.
    [switch]$KeepArtifacts
)
$ErrorActionPreference = "Stop"
$W = 1280; $H = 720
$WM_APP_SAVE_SCREENSHOT = 0x8001  # WM_APP + 1, see window_proc in src/ffi/dxgi.rs
$WM_CLOSE = 0x0010

if (-not (Test-Path $Exe)) { throw "fastplay binary not found: $Exe (build first)" }
if (-not (Get-Command $Ffmpeg -ErrorAction SilentlyContinue)) { throw "ffmpeg not found on PATH" }
New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class FpToneMapWin {
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
    $cb = [FpToneMapWin+EnumProc]{ param($h, $lp)
        if (-not [FpToneMapWin]::IsWindowVisible($h)) { return $true }
        $procId = 0
        [FpToneMapWin]::GetWindowThreadProcessId($h, [ref]$procId) | Out-Null
        if ($procId -ne $TargetPid) { return $true }
        $len = [FpToneMapWin]::GetWindowTextLength($h)
        if ($len -eq 0) { return $true }
        $sb = New-Object System.Text.StringBuilder ($len + 1)
        [FpToneMapWin]::GetWindowText($h, $sb, $sb.Capacity) | Out-Null
        if ($sb.ToString().EndsWith(" - FastPlay")) {
            (Get-Variable found -Scope 1).Value = $h
            return $false
        }
        return $true
    }
    [FpToneMapWin]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
    $found
}

$screenshotDir = Join-Path $env:USERPROFILE "Pictures\FastPlay"
$script:capturedShots = @()

# Play $Clip, trigger the backbuffer screenshot, return the captured bitmap path.
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
        [FpToneMapWin]::PostMessageW($hwnd, $WM_APP_SAVE_SCREENSHOT, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
        foreach ($attempt in 1..40) {
            Start-Sleep -Milliseconds 250
            $shot = Get-ChildItem $screenshotDir -Filter "fastplay-screenshot-*.bmp" -ErrorAction SilentlyContinue |
                Where-Object { $_.LastWriteTime -gt $launchTime } |
                Sort-Object LastWriteTime -Descending | Select-Object -First 1
            if ($shot) {
                $script:capturedShots += $shot.FullName
                return $shot.FullName
            }
        }
        throw "no screenshot appeared for $Clip"
    }
    finally {
        if ($hwnd -ne [IntPtr]::Zero) {
            [FpToneMapWin]::PostMessageW($hwnd, $WM_CLOSE, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
        }
        $proc.WaitForExit(8000) | Out-Null
        if (-not $proc.HasExited) { $proc.Kill() }
    }
}

# ---- CPU model of HDR_TONE_MAP_PIXEL_SHADER (double precision) --------------
function Saturate([double]$v) { [Math]::Max(0.0, [Math]::Min(1.0, $v)) }

function Pq-Eotf([double]$s) {
    $m1 = 0.1593017578125; $m2 = 78.84375
    $c1 = 0.8359375; $c2 = 18.8515625; $c3 = 18.6875
    $enc = [Math]::Pow([Math]::Max($s, 0.0), 1.0 / $m2)
    $num = [Math]::Max($enc - $c1, 0.0)
    $den = [Math]::Max($c2 - $c3 * $enc, 1e-6)
    [Math]::Pow($num / $den, 1.0 / $m1)
}

function Hlg-InverseOetf([double]$s) {
    $a = 0.17883277; $b = 0.28466892; $c = 0.55991073
    if ($s -lt 0.5) { return ($s * $s) / 3.0 }
    return ([Math]::Exp(($s - $c) / $a) + $b) / 12.0
}

function Tone-Curve([double]$v, [double]$knee) {
    if ($v -le $knee) { return $v }
    $headroom = 1.0 - $knee
    return $knee + $headroom * (1.0 - [Math]::Exp(-($v - $knee) / $headroom))
}

function Srgb-Encode([double]$v) {
    if ($v -le 0.0031308) { return $v * 12.92 }
    return 1.055 * [Math]::Pow($v, 1.0 / 2.4) - 0.055
}

# $transfer: "pq" or "hlg". Input: 8-bit studio-range NV12 code values.
function Model-Pixel([double]$Y, [double]$Cb, [double]$Cr, [string]$transfer) {
    $luma = ($Y / 255.0 - 16.0 / 255.0) * (255.0 / 219.0)
    $cb = ($Cb / 255.0 - 128.0 / 255.0) * (255.0 / 224.0)
    $cr = ($Cr / 255.0 - 128.0 / 255.0) * (255.0 / 224.0)

    $r = Saturate ($luma + 1.47460 * $cr)
    $g = Saturate ($luma - 0.16455 * $cb - 0.57135 * $cr)
    $b = Saturate ($luma + 1.88140 * $cb)

    $lin = @(0.0, 0.0, 0.0)
    if ($transfer -eq "pq") {
        $scale = 10000.0 / 203.0
        $lin[0] = (Pq-Eotf $r) * $scale
        $lin[1] = (Pq-Eotf $g) * $scale
        $lin[2] = (Pq-Eotf $b) * $scale
    } else {
        $sr = Hlg-InverseOetf $r
        $sg = Hlg-InverseOetf $g
        $sb = Hlg-InverseOetf $b
        $sceneLuma = [Math]::Max(0.2627 * $sr + 0.6780 * $sg + 0.0593 * $sb, 1e-6)
        $boost = [Math]::Pow($sceneLuma, 0.2) * (1000.0 / 203.0)
        $lin[0] = $sr * $boost; $lin[1] = $sg * $boost; $lin[2] = $sb * $boost
    }

    $knee = 0.75
    $m = @((Tone-Curve $lin[0] $knee), (Tone-Curve $lin[1] $knee), (Tone-Curve $lin[2] $knee))

    $r709 = Saturate ( 1.66049 * $m[0] - 0.58764 * $m[1] - 0.07285 * $m[2])
    $g709 = Saturate (-0.12455 * $m[0] + 1.13290 * $m[1] - 0.00835 * $m[2])
    $b709 = Saturate (-0.01824 * $m[0] - 0.10057 * $m[1] + 1.11881 * $m[2])

    @(
        [int][Math]::Round(255.0 * (Srgb-Encode $r709)),
        [int][Math]::Round(255.0 * (Srgb-Encode $g709)),
        [int][Math]::Round(255.0 * (Srgb-Encode $b709))
    )
}

# ---- Cases -------------------------------------------------------------------
# lutyuv darkens PQ bars into the unclipped midtone range; the model consumes
# whatever code values result, so the exact factor only needs to keep every
# channel below the tone curve's saturation.
$dimLut = "lutyuv=y='(val-16)*0.45+16':u='(val-128)*0.45+128':v='(val-128)*0.45+128'"
$cases = @(
    @{ Name = "PQ";     Trc = "smpte2084";    Transfer = "pq";  Dim = $false; ExpectSaturated = $true  },
    @{ Name = "PQ-DIM"; Trc = "smpte2084";    Transfer = "pq";  Dim = $true;  ExpectSaturated = $false },
    @{ Name = "HLG";    Trc = "arib-std-b67"; Transfer = "hlg"; Dim = $false; ExpectSaturated = $false }
)
$names = @("gray", "yellow", "cyan", "green", "magenta", "red", "blue")
$overallFail = $false

foreach ($case in $cases) {
    $slug = $case.Name.ToLower()
    $clip = Join-Path $WorkDir "bars-$slug.mp4"
    $nv12 = Join-Path $WorkDir "bars-$slug.nv12"
    $dim = if ($case.Dim) { "$($dimLut)," } else { "" }
    if (-not (Test-Path $clip)) {
        & $Ffmpeg -y -hide_banner -loglevel error `
            -f lavfi -i "smptebars=duration=6:size=${W}x${H}:rate=30" `
            -vf "scale=out_color_matrix=bt2020:out_range=tv,format=yuv420p,$($dim)setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=$($case.Trc):range=tv" `
            -c:v libx264 -preset veryfast $clip
        if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed generating $clip" }
    }
    & $Ffmpeg -y -hide_banner -loglevel error -i $clip -frames:v 1 -f rawvideo -pix_fmt nv12 $nv12
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed extracting NV12 for $clip" }
    $src = [System.IO.File]::ReadAllBytes($nv12)
    if ($src.Length -ne $W * $H * 3 / 2) { throw "NV12 for $slug is $($src.Length) bytes, expected $($W*$H*3/2)" }

    Write-Host "=== $($case.Name) tone-map vs CPU model ==="
    $shotPath = Capture-Backbuffer $clip
    $bmp = [System.Drawing.Bitmap]::FromFile($shotPath)
    $maxDelta = 0
    $sawUnsaturated = $false
    "{0,-8} {1,-16} {2,-16} {3}" -f "bar", "backbuffer", "model", "max|d|"
    foreach ($i in 0..6) {
        $x = [int]($W * ($i + 0.5) / 7.0); $y = [int]($H * 0.30)
        $sx = [int]($bmp.Width * ($i + 0.5) / 7.0); $sy = [int]($bmp.Height * 0.30)
        $px = $bmp.GetPixel($sx, $sy)
        $Y = [double]$src[$y * $W + $x]
        $uvOff = $W * $H + [int][Math]::Floor($y / 2) * $W + 2 * [int][Math]::Floor($x / 2)
        $Cb = [double]$src[$uvOff]; $Cr = [double]$src[$uvOff + 1]
        $m = Model-Pixel $Y $Cb $Cr $case.Transfer
        foreach ($ch in $m) { if ($ch -ne 0 -and $ch -ne 255) { $sawUnsaturated = $true } }
        $d = [Math]::Max([Math]::Max(
                [Math]::Abs($px.R - $m[0]),
                [Math]::Abs($px.G - $m[1])),
            [Math]::Abs($px.B - $m[2]))
        if ($d -gt $maxDelta) { $maxDelta = $d }
        "{0,-8} {1,-16} {2,-16} {3}" -f $names[$i],
            "($($px.R),$($px.G),$($px.B))", "($($m[0]),$($m[1]),$($m[2]))", $d
    }
    $bmp.Dispose()
    if ($case.ExpectSaturated -and $sawUnsaturated) {
        Write-Host "  FAIL: this case must exercise the clipped region (model predicted a non-0/255 channel)`n"
        $overallFail = $true
    } elseif (-not $case.ExpectSaturated -and -not $sawUnsaturated) {
        Write-Host "  FAIL: this case must exercise unclipped values (model predicted only 0/255 channels)`n"
        $overallFail = $true
    } elseif ($maxDelta -le $Tolerance) {
        Write-Host "  PASS: max delta $maxDelta (tolerance $Tolerance)`n"
    } else {
        Write-Host "  FAIL: max delta $maxDelta exceeds tolerance $Tolerance`n"
        $overallFail = $true
    }
}

# ---- Cleanup -------------------------------------------------------------------
if (-not $KeepArtifacts) {
    foreach ($shot in $script:capturedShots) {
        Remove-Item $shot -Force -ErrorAction SilentlyContinue
    }
    Remove-Item $WorkDir -Recurse -Force -ErrorAction SilentlyContinue
} else {
    Write-Host "artifacts retained: $WorkDir + $($script:capturedShots.Count) screenshot(s)"
}

if ($overallFail) {
    Write-Host "RESULT: FAIL"
    exit 1
}
Write-Host "RESULT: PASS (tone-map shader matches the CPU model on PQ, PQ midtones, and HLG)"
exit 0
