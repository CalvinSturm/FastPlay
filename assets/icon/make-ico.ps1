<#
.SYNOPSIS
    Build assets/icon/fastplay.ico from a square RGBA master PNG.

.DESCRIPTION
    Generates the six sizes Windows consumes (256, 128, 64, 48, 32, 16),
    lanczos-downscaled by ffmpeg, and assembles a classic .ico: each entry
    a 32-bit BGRA BMP (bottom-up) with a zeroed, row-padded AND mask — the
    alpha channel carries all transparency. No PNG-compressed entries, for
    maximum consumer compatibility (VS shell, old dialogs).

    The master must be square RGBA with a transparent background; the
    artwork should fill ~85% of the canvas so small sizes stay legible.

.EXAMPLE
    pwsh -File assets/icon/make-ico.ps1 -Master assets/icon/fastplay-glyph.png
#>
[CmdletBinding()]
param(
    [string]$Master = "$PSScriptRoot\fastplay-glyph.png",
    [string]$Out = "$PSScriptRoot\fastplay.ico",
    [string]$Ffmpeg = "ffmpeg"
)
$ErrorActionPreference = "Stop"
if (-not (Test-Path $Master)) { throw "master PNG not found: $Master" }
if (-not (Get-Command $Ffmpeg -ErrorAction SilentlyContinue)) { throw "ffmpeg not found on PATH" }

$sizes = @(256, 128, 64, 48, 32, 16)
$tmp = Join-Path $env:TEMP "fastplay-ico-build"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

$images = foreach ($s in $sizes) {
    $raw = Join-Path $tmp "icon-$s.bgra"
    & $Ffmpeg -y -hide_banner -loglevel error -i $Master `
        -vf "scale=${s}:${s}:flags=lanczos,format=bgra" `
        -f rawvideo -pix_fmt bgra $raw
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed scaling to ${s}px" }
    $bgra = [System.IO.File]::ReadAllBytes($raw)
    if ($bgra.Length -ne $s * $s * 4) { throw "unexpected raw size for ${s}px" }

    # BITMAPINFOHEADER + XOR (bottom-up BGRA) + zeroed AND mask (1bpp,
    # rows padded to 32-bit). Alpha carries the real transparency.
    $andRowBytes = [Math]::Ceiling($s / 32.0) * 4
    $ms = New-Object System.IO.MemoryStream
    $bw = New-Object System.IO.BinaryWriter($ms)
    $bw.Write([uint32]40); $bw.Write([int]$s); $bw.Write([int]($s * 2))
    $bw.Write([uint16]1); $bw.Write([uint16]32); $bw.Write([uint32]0)
    $bw.Write([uint32]($s * $s * 4)); $bw.Write([int]0); $bw.Write([int]0)
    $bw.Write([uint32]0); $bw.Write([uint32]0)
    for ($y = $s - 1; $y -ge 0; $y--) { $bw.Write($bgra, $y * $s * 4, $s * 4) }
    $bw.Write((New-Object byte[] ($andRowBytes * $s)))
    $bw.Flush()
    ,@($s, $ms.ToArray())
}

$outMs = New-Object System.IO.MemoryStream
$obw = New-Object System.IO.BinaryWriter($outMs)
$obw.Write([uint16]0); $obw.Write([uint16]1); $obw.Write([uint16]$images.Count)
$offset = 6 + 16 * $images.Count
foreach ($img in $images) {
    $s = $img[0]; $data = $img[1]
    $obw.Write([byte]($(if ($s -eq 256) { 0 } else { $s })))
    $obw.Write([byte]($(if ($s -eq 256) { 0 } else { $s })))
    $obw.Write([byte]0); $obw.Write([byte]0)
    $obw.Write([uint16]1); $obw.Write([uint16]32)
    $obw.Write([uint32]$data.Length); $obw.Write([uint32]$offset)
    $offset += $data.Length
}
foreach ($img in $images) { $obw.Write($img[1]) }
$obw.Flush()
[System.IO.File]::WriteAllBytes($Out, $outMs.ToArray())
Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
"wrote $Out ($([System.IO.File]::ReadAllBytes($Out).Length) bytes, $($images.Count) entries)"
