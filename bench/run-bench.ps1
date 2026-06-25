<#
.SYNOPSIS
    Benchmark harness for FastPlay. Drives the release build over a corpus and
    aggregates the metrics it logs to session.log into p50/p95 reports.

.DESCRIPTION
    For each clip x iteration the harness launches the player, drives a scripted
    workload via PostMessageW (open -> seeks -> pause/resume -> play to end ->
    graceful close), then parses %APPDATA%\FastPlay\session.log. It reports the
    roadmap metrics (docs/ROADMAP.md section 2):

      - open_to_first_frame_ms        (open-to-first-frame)
      - seek_to_first_frame_ms        (seek-to-first-frame)
      - seek_to_av_settled_ms         (seek-to-A/V-settled)
      - pause_to_stop_ms              (pause latency)
      - play_to_motion_ms             (resume latency)
      - dropped_video_frames          (drops over a full playthrough)
      - audio_underruns               (underruns over a full playthrough)
      - hw_fallback_count             (hardware-decode fallbacks)

    Latency metrics are reported as p50/p95 across all samples. The playthrough
    counters are reported as totals/max. Use a larger -CorpusDir and
    -Iterations for stable percentiles.

    The player must be a release build (the debug build is a console-subsystem
    app whose window the title match would miss). session.log only flushes on a
    graceful exit, so the harness always closes via WM_CLOSE.

.EXAMPLE
    pwsh -File bench/gen-corpus.ps1
    pwsh -File bench/run-bench.ps1 -Iterations 5

.EXAMPLE
    pwsh -File bench/run-bench.ps1 -CorpusDir D:\media\bench -Iterations 10 -LatencyOnly
#>
[CmdletBinding()]
param(
    [string]$Exe = "$PSScriptRoot\..\target\release\fastplay.exe",
    [string]$CorpusDir = "$PSScriptRoot\corpus",
    [string]$OutDir = "$PSScriptRoot\results",
    [int]$Iterations = 3,
    [int]$SeeksPerRun = 5,
    [switch]$LatencyOnly,
    [string]$Ffprobe = "ffprobe"
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $Exe)) {
    throw "Player not found: $Exe`nBuild it first: cargo build --release"
}
if (-not (Test-Path $CorpusDir)) {
    throw "Corpus dir not found: $CorpusDir`nGenerate one: pwsh -File bench/gen-corpus.ps1"
}
$clips = @(Get-ChildItem $CorpusDir -File | Where-Object { $_.Extension -in '.mp4', '.mkv', '.mov', '.webm', '.avi' })
if ($clips.Count -eq 0) { throw "No media files in $CorpusDir" }

$log = "$env:APPDATA\FastPlay\session.log"
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class Win {
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr p);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  public delegate bool EnumProc(IntPtr h, IntPtr p);
  public static IntPtr Found = IntPtr.Zero;
  public static uint TargetPid = 0;
  public static bool Cb(IntPtr h, IntPtr p) {
    uint pid; GetWindowThreadProcessId(h, out pid);
    if (pid != TargetPid) return true;
    var sb = new StringBuilder(512); GetWindowTextW(h, sb, 512);
    string t = sb.ToString();
    if (t.EndsWith(" - FastPlay") || t == "FastPlay") { Found = h; return false; }
    return true;
  }
}
"@

$WM_CHAR = 0x0102; $WM_KEYDOWN = 0x0100; $WM_CLOSE = 0x0010
$SPACE = 0x20; $VK_RIGHT = 0x27; $VK_LEFT = 0x25

function Get-DurationSeconds([string]$path) {
    if (Get-Command $Ffprobe -ErrorAction SilentlyContinue) {
        try {
            $d = & $Ffprobe -v error -show_entries format=duration -of csv=p=0 "$path" 2>$null
            if ($d) { return [double]$d }
        }
        catch {}
    }
    return 30.0  # fallback when ffprobe is unavailable
}

function Find-Window([int]$processId) {
    [Win]::TargetPid = [uint32]$processId
    for ($i = 0; $i -lt 50; $i++) {
        [Win]::Found = [IntPtr]::Zero
        [Win]::EnumWindows([Win+EnumProc] { param($h, $p) [Win]::Cb($h, $p) }, [IntPtr]::Zero) | Out-Null
        if ([Win]::Found -ne [IntPtr]::Zero) { return [Win]::Found }
        Start-Sleep -Milliseconds 200
    }
    return [IntPtr]::Zero
}

function Wait-ForLogLine([string]$pattern, [double]$timeoutSec) {
    $deadline = (Get-Date).AddSeconds($timeoutSec)
    while ((Get-Date) -lt $deadline) {
        if ((Test-Path $log) -and (Select-String -Path $log -Pattern $pattern -Quiet)) { return $true }
        Start-Sleep -Milliseconds 150
    }
    return $false
}

function Post([IntPtr]$h, [int]$msg, [int]$w, [int]$l) {
    [Win]::PostMessageW($h, [uint32]$msg, [IntPtr]$w, [IntPtr]$l) | Out-Null
}

# Nearest-rank percentile (p in 0..100).
function Get-Percentile([double[]]$values, [double]$p) {
    if (-not $values -or $values.Count -eq 0) { return $null }
    $sorted = $values | Sort-Object
    $rank = [math]::Ceiling(($p / 100.0) * $sorted.Count)
    if ($rank -lt 1) { $rank = 1 }
    if ($rank -gt $sorted.Count) { $rank = $sorted.Count }
    return [double]$sorted[$rank - 1]
}

function Get-AllMatches([string]$text, [string]$pattern) {
    $vals = @()
    foreach ($m in [regex]::Matches($text, $pattern)) {
        $vals += [double]$m.Groups[1].Value
    }
    return , $vals
}

function Invoke-Run([string]$clip, [double]$duration) {
    if (Test-Path $log) { Remove-Item $log -Force }
    $proc = Start-Process -FilePath $Exe -ArgumentList "`"$clip`"" -PassThru
    try {
        $hwnd = Find-Window $proc.Id
        if ($hwnd -eq [IntPtr]::Zero) { throw "render window not found" }

        # Wait for the first frame so the open metric is logged before we seek.
        [void](Wait-ForLogLine "open_to_first_frame_ms=" 10)

        # Seeks: alternate mostly-forward with an occasional back, letting each settle.
        for ($s = 0; $s -lt $SeeksPerRun; $s++) {
            $key = if ($s -gt 0 -and $s % 4 -eq 0) { $VK_LEFT } else { $VK_RIGHT }
            Post $hwnd $WM_KEYDOWN $key 0
            Start-Sleep -Milliseconds 700
        }

        # Pause then resume.
        Post $hwnd $WM_CHAR $SPACE 0
        Start-Sleep -Milliseconds 600
        Post $hwnd $WM_CHAR $SPACE 0
        Start-Sleep -Milliseconds 600

        if (-not $LatencyOnly) {
            # Let playback reach the end so the playback_summary line is emitted.
            [void](Wait-ForLogLine "playback_summary" ([math]::Ceiling($duration) + 15))
        }

        Post $hwnd $WM_CLOSE 0 0
        if (-not $proc.WaitForExit(8000)) { $proc | Stop-Process -Force; throw "did not exit gracefully" }
    }
    finally {
        if (-not $proc.HasExited) { $proc | Stop-Process -Force }
    }

    $text = Get-Content $log -Raw
    $openArr = Get-AllMatches $text 'open_to_first_frame_ms=(\d+)'
    $summaryDrop = Get-AllMatches $text 'playback_summary .*?dropped_video_frames=(\d+)'
    $summaryUnder = Get-AllMatches $text 'playback_summary .*?audio_underruns=(\d+)'
    $summaryFallback = Get-AllMatches $text 'playback_summary .*?hw_fallback_count=(\d+)'

    return [pscustomobject]@{
        open_to_first_frame_ms = if ($openArr.Count) { $openArr[0] } else { $null }
        seek_to_first_frame_ms = Get-AllMatches $text 'seek_to_first_frame_ms=(\d+)'
        seek_to_av_settled_ms  = Get-AllMatches $text 'seek_to_av_settled_ms=(\d+)'
        pause_to_stop_ms       = Get-AllMatches $text 'pause_to_stop_ms=(\d+)'
        play_to_motion_ms      = Get-AllMatches $text 'play_to_motion_ms=(\d+)'
        dropped_video_frames   = if ($summaryDrop.Count) { $summaryDrop[-1] } else { $null }
        audio_underruns        = if ($summaryUnder.Count) { $summaryUnder[-1] } else { $null }
        hw_fallback_count      = if ($summaryFallback.Count) { $summaryFallback[-1] } else { $null }
    }
}

# ---- Run ----
$latencySamples = @{
    open_to_first_frame_ms = @()
    seek_to_first_frame_ms = @()
    seek_to_av_settled_ms  = @()
    pause_to_stop_ms       = @()
    play_to_motion_ms      = @()
}
$totalDrops = 0; $totalUnderruns = 0; $maxFallback = 0; $playthroughs = 0
$runRows = @()

$total = $clips.Count * $Iterations
$n = 0
foreach ($clip in $clips) {
    $dur = Get-DurationSeconds $clip.FullName
    for ($i = 1; $i -le $Iterations; $i++) {
        $n++
        Write-Host ("[{0}/{1}] {2} (iter {3}, {4:N0}s)" -f $n, $total, $clip.Name, $i, $dur)
        $r = Invoke-Run $clip.FullName $dur

        if ($null -ne $r.open_to_first_frame_ms) { $latencySamples.open_to_first_frame_ms += $r.open_to_first_frame_ms }
        $latencySamples.seek_to_first_frame_ms += $r.seek_to_first_frame_ms
        $latencySamples.seek_to_av_settled_ms += $r.seek_to_av_settled_ms
        $latencySamples.pause_to_stop_ms += $r.pause_to_stop_ms
        $latencySamples.play_to_motion_ms += $r.play_to_motion_ms

        if ($null -ne $r.dropped_video_frames) {
            $playthroughs++
            $totalDrops += $r.dropped_video_frames
            $totalUnderruns += $r.audio_underruns
            if ($r.hw_fallback_count -gt $maxFallback) { $maxFallback = $r.hw_fallback_count }
        }

        $runRows += [pscustomobject]@{
            clip                   = $clip.Name
            iteration              = $i
            open_to_first_frame_ms = $r.open_to_first_frame_ms
            seeks                  = $r.seek_to_first_frame_ms.Count
            dropped_video_frames   = $r.dropped_video_frames
            audio_underruns        = $r.audio_underruns
            hw_fallback_count      = $r.hw_fallback_count
        }
    }
}

# ---- Aggregate ----
$summary = [ordered]@{}
foreach ($key in $latencySamples.Keys) {
    $vals = [double[]]$latencySamples[$key]
    $summary[$key] = [ordered]@{
        count = $vals.Count
        p50   = Get-Percentile $vals 50
        p95   = Get-Percentile $vals 95
        min   = if ($vals.Count) { ($vals | Measure-Object -Minimum).Minimum } else { $null }
        max   = if ($vals.Count) { ($vals | Measure-Object -Maximum).Maximum } else { $null }
    }
}
$summary['playthroughs'] = $playthroughs
$summary['dropped_video_frames_total'] = $totalDrops
$summary['audio_underruns_total'] = $totalUnderruns
$summary['hw_fallback_count_max'] = $maxFallback

$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$report = [ordered]@{
    generated_utc = (Get-Date).ToUniversalTime().ToString("o")
    exe           = $Exe
    corpus_dir    = $CorpusDir
    clips         = ($clips | ForEach-Object { $_.Name })
    iterations    = $Iterations
    seeks_per_run = $SeeksPerRun
    latency_only  = [bool]$LatencyOnly
    summary       = $summary
    runs          = $runRows
}

$jsonPath = Join-Path $OutDir "bench-$stamp.json"
$csvPath = Join-Path $OutDir "bench-$stamp.csv"
$report | ConvertTo-Json -Depth 6 | Set-Content -Path $jsonPath -Encoding UTF8
$runRows | Export-Csv -Path $csvPath -NoTypeInformation -Encoding UTF8

# ---- Console summary ----
Write-Host ""
Write-Host "=== FastPlay benchmark (p50 / p95, ms) ==="
foreach ($key in $latencySamples.Keys) {
    $s = $summary[$key]
    $p50 = if ($null -ne $s.p50) { "{0,7:N0}" -f $s.p50 } else { "      -" }
    $p95 = if ($null -ne $s.p95) { "{0,7:N0}" -f $s.p95 } else { "      -" }
    Write-Host ("{0,-26} p50={1}  p95={2}  (n={3})" -f $key, $p50, $p95, $s.count)
}
Write-Host ""
Write-Host ("playthroughs={0}  dropped_frames_total={1}  underruns_total={2}  hw_fallback_max={3}" -f `
        $playthroughs, $totalDrops, $totalUnderruns, $maxFallback)
Write-Host ""
Write-Host "JSON: $jsonPath"
Write-Host "CSV:  $csvPath"
