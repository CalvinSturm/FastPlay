<#
.SYNOPSIS
    Pixel-verify FastPlay's HDR10 color-space configuration via 10-bit
    GPU backbuffer readback.

.DESCRIPTION
    Drives the env-gated validation entry (src/render/hdr_validate.rs),
    which renders one raw NV12 frame through the resolved HDR10 color
    spaces (VideoProcessorSet{Stream,Output}ColorSpace1) into an
    R10G10B10A2 swapchain and dumps the backbuffer BEFORE Present — DWM
    never touches the measured pixels. Window capture is useless here: it
    would return the compositor's 8-bit tone-mapped composite of the PQ
    backbuffer, not our plumbing.

    Oracles:
    - Structural: CheckColorSpaceSupport (swapchain creation fails if the
      display path rejects RGB_FULL_G2084_NONE_P2020) and
      CheckVideoProcessorFormatConversion (NV12+input space ->
      R10G10B10A2+output space), both printed by the validator. The
      conversion check also rejects the -WrongMatrix pair on real drivers.
    - Pixel: the 7 SMPTE bars. The GPU consumes the exact NV12 bytes that
      ffmpeg decoded from the clip; the reference applies ITU-R BT.2020
      NCL limited->full arithmetic to those same bytes in double
      precision. (swscale's own YUV->RGB integer path was measured ~-6/1023
      below spec math and was removed from the loop; the GPU sits ~+2..+8
      above it due to its 8->10-bit promotion arithmetic. Bar centers are
      flat so chroma upsampling does not enter.) Transfer stays PQ-encoded
      on both sides - no linearization, raw 10-bit code values only.
    - Negative control (-WrongMatrix): forces the SDR BT.709 input space
      through the same machinery; colored bars must diverge (measured
      35-44 vs <=8 for the correct constants).

.EXAMPLE
    pwsh -File bench/verify-colors-pq.ps1
    pwsh -File bench/verify-colors-pq.ps1 -WrongMatrix       # expect FAIL
    pwsh -File bench/verify-colors-pq.ps1 -Mode shader-pq    # tone-map shader, PQ output

.NOTES
    -Mode vp        (default) the dedicated hdr10_validation_blt through the
                    video processor with ColorSpace1 configuration.
    -Mode shader-pq the PRODUCTION tone-map shader in PQ-output mode
                    (HdrPqOutput). PQ code values pass through the shader
                    bit-transparently (the YCbCr->R'G'B' matrix is the whole
                    conversion), so the same spec-math reference applies.
                    Its -WrongMatrix control forces the HLG transfer instead
                    of the SDR BT.709 space.
#>
[CmdletBinding()]
param(
    [string]$Exe = "$PSScriptRoot\..\target\debug\fastplay.exe",
    [string]$Ffmpeg = "ffmpeg",
    [string]$WorkDir = "$env:TEMP\fastplay-color-verify",
    # 10-bit tolerance, ~= +/-3 of 255. Tighter than the SDR harness's
    # +/-4-of-255; clears the measured driver rounding (max +8, from the
    # GPU promoting 8-bit NV12 to 10-bit as x*1023/255 vs the reference's
    # x*4) while staying far under the wrong-matrix signal (35+).
    [int]$Tolerance = 12,
    [ValidateSet("vp", "shader-pq")]
    [string]$Mode = "vp",
    [switch]$WrongMatrix,
    # shader-pq only: generate FULL-range PQ bars and tag the input
    # full-range (the Topaz Video AI "HDR Enhanced" 8-bit export shape).
    [switch]$FullRange
)
if ($FullRange -and $Mode -ne "shader-pq") { throw "-FullRange applies to -Mode shader-pq only" }

$ErrorActionPreference = "Stop"
$W = 1280; $H = 720

if (-not (Test-Path $Exe)) { throw "fastplay binary not found: $Exe (build first)" }
if (-not (Get-Command $Ffmpeg -ErrorAction SilentlyContinue)) { throw "ffmpeg not found on PATH" }
New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null

# --- Test material: PQ/BT.2020-tagged SMPTE bars ----------------------------
# smptebars RGB -> BT.2020 NCL limited-range YCbCr, then tagged PQ. The
# transfer tag reinterprets the values; correctness of YCbCr<->RGB math is
# what we measure, and both sides of the comparison use the same encoding.
$rangeSlug = if ($FullRange) { "full" } else { "tv" }
$clip = Join-Path $WorkDir "bars2020pq-$rangeSlug.mp4"
$nv12 = Join-Path $WorkDir "bars2020pq-$rangeSlug.nv12"
if (-not (Test-Path $clip)) {
    $rangeArg = if ($FullRange) { "pc" } else { "tv" }
    & $Ffmpeg -y -hide_banner -loglevel error `
        -f lavfi -i "smptebars=duration=1:size=${W}x${H}:rate=30" `
        -vf "scale=out_color_matrix=bt2020:out_range=$rangeArg,format=yuv420p,setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=smpte2084:range=$rangeArg" `
        -c:v libx264 -preset veryfast $clip
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed generating $clip" }
}
# The exact decoded NV12 bytes: uploaded verbatim by the validator AND used
# as the input to the spec-math reference below, so the decoder cannot
# disagree with itself.
& $Ffmpeg -y -hide_banner -loglevel error -i $clip -frames:v 1 -f rawvideo -pix_fmt nv12 $nv12
if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed extracting NV12" }

# --- Run the validator ------------------------------------------------------
$outBin = Join-Path $WorkDir "backbuffer-r10-$Mode-$rangeSlug.bin"
if (Test-Path $outBin) { Remove-Item $outBin -Force }
$env:FASTPLAY_HDR_VALIDATE_NV12 = $nv12
$env:FASTPLAY_HDR_VALIDATE_SIZE = "${W}x${H}"
$env:FASTPLAY_HDR_VALIDATE_OUT = $outBin
$env:FASTPLAY_HDR_VALIDATE_MODE = $Mode
if ($WrongMatrix) { $env:FASTPLAY_HDR_VALIDATE_WRONG_MATRIX = "1" }
if ($FullRange) { $env:FASTPLAY_HDR_VALIDATE_FULL_RANGE = "1" }
try {
    & $Exe
    if ($LASTEXITCODE -ne 0) { throw "validator exited with code $LASTEXITCODE" }
}
finally {
    Remove-Item Env:FASTPLAY_HDR_VALIDATE_NV12, Env:FASTPLAY_HDR_VALIDATE_SIZE,
        Env:FASTPLAY_HDR_VALIDATE_OUT, Env:FASTPLAY_HDR_VALIDATE_MODE -ErrorAction SilentlyContinue
    Remove-Item Env:FASTPLAY_HDR_VALIDATE_WRONG_MATRIX, Env:FASTPLAY_HDR_VALIDATE_FULL_RANGE -ErrorAction SilentlyContinue
}
if (-not (Test-Path $outBin)) { throw "validator produced no readback file" }

# --- Parse the R10G10B10A2 dump; compute the spec-math reference ------------
$bin = [System.IO.File]::ReadAllBytes($outBin)
if ([System.Text.Encoding]::ASCII.GetString($bin, 0, 5) -ne "R10A2") { throw "bad dump magic" }
$bw = [BitConverter]::ToUInt32($bin, 6); $bh = [BitConverter]::ToUInt32($bin, 10)
if ($bw -ne $W -or $bh -ne $H) { throw "dump is ${bw}x${bh}, expected ${W}x${H}" }
$pixOff = 14
$src = [System.IO.File]::ReadAllBytes($nv12)
if ($src.Length -ne $W * $H * 3 / 2) { throw "NV12 is $($src.Length) bytes, expected $($W*$H*3/2)" }

function Get-BackbufferPixel([int]$x, [int]$y) {
    # DXGI R10G10B10A2: one LE dword, R in bits 0-9, G 10-19, B 20-29.
    $dw = [BitConverter]::ToUInt32($bin, $pixOff + 4 * ($y * $W + $x))
    @(($dw -band 0x3FF), (($dw -shr 10) -band 0x3FF), (($dw -shr 20) -band 0x3FF))
}
function Get-ReferencePixel([int]$x, [int]$y) {
    # ITU-R BT.2020 non-constant-luminance in double precision on the same
    # NV12 bytes the GPU consumed; studio (16-235/16-240) or full (0-255,
    # chroma centered on 128) normalization per -FullRange. Sampled
    # positions are flat bar interiors, so nearest-neighbor chroma is exact.
    $Y = [double]$src[$y * $W + $x]
    $uv = $W * $H + [int][Math]::Floor($y / 2) * $W + 2 * [int][Math]::Floor($x / 2)
    $Cb = [double]$src[$uv]; $Cr = [double]$src[$uv + 1]
    if ($FullRange) {
        $Yn = $Y / 255.0
        $Cbn = ($Cb - 128.0) / 255.0
        $Crn = ($Cr - 128.0) / 255.0
    } else {
        $Yn = ($Y - 16.0) / 219.0
        $Cbn = ($Cb - 128.0) / 224.0
        $Crn = ($Cr - 128.0) / 224.0
    }
    # BT.2020 NCL: Kr = 0.2627, Kb = 0.0593.
    $R = $Yn + 1.4746 * $Crn
    $G = $Yn - 0.16455313 * $Cbn - 0.57135313 * $Crn
    $B = $Yn + 1.8814 * $Cbn
    @($R, $G, $B) | ForEach-Object {
        [int][Math]::Round(1023.0 * [Math]::Max(0.0, [Math]::Min(1.0, [double]$_)))
    }
}

$names = @("gray", "yellow", "cyan", "green", "magenta", "red", "blue")
$maxDelta = 0
"{0,-8} {1,-18} {2,-18} {3}" -f "bar", "backbuffer(10b)", "reference(10b)", "max|d|"
foreach ($i in 0..6) {
    $x = [int]($W * ($i + 0.5) / 7.0); $y = [int]($H * 0.30)
    $c = Get-BackbufferPixel $x $y
    $r = Get-ReferencePixel $x $y
    $d = [Math]::Max([Math]::Max(
            [Math]::Abs($c[0] - $r[0]),
            [Math]::Abs($c[1] - $r[1])),
        [Math]::Abs($c[2] - $r[2]))
    if ($d -gt $maxDelta) { $maxDelta = $d }
    "{0,-8} {1,-18} {2,-18} {3}" -f $names[$i],
        "($($c[0]),$($c[1]),$($c[2]))", "($($r[0]),$($r[1]),$($r[2]))", $d
}
""
if ($WrongMatrix) {
    if ($maxDelta -gt $Tolerance) {
        "NEGATIVE CONTROL OK: wrong input color space produced max delta $maxDelta (> $Tolerance)"
        exit 0
    }
    "NEGATIVE CONTROL FAILED: wrong color space still matched (max delta $maxDelta) - oracle is blind"
    exit 1
}
if ($maxDelta -le $Tolerance) {
    "PASS ($Mode): HDR10 backbuffer within +/-$Tolerance/1023 of ffmpeg reference (max delta $maxDelta)"
    exit 0
}
"FAIL: max per-channel delta $maxDelta exceeds tolerance $Tolerance"
exit 1
