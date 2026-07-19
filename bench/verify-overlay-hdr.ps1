<#
.SYNOPSIS
    Pixel-verify the overlay renderer's PQ-chain shader variant via 10-bit
    GPU backbuffer readback.

.DESCRIPTION
    Drives the env-gated validation entry (src/render/hdr_validate.rs) in
    overlay mode: a full-viewport flat sRGB (200,120,60) overlay — left
    half opaque, right half alpha 128 — is drawn through the PRODUCTION
    SubtitleRenderer's HDR pixel shader into the R10G10B10A2 skeleton
    swapchain cleared to opaque black, and the raw backbuffer is dumped
    pre-Present.

    Oracle: a double-precision CPU model of the shader —
      sRGB decode -> BT.709->BT.2020 (BT.2087 matrix)
      -> scale to 203-nit reference white on the 10 000-nit PQ ceiling
      -> SMPTE ST 2084 inverse EOTF
    The opaque half must match that directly; the half-alpha half must
    match it scaled by 128/255 (straight-alpha blend over the black clear,
    performed in PQ space — exactly what production does).

.EXAMPLE
    pwsh -File bench/verify-overlay-hdr.ps1
#>
[CmdletBinding()]
param(
    [string]$Exe = "$PSScriptRoot\..\target\debug\fastplay.exe",
    [string]$WorkDir = "$env:TEMP\fastplay-color-verify",
    # f32 shader vs double model + blend rounding + 10-bit quantization.
    [int]$Tolerance = 8
)

$ErrorActionPreference = "Stop"
$W = 1280; $H = 720

if (-not (Test-Path $Exe)) { throw "fastplay binary not found: $Exe (build first)" }
New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null

# --- Run the validator ------------------------------------------------------
$outBin = Join-Path $WorkDir "backbuffer-r10-overlay.bin"
if (Test-Path $outBin) { Remove-Item $outBin -Force }
$env:FASTPLAY_HDR_VALIDATE_NV12 = "unused-by-overlay-mode"
$env:FASTPLAY_HDR_VALIDATE_SIZE = "${W}x${H}"
$env:FASTPLAY_HDR_VALIDATE_OUT = $outBin
$env:FASTPLAY_HDR_VALIDATE_MODE = "overlay"
try {
    & $Exe
    if ($LASTEXITCODE -ne 0) { throw "validator exited with code $LASTEXITCODE" }
}
finally {
    Remove-Item Env:FASTPLAY_HDR_VALIDATE_NV12, Env:FASTPLAY_HDR_VALIDATE_SIZE,
        Env:FASTPLAY_HDR_VALIDATE_OUT, Env:FASTPLAY_HDR_VALIDATE_MODE -ErrorAction SilentlyContinue
}
if (-not (Test-Path $outBin)) { throw "validator produced no readback file" }

# --- Parse the dump; compute the CPU model ----------------------------------
$bin = [System.IO.File]::ReadAllBytes($outBin)
if ([System.Text.Encoding]::ASCII.GetString($bin, 0, 5) -ne "R10A2") { throw "bad dump magic" }
$bw = [BitConverter]::ToUInt32($bin, 6); $bh = [BitConverter]::ToUInt32($bin, 10)
if ($bw -ne $W -or $bh -ne $H) { throw "dump is ${bw}x${bh}, expected ${W}x${H}" }
$pixOff = 14

function Get-BackbufferPixel([int]$x, [int]$y) {
    $dw = [BitConverter]::ToUInt32($bin, $pixOff + 4 * ($y * $W + $x))
    @(($dw -band 0x3FF), (($dw -shr 10) -band 0x3FF), (($dw -shr 20) -band 0x3FF))
}

function SrgbDecode([double]$e) {
    if ($e -le 0.04045) { return $e / 12.92 }
    return [Math]::Pow(($e + 0.055) / 1.055, 2.4)
}
function PqInverseEotf([double]$l) {
    $m1 = 0.1593017578125; $m2 = 78.84375
    $c1 = 0.8359375; $c2 = 18.8515625; $c3 = 18.6875
    $y = [Math]::Pow([Math]::Max($l, 0.0), $m1)
    return [Math]::Pow(($c1 + $c2 * $y) / (1.0 + $c3 * $y), $m2)
}

# The validator's flat overlay color, sRGB.
$srgb = @((200.0 / 255.0), (120.0 / 255.0), (60.0 / 255.0))
$lin = $srgb | ForEach-Object { SrgbDecode $_ }
# BT.709 -> BT.2020 (BT.2087), linear light.
$r2020 = 0.627404 * $lin[0] + 0.329283 * $lin[1] + 0.043313 * $lin[2]
$g2020 = 0.069097 * $lin[0] + 0.919540 * $lin[1] + 0.011362 * $lin[2]
$b2020 = 0.016391 * $lin[0] + 0.088013 * $lin[1] + 0.895595 * $lin[2]
# 203-nit reference white on the PQ ceiling, PQ-encoded.
$pq = @($r2020, $g2020, $b2020) | ForEach-Object { PqInverseEotf ($_ * 203.0 / 10000.0) }

$opaqueRef = $pq | ForEach-Object { [int][Math]::Round(1023.0 * $_) }
# Half-alpha over the opaque-black clear: straight-alpha blend in PQ space.
$halfRef = $pq | ForEach-Object { [int][Math]::Round(1023.0 * $_ * 128.0 / 255.0) }

$maxDelta = 0
"{0,-14} {1,-18} {2,-18} {3}" -f "region", "backbuffer(10b)", "model(10b)", "max|d|"
foreach ($case in @(
        @{ name = "opaque"; x = [int]($W / 4); ref = $opaqueRef },
        @{ name = "alpha128"; x = [int](3 * $W / 4); ref = $halfRef })) {
    $c = Get-BackbufferPixel $case.x ([int]($H / 2))
    $r = $case.ref
    $d = [Math]::Max([Math]::Max(
            [Math]::Abs($c[0] - $r[0]),
            [Math]::Abs($c[1] - $r[1])),
        [Math]::Abs($c[2] - $r[2]))
    if ($d -gt $maxDelta) { $maxDelta = $d }
    "{0,-14} {1,-18} {2,-18} {3}" -f $case.name,
        "($($c[0]),$($c[1]),$($c[2]))", "($($r[0]),$($r[1]),$($r[2]))", $d
}
""
if ($maxDelta -le $Tolerance) {
    "PASS: PQ-chain overlay shader within +/-$Tolerance/1023 of the CPU model (max delta $maxDelta)"
    exit 0
}
"FAIL: max per-channel delta $maxDelta exceeds tolerance $Tolerance"
exit 1
