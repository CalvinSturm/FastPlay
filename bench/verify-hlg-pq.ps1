<#
.SYNOPSIS
    Pixel-verify the tone-map shader's HLG -> PQ output stage (HdrPqOutput)
    via 10-bit GPU backbuffer readback.

.DESCRIPTION
    Drives the env-gated validation entry (src/render/hdr_validate.rs) in
    shader-hlg mode: one raw NV12 frame of HLG-tagged SMPTE bars is rendered
    through the PRODUCTION HdrToneMapRenderer with the PQ output encode into
    the R10G10B10A2 skeleton swapchain, and the raw backbuffer is dumped
    pre-Present (DWM never touches the measured pixels).

    Oracle: a double-precision CPU model of the shader's HLG branch applied
    to the same NV12 bytes the GPU consumed —
      BT.2020 NCL limited->full YCbCr matrix (clamped)
      -> HLG inverse OETF (scene light)
      -> BT.2100 OOTF, system gamma 1.2 at the 1000-nit nominal peak
      -> PQ inverse EOTF (SMPTE ST 2084)
    Bar centers are flat, so chroma upsampling does not enter.

    Negative control (-WrongTransfer): the validator tags the same HLG bytes
    as PQ, so the shader's passthrough branch runs instead; colored bars
    must diverge grossly, proving the oracle catches a wrong transfer.

.EXAMPLE
    pwsh -File bench/verify-hlg-pq.ps1
    pwsh -File bench/verify-hlg-pq.ps1 -WrongTransfer   # expect the control to trip
#>
[CmdletBinding()]
param(
    [string]$Exe = "$PSScriptRoot\..\target\debug\fastplay.exe",
    [string]$Ffmpeg = "ffmpeg",
    [string]$WorkDir = "$env:TEMP\fastplay-color-verify",
    # 10-bit tolerance ~= +/-2 of 255: f32 shader vs double model plus
    # output quantization.
    [int]$Tolerance = 8,
    [switch]$WrongTransfer
)

$ErrorActionPreference = "Stop"
$W = 1280; $H = 720

if (-not (Test-Path $Exe)) { throw "fastplay binary not found: $Exe (build first)" }
if (-not (Get-Command $Ffmpeg -ErrorAction SilentlyContinue)) { throw "ffmpeg not found on PATH" }
New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null

# --- Test material: HLG/BT.2020-tagged SMPTE bars ---------------------------
$clip = Join-Path $WorkDir "bars2020hlg.mp4"
$nv12 = Join-Path $WorkDir "bars2020hlg.nv12"
if (-not (Test-Path $clip)) {
    & $Ffmpeg -y -hide_banner -loglevel error `
        -f lavfi -i "smptebars=duration=1:size=${W}x${H}:rate=30" `
        -vf "scale=out_color_matrix=bt2020:out_range=tv,format=yuv420p,setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=arib-std-b67:range=tv" `
        -c:v libx264 -preset veryfast $clip
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed generating $clip" }
}
& $Ffmpeg -y -hide_banner -loglevel error -i $clip -frames:v 1 -f rawvideo -pix_fmt nv12 $nv12
if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed extracting NV12" }

# --- Run the validator ------------------------------------------------------
$outBin = Join-Path $WorkDir "backbuffer-r10-shader-hlg.bin"
if (Test-Path $outBin) { Remove-Item $outBin -Force }
$env:FASTPLAY_HDR_VALIDATE_NV12 = $nv12
$env:FASTPLAY_HDR_VALIDATE_SIZE = "${W}x${H}"
$env:FASTPLAY_HDR_VALIDATE_OUT = $outBin
$env:FASTPLAY_HDR_VALIDATE_MODE = "shader-hlg"
if ($WrongTransfer) { $env:FASTPLAY_HDR_VALIDATE_WRONG_MATRIX = "1" }
try {
    & $Exe
    if ($LASTEXITCODE -ne 0) { throw "validator exited with code $LASTEXITCODE" }
}
finally {
    Remove-Item Env:FASTPLAY_HDR_VALIDATE_NV12, Env:FASTPLAY_HDR_VALIDATE_SIZE,
        Env:FASTPLAY_HDR_VALIDATE_OUT, Env:FASTPLAY_HDR_VALIDATE_MODE -ErrorAction SilentlyContinue
    Remove-Item Env:FASTPLAY_HDR_VALIDATE_WRONG_MATRIX -ErrorAction SilentlyContinue
}
if (-not (Test-Path $outBin)) { throw "validator produced no readback file" }

# --- Parse the R10G10B10A2 dump; compute the CPU model ----------------------
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

function HlgInverseOetf([double]$e) {
    # BT.2100 HLG inverse OETF: signal -> scene light in [0,1].
    $a = 0.17883277; $b = 0.28466892; $c = 0.55991073
    if ($e -le 0.5) { return ($e * $e) / 3.0 }
    return ([Math]::Exp(($e - $c) / $a) + $b) / 12.0
}

function PqInverseEotf([double]$l) {
    # SMPTE ST 2084 inverse EOTF; input in units of 10 000 nits.
    $m1 = 0.1593017578125; $m2 = 78.84375
    $c1 = 0.8359375; $c2 = 18.8515625; $c3 = 18.6875
    $y = [Math]::Pow([Math]::Max($l, 0.0), $m1)
    return [Math]::Pow(($c1 + $c2 * $y) / (1.0 + $c3 * $y), $m2)
}

function Get-ReferencePixel([int]$x, [int]$y) {
    # The shader's HLG->PQ branch in double precision, on the same NV12
    # bytes the GPU consumed.
    $Y = [double]$src[$y * $W + $x]
    $uv = $W * $H + [int][Math]::Floor($y / 2) * $W + 2 * [int][Math]::Floor($x / 2)
    $Cb = [double]$src[$uv]; $Cr = [double]$src[$uv + 1]
    $Yn = ($Y - 16.0) / 219.0
    $Cbn = ($Cb - 128.0) / 224.0
    $Crn = ($Cr - 128.0) / 224.0
    # BT.2020 NCL matrix, clamped to [0,1] like the shader's saturate.
    $Rp = [Math]::Max(0.0, [Math]::Min(1.0, $Yn + 1.4746 * $Crn))
    $Gp = [Math]::Max(0.0, [Math]::Min(1.0, $Yn - 0.16455313 * $Cbn - 0.57135313 * $Crn))
    $Bp = [Math]::Max(0.0, [Math]::Min(1.0, $Yn + 1.8814 * $Cbn))
    # HLG inverse OETF -> scene light.
    $sr = HlgInverseOetf $Rp; $sg = HlgInverseOetf $Gp; $sb = HlgInverseOetf $Bp
    # BT.2100 OOTF: display = scene * Ys^0.2 * 1000 nits (system gamma 1.2).
    $ys = [Math]::Max(0.2627 * $sr + 0.6780 * $sg + 0.0593 * $sb, 1e-6)
    $gain = [Math]::Pow($ys, 0.2) * 1000.0
    # PQ encode against the 10 000-nit ceiling.
    @($sr, $sg, $sb) | ForEach-Object {
        $pq = PqInverseEotf (([double]$_ * $gain) / 10000.0)
        [int][Math]::Round(1023.0 * [Math]::Max(0.0, [Math]::Min(1.0, $pq)))
    }
}

$names = @("gray", "yellow", "cyan", "green", "magenta", "red", "blue")
$maxDelta = 0
"{0,-8} {1,-18} {2,-18} {3}" -f "bar", "backbuffer(10b)", "model(10b)", "max|d|"
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
if ($WrongTransfer) {
    if ($maxDelta -gt $Tolerance) {
        "NEGATIVE CONTROL OK: wrong transfer produced max delta $maxDelta (> $Tolerance)"
        exit 0
    }
    "NEGATIVE CONTROL FAILED: wrong transfer still matched (max delta $maxDelta) - oracle is blind"
    exit 1
}
if ($maxDelta -le $Tolerance) {
    "PASS: HLG->PQ shader output within +/-$Tolerance/1023 of the CPU model (max delta $maxDelta)"
    exit 0
}
"FAIL: max per-channel delta $maxDelta exceeds tolerance $Tolerance"
exit 1
