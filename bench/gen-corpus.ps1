<#
.SYNOPSIS
    Generate the synthetic latency-benchmark corpus for FastPlay using ffmpeg.

.DESCRIPTION
    Produces a fixed, reproducible set of clips covering the decode/present
    paths run-bench.ps1 reports on, so the harness can run without shipping
    media in the repo:

      codec  res    range  file
      H.264  1080p  SDR    h264_1080p30_sdr.mp4
      H.264  2160p  SDR    h264_2160p30_sdr.mp4
      HEVC   1080p  SDR    hevc_1080p30_sdr.mp4
      HEVC   2160p  PQ     hevc_2160p30_pq.mp4     (Main10, BT.2020, HDR10)
      HEVC   1080p  HLG    hevc_1080p30_hlg.mp4    (Main10, BT.2020, HLG)

    Every clip is 40 s at 30 fps with a fixed 4 s GOP (-g 120, no scene-cut
    keyframes) so seek cost — the distance from a target back to its keyframe —
    is identical across codecs and across runs. Content is testsrc2 with
    temporal noise so the encoders hit a realistic bitrate instead of the
    near-zero rate a static test pattern compresses to. Audio is a 48 kHz
    stereo AAC tone.

    The software path is not a separate file: run-bench.ps1 re-runs clips with
    --force-sw. Clips are written to bench/corpus, which is git-ignored.

.EXAMPLE
    pwsh -File bench/gen-corpus.ps1
    pwsh -File bench/gen-corpus.ps1 -Force
#>
[CmdletBinding()]
param(
    [string]$OutDir = "$PSScriptRoot\corpus",
    [string]$Ffmpeg = "ffmpeg",
    [int]$Seconds = 40,
    [switch]$Force
)

$ErrorActionPreference = "Stop"

if (-not (Get-Command $Ffmpeg -ErrorAction SilentlyContinue)) {
    throw "ffmpeg not found on PATH (looked for '$Ffmpeg'). Install ffmpeg or pass -Ffmpeg <path>."
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$gop = 120
$x265Common = "keyint=${gop}:min-keyint=${gop}:scenecut=0:log-level=error"
$bt709 = @('-color_primaries', 'bt709', '-color_trc', 'bt709', '-colorspace', 'bt709')
$bt2020 = @('-color_primaries', 'bt2020', '-colorspace', 'bt2020nc')

$clips = @(
    @{ name = 'h264_1080p30_sdr'; w = 1920; h = 1080; rate = '12M'
        codec = @('-c:v', 'libx264', '-preset', 'veryfast', '-pix_fmt', 'yuv420p', '-g', $gop, '-sc_threshold', '0') + $bt709
    },
    @{ name = 'h264_2160p30_sdr'; w = 3840; h = 2160; rate = '40M'
        codec = @('-c:v', 'libx264', '-preset', 'veryfast', '-pix_fmt', 'yuv420p', '-g', $gop, '-sc_threshold', '0') + $bt709
    },
    @{ name = 'hevc_1080p30_sdr'; w = 1920; h = 1080; rate = '8M'
        codec = @('-c:v', 'libx265', '-preset', 'ultrafast', '-pix_fmt', 'yuv420p', '-tag:v', 'hvc1',
            '-x265-params', "${x265Common}:colorprim=bt709:transfer=bt709:colormatrix=bt709") + $bt709
    },
    @{ name = 'hevc_2160p30_pq'; w = 3840; h = 2160; rate = '30M'
        codec = @('-c:v', 'libx265', '-preset', 'ultrafast', '-pix_fmt', 'yuv420p10le', '-profile:v', 'main10', '-tag:v', 'hvc1',
            '-x265-params', "${x265Common}:hdr10=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400") +
        $bt2020 + @('-color_trc', 'smpte2084')
    },
    @{ name = 'hevc_1080p30_hlg'; w = 1920; h = 1080; rate = '10M'
        codec = @('-c:v', 'libx265', '-preset', 'ultrafast', '-pix_fmt', 'yuv420p10le', '-profile:v', 'main10', '-tag:v', 'hvc1',
            '-x265-params', "${x265Common}:colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc") +
        $bt2020 + @('-color_trc', 'arib-std-b67')
    }
)

foreach ($c in $clips) {
    $out = Join-Path $OutDir "$($c.name).mp4"
    if ((Test-Path $out) -and -not $Force) {
        Write-Host "skip (exists): $out"
        continue
    }
    Write-Host "generating: $out"
    $ffArgs = @('-y', '-hide_banner', '-loglevel', 'error',
        '-f', 'lavfi', '-i', "testsrc2=duration=${Seconds}:size=$($c.w)x$($c.h):rate=30,noise=alls=10:allf=t",
        '-f', 'lavfi', '-i', "sine=frequency=440:sample_rate=48000:duration=${Seconds}",
        '-map', '0:v', '-map', '1:a') + $c.codec +
    @('-b:v', $c.rate, '-maxrate', $c.rate, '-bufsize', $c.rate,
        '-c:a', 'aac', '-ac', '2', '-b:a', '160k', '-movflags', '+faststart', '-shortest', $out)
    & $Ffmpeg @ffArgs
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed for $out" }
}

Write-Host ""
Write-Host "Corpus ready in $OutDir"
Get-ChildItem $OutDir -Filter *.mp4 | ForEach-Object { Write-Host "  $($_.Name) ($([math]::Round($_.Length/1MB, 1)) MB)" }
