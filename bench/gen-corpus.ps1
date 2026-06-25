<#
.SYNOPSIS
    Generate a small synthetic benchmark corpus for FastPlay using ffmpeg.

.DESCRIPTION
    Produces a handful of H.264 + AAC clips of varied resolution/duration so the
    harness can run out-of-the-box without shipping media in the repo. For
    representative numbers, point run-bench.ps1 at a -CorpusDir of real files
    instead (the roadmap calls for a fixed local corpus).

    Clips are written to bench/corpus, which is git-ignored.

.EXAMPLE
    pwsh -File bench/gen-corpus.ps1
    pwsh -File bench/gen-corpus.ps1 -Force
#>
[CmdletBinding()]
param(
    [string]$OutDir = "$PSScriptRoot\corpus",
    [string]$Ffmpeg = "ffmpeg",
    [switch]$Force
)

$ErrorActionPreference = "Stop"

if (-not (Get-Command $Ffmpeg -ErrorAction SilentlyContinue)) {
    throw "ffmpeg not found on PATH (looked for '$Ffmpeg'). Install ffmpeg or pass -Ffmpeg <path>."
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

# name; width; height; fps; seconds; audio-hz
$clips = @(
    @{ name = "360p30_20s"; w = 640;  h = 360;  fps = 30; secs = 20; hz = 440 },
    @{ name = "720p30_30s"; w = 1280; h = 720;  fps = 30; secs = 30; hz = 480 },
    @{ name = "1080p30_20s"; w = 1920; h = 1080; fps = 30; secs = 20; hz = 520 }
)

foreach ($c in $clips) {
    $out = Join-Path $OutDir "$($c.name).mp4"
    if ((Test-Path $out) -and -not $Force) {
        Write-Host "skip (exists): $out"
        continue
    }
    Write-Host "generating: $out"
    & $Ffmpeg -y -hide_banner -loglevel error `
        -f lavfi -i "testsrc=duration=$($c.secs):size=$($c.w)x$($c.h):rate=$($c.fps)" `
        -f lavfi -i "sine=frequency=$($c.hz):duration=$($c.secs)" `
        -c:v libx264 -pix_fmt yuv420p -preset veryfast `
        -c:a aac -shortest $out
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed for $out" }
}

Write-Host ""
Write-Host "Corpus ready in $OutDir"
Get-ChildItem $OutDir -Filter *.mp4 | ForEach-Object { Write-Host "  $($_.Name) ($([math]::Round($_.Length/1KB)) KB)" }
