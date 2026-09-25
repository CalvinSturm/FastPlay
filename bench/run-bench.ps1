<#
.SYNOPSIS
    Latency benchmark for FastPlay: open-to-first-frame, seek-to-first-frame,
    and seek-to-A/V-settled across codecs, resolutions, HDR modes, and decode
    paths, with per-event attribution and median/p95 summaries.

.DESCRIPTION
    The harness drives the real release build and reads back the metrics the
    app already logs (src/app/session.rs); nothing is timed from outside the
    process, so harness scheduling jitter does not leak into the numbers.

    For every iteration x clip x variant ('hw' = default hardware path,
    'sw' = --force-sw) one run does:

      1. launch the player on the clip under a fresh, isolated %APPDATA%
         (no resume-at-position, no Recent-list pollution, private log dir)
      2. wait -OpenWaitMs                          -> launch open
      3. -WarmReopens times: WM_APP+4, wait         -> in-process reopens
      4. for each -SeekFractions: WM_APP+3 with the absolute target in ms,
         wait -SeekGapMs                            -> seeks
      5. pause, resume                              -> pause/resume latency
      6. -Playthrough only: reopen, play to the end -> drops/underruns/fallbacks
      7. WM_CLOSE (the log ring flushes only on graceful exit)

    The flushed session log is then parsed *sequentially*: each `[open]` and
    `[execute_seek]` line opens a new event and the metric lines that follow
    are attributed to it. An event whose metric never arrives before the next
    one starts is recorded with a failure status instead of silently vanishing
    from the sample set.

    Iterations are the outer loop so machine drift (thermals, background
    load) spreads across all configurations instead of biasing one.

    Outputs, in -OutDir\<stamp>[-<Label>]\:
      events.csv   one row per open / reopen / seek / pause / resume
      runs.csv     one row per process launch
      summary.csv  n / ok / failed / median / p95 / min / max per group
      report.json  all of the above + media info + environment + config
      runs\<id>\FastPlay\session-*.log  raw logs, preserved for re-parsing

.EXAMPLE
    pwsh -File bench/gen-corpus.ps1
    pwsh -File bench/run-bench.ps1 -Label baseline

.EXAMPLE
    pwsh -File bench/run-bench.ps1 -CorpusDir bench\corpus,D:\media\clip.mkv -Variants hw -Iterations 10
#>
[CmdletBinding()]
param(
    [string]$Exe = "$PSScriptRoot\..\target\release\fastplay.exe",
    # Directories (scanned for media) and/or individual media files.
    [string[]]$CorpusDir = @("$PSScriptRoot\corpus"),
    [string]$OutDir = "$PSScriptRoot\results",
    [string]$Label = "",
    [int]$Iterations = 5,
    [ValidateSet('hw', 'sw')][string[]]$Variants = @('hw', 'sw'),
    # Seek targets as fractions of duration. The defaults land at varied
    # offsets past a keyframe on a 4 s GOP (1.2, 0.8, 3.8, 0.0, 1.6 s on the
    # 40 s synthetic corpus) and mix forward and backward jumps.
    [double[]]$SeekFractions = @(0.53, 0.12, 0.895, 0.30, 0.74),
    [int]$WarmReopens = 2,
    [int]$OpenWaitMs = 2500,
    # Longer than the slowest expected seek: a seek still pending when the next
    # one is posted is superseded, and recorded as such rather than timed.
    [int]$SeekGapMs = 3000,
    # After the latency phase, reopen and play the whole clip to its natural
    # end so the playback_summary counters (drops, underruns, HW fallbacks)
    # cover one clean pass. Adds roughly the clip duration to every run.
    [switch]$Playthrough,
    [string]$Ffprobe = "ffprobe"
)

$ErrorActionPreference = "Stop"

$Exe = (Resolve-Path $Exe -ErrorAction SilentlyContinue)?.Path
if (-not $Exe) { throw "Player not found. Build it first: cargo build --release" }
if (-not (Get-Command $Ffprobe -ErrorAction SilentlyContinue)) { throw "ffprobe not found on PATH (needed for media info)" }

$mediaExt = '.mp4', '.mkv', '.mov', '.webm', '.avi', '.m4v', '.ts'
$clips = @(foreach ($entry in $CorpusDir) {
        if (Test-Path $entry -PathType Container) {
            Get-ChildItem $entry -File | Where-Object { $_.Extension -in $mediaExt }
        }
        elseif (Test-Path $entry -PathType Leaf) { Get-Item $entry }
        else { throw "Not found: $entry" }
    })
if ($clips.Count -eq 0) { throw "No media files in: $($CorpusDir -join ', ')" }

Import-Module (Join-Path $PSScriptRoot "FastPlayLog.psm1") -Force

Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class FpBenchWin {
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr p);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  public delegate bool EnumProc(IntPtr h, IntPtr p);
  public static IntPtr Find(uint targetPid) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, p) => {
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (pid != targetPid) return true;
      var sb = new StringBuilder(512); GetWindowTextW(h, sb, 512);
      string t = sb.ToString();
      if (t.EndsWith(" - FastPlay") || t == "FastPlay") { found = h; return false; }
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
"@

$WM_CHAR = 0x0102; $WM_CLOSE = 0x0010; $SPACE = 0x20
$WM_APP = 0x8000
$WM_APP_SEEK_TO_MS = $WM_APP + 3      # src/ffi/dxgi.rs
$WM_APP_REOPEN_CURRENT = $WM_APP + 4  # src/ffi/dxgi.rs

function Post([IntPtr]$h, [int]$msg, [long]$w) {
    [void][FpBenchWin]::PostMessageW($h, [uint32]$msg, [IntPtr]$w, [IntPtr]::Zero)
}

# ---- Media info (ffprobe) ----
function Get-MediaInfo([System.IO.FileInfo]$file) {
    $j = & $Ffprobe -v error -show_entries "stream=codec_type,codec_name,profile,width,height,pix_fmt,r_frame_rate,color_transfer,color_primaries:format=format_name,duration,bit_rate" -of json $file.FullName | ConvertFrom-Json
    $v = @($j.streams | Where-Object codec_type -eq 'video')[0]
    $a = @($j.streams | Where-Object codec_type -eq 'audio') | Select-Object -First 1
    $trc = if ($v.PSObject.Properties['color_transfer']) { $v.color_transfer } else { '' }
    $dynamic = switch ($trc) { 'smpte2084' { 'PQ' } 'arib-std-b67' { 'HLG' } default { 'SDR' } }
    $fpsParts = "$($v.r_frame_rate)".Split('/')
    $fps = if ($fpsParts.Count -eq 2 -and [double]$fpsParts[1] -ne 0) { [math]::Round([double]$fpsParts[0] / [double]$fpsParts[1], 3) } else { $null }
    # Keyframe PTS list, so each seek target can be tagged with how far past
    # its keyframe it lands (the decode-forward distance a seek pays).
    $keys = @(& $Ffprobe -v error -select_streams v:0 -show_entries packet=pts_time,flags -of csv=p=0 $file.FullName |
        Where-Object { $_ -match '^([\d.]+),K' } | ForEach-Object { [double]($_.Split(',')[0]) } | Sort-Object)
    [pscustomobject]@{
        clip         = $file.Name
        path         = $file.FullName
        size_mb      = [math]::Round($file.Length / 1MB, 1)
        container    = $j.format.format_name
        duration_s   = [math]::Round([double]$j.format.duration, 3)
        bitrate_kbps = if ($j.format.PSObject.Properties['bit_rate']) { [math]::Round([double]$j.format.bit_rate / 1000) } else { $null }
        codec        = $v.codec_name
        profile      = if ($v.PSObject.Properties['profile']) { $v.profile } else { '' }
        width        = [int]$v.width
        height       = [int]$v.height
        fps          = $fps
        pix_fmt      = $v.pix_fmt
        bit_depth    = if ($v.pix_fmt -match '10|p010') { 10 } elseif ($v.pix_fmt -match '12') { 12 } else { 8 }
        transfer     = $trc
        dynamic_range = $dynamic
        audio_codec  = if ($a) { $a.codec_name } else { '' }
        keyframes_s  = $keys
    }
}

function Get-KeyframeDistanceMs($info, [double]$targetS) {
    $prev = @($info.keyframes_s | Where-Object { $_ -le $targetS + 0.0005 })
    if ($prev.Count -eq 0) { return $null }
    [math]::Round(($targetS - $prev[-1]) * 1000)
}

# ---- Log parsing: attribute each metric line to the event it follows ----
function Read-RunLog([string]$path) {
    $run = [ordered]@{ startup_to_open_ms = $null; display_hdr_active = $null; errors = @(); interference = @(); playback_summary = $null }
    $opens = [System.Collections.Generic.List[object]]::new()
    $seeks = [System.Collections.Generic.List[object]]::new()
    $pauses = [System.Collections.Generic.List[object]]::new()
    $resumes = [System.Collections.Generic.List[object]]::new()
    $open = $null; $seek = $null
    foreach ($line in [System.IO.File]::ReadLines($path)) {
        if ($line -match '^startup_to_open_ms=(\d+)') { $run.startup_to_open_ms = [int]$Matches[1] }
        elseif ($line -match '^\[open\] open=(\d+) start_ms=(\d+)') {
            $seek = $null
            $open = [ordered]@{ gen = [int]$Matches[1]; start_ms = [int]$Matches[2]; first_frame_ms = $null; first_audio_ms = $null; decode_mode = $null; presentation_path = $null; hw_fallback_count = 0 }
            $opens.Add($open)
        }
        elseif ($line -match '^\[spawn_decode_thread\].*display_hdr_active: (true|false)') {
            if ($null -eq $run.display_hdr_active) { $run.display_hdr_active = $Matches[1] -eq 'true' }
        }
        elseif ($line -match '^\[presentation_path\] open=\d+ path=(\S+)') { if ($open) { $open.presentation_path = $Matches[1] } }
        elseif ($line -match '^decode_mode=(\S+) hw_fallback_count=(\d+)') {
            if ($open) {
                if (-not $open.decode_mode) { $open.decode_mode = $Matches[1] }
                $open.hw_fallback_count = [int]$Matches[2]
            }
            if ($seek) { $seek.decode_mode = $Matches[1] }
        }
        elseif ($line -match '^open_to_first_frame_ms=(\d+)') { if ($open) { $open.first_frame_ms = [int]$Matches[1] } }
        elseif ($line -match '^open_to_first_audio_ms=(\d+)') { if ($open) { $open.first_audio_ms = [int]$Matches[1] } }
        elseif ($line -match '^\[execute_seek\] pos=([\d.]+)s') {
            $seek = [ordered]@{ pos_s = [double]$Matches[1]; first_frame_ms = $null; settled_ms = $null; decode_mode = $null }
            $seeks.Add($seek)
        }
        elseif ($line -match '^seek_to_first_frame_ms=(\d+)') { if ($seek) { $seek.first_frame_ms = [int]$Matches[1] } }
        elseif ($line -match '^seek_to_av_settled_ms=(\d+)') { if ($seek) { $seek.settled_ms = [int]$Matches[1] } }
        elseif ($line -match '^playback_summary decode_mode=(\S+) hw_fallback_count=(\d+) presented_frames=(\d+) dropped_video_frames=(\d+) audio_underruns=(\d+)') {
            $run.playback_summary = [ordered]@{ decode_mode = $Matches[1]; hw_fallback_count = [int]$Matches[2]; presented_frames = [int]$Matches[3]; dropped_video_frames = [int]$Matches[4]; audio_underruns = [int]$Matches[5] }
        }
        elseif ($line -match '^pause_to_stop_ms=(\d+)') { $pauses.Add([int]$Matches[1]) }
        elseif ($line -match '^play_to_motion_ms=(\d+)') { $resumes.Add([int]$Matches[1]) }
        elseif ($line -match '^(panic|open failed|\[seek\] rejected)|device.?lost|recover_device') { $run.errors += $line }
        # The harness never changes volume, rotation, zoom, or rate: these mean
        # physical input reached the benchmark window (it launches in the
        # foreground), so the run's timings cannot be trusted.
        elseif ($line -match '^(volume=|rotation|zoom|playback_rate|frameless_windowed=|menu_bar_mode=)') { $run.interference += $line }
    }
    [pscustomobject]@{ run = $run; opens = $opens; seeks = $seeks; pauses = $pauses; resumes = $resumes }
}

# ---- Stats ----
function Get-Median([double[]]$v) {
    if ($v.Count -eq 0) { return $null }
    $s = [double[]]($v | Sort-Object); $m = [int][math]::Floor($s.Count / 2)
    if ($s.Count % 2) { $s[$m] } else { ($s[$m - 1] + $s[$m]) / 2 }
}
# Nearest-rank p95; $null below 10 samples, where it would just be the max.
function Get-P95([double[]]$v) {
    if ($v.Count -lt 10) { return $null }
    $s = [double[]]($v | Sort-Object)
    $s[[int][math]::Ceiling(0.95 * $s.Count) - 1]
}

# ---- Environment ----
$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$runDirName = if ($Label) { "$stamp-$Label" } else { $stamp }
$resultDir = Join-Path $OutDir $runDirName
$profilesDir = Join-Path $resultDir "runs"
New-Item -ItemType Directory -Force -Path $profilesDir | Out-Null

$repo = Split-Path $PSScriptRoot -Parent
$gitCommit = (git -C $repo rev-parse --short HEAD 2>$null)
$gitDirty = [bool](git -C $repo status --porcelain --untracked-files=no 2>$null)
$gpus = @(Get-CimInstance Win32_VideoController | ForEach-Object { "$($_.Name) (driver $($_.DriverVersion), $($_.CurrentHorizontalResolution)x$($_.CurrentVerticalResolution)@$($_.CurrentRefreshRate)Hz)" })
$cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1).Name
$os = (Get-CimInstance Win32_OperatingSystem)
$environment = [ordered]@{
    generated_utc = (Get-Date).ToUniversalTime().ToString("o")
    machine       = $env:COMPUTERNAME
    os            = "$($os.Caption) $($os.Version)"
    cpu           = $cpu
    gpus          = $gpus
    git_commit    = $gitCommit
    git_dirty     = $gitDirty
    exe           = $Exe
    exe_sha256    = (Get-FileHash $Exe -Algorithm SHA256).Hash
    exe_mtime     = (Get-Item $Exe).LastWriteTime.ToString("o")
}

Write-Host "Probing $($clips.Count) clip(s)..."
$media = [ordered]@{}
foreach ($c in $clips) { $media[$c.FullName] = Get-MediaInfo $c }

# ---- Run ----
$events = [System.Collections.Generic.List[object]]::new()
$runs = [System.Collections.Generic.List[object]]::new()
$firstLaunchSeen = @{}
$total = $Iterations * $clips.Count * $Variants.Count
$n = 0
$originalAppData = $env:APPDATA

for ($iter = 1; $iter -le $Iterations; $iter++) {
    foreach ($clip in $clips) {
        $info = $media[$clip.FullName]
        foreach ($variant in $Variants) {
            $n++
            $runId = "{0:D3}-{1}-{2}" -f $n, [IO.Path]::GetFileNameWithoutExtension($clip.Name), $variant
            Write-Host ("[{0}/{1}] iter {2} {3} [{4}]" -f $n, $total, $iter, $clip.Name, $variant)
            $profileDir = Join-Path $profilesDir $runId
            New-Item -ItemType Directory -Force -Path $profileDir | Out-Null
            $seekTargetsMs = @($SeekFractions | ForEach-Object { [long][math]::Round($_ * $info.duration_s * 1000) })

            $status = 'ok'; $proc = $null; $launch = Get-Date
            try {
                $argList = @()
                if ($variant -eq 'sw') { $argList += '--force-sw' }
                $argList += "`"$($clip.FullName)`""
                $env:APPDATA = $profileDir
                try { $proc = Start-Process -FilePath $Exe -ArgumentList $argList -PassThru }
                finally { $env:APPDATA = $originalAppData }

                $hwnd = [IntPtr]::Zero
                for ($w = 0; $w -lt 100 -and $hwnd -eq [IntPtr]::Zero -and -not $proc.HasExited; $w++) {
                    Start-Sleep -Milliseconds 100
                    $hwnd = [FpBenchWin]::Find([uint32]$proc.Id)
                }
                if ($hwnd -eq [IntPtr]::Zero) { throw "render window not found" }

                Start-Sleep -Milliseconds $OpenWaitMs
                for ($r = 0; $r -lt $WarmReopens; $r++) {
                    Post $hwnd $WM_APP_REOPEN_CURRENT 0
                    Start-Sleep -Milliseconds $OpenWaitMs
                }
                foreach ($t in $seekTargetsMs) {
                    Post $hwnd $WM_APP_SEEK_TO_MS $t
                    Start-Sleep -Milliseconds $SeekGapMs
                }
                Post $hwnd $WM_CHAR $SPACE
                Start-Sleep -Milliseconds 600
                Post $hwnd $WM_CHAR $SPACE
                Start-Sleep -Milliseconds 800
                if ($Playthrough) {
                    # Rewind first: a reopen past 30 s would resume mid-file.
                    Post $hwnd $WM_APP_SEEK_TO_MS 0
                    Start-Sleep -Milliseconds 500
                    Post $hwnd $WM_APP_REOPEN_CURRENT 0
                    Start-Sleep -Milliseconds ([int]($info.duration_s * 1000) + 3000)
                }

                Post $hwnd $WM_CLOSE 0
                if (-not $proc.WaitForExit(10000)) { $status = 'close_timeout' }
            }
            catch { $status = "harness_error: $($_.Exception.Message)" }
            finally {
                if ($proc -and -not $proc.HasExited) { $proc | Stop-Process -Force }
            }

            $logPath = if ($proc) {
                Resolve-FastPlayRunLog -ProcessId $proc.Id -LaunchTime $launch -LogDir (Join-Path $profileDir 'FastPlay')
            }
            $parsed = if ($logPath) { Read-RunLog $logPath } else { $null }
            if (-not $parsed -and $status -eq 'ok') { $status = 'no_log' }
            if ($parsed -and $parsed.run.errors.Count -and $status -eq 'ok') { $status = 'app_errors' }

            $common = [ordered]@{
                run = $runId; iteration = $iter; clip = $info.clip; variant = $variant
                codec = $info.codec; width = $info.width; height = $info.height; fps = $info.fps
                bit_depth = $info.bit_depth; dynamic_range = $info.dynamic_range; bitrate_kbps = $info.bitrate_kbps
            }
            function Add-Event([hashtable]$fields) {
                $row = [ordered]@{}
                foreach ($k in $common.Keys) { $row[$k] = $common[$k] }
                foreach ($k in 'event', 'index', 'open_kind', 'target_ms', 'keyframe_distance_ms', 'first_frame_ms', 'settled_ms', 'first_audio_ms', 'latency_ms', 'decode_mode', 'presentation_path', 'hw_fallback_count', 'status') {
                    $row[$k] = if ($fields.ContainsKey($k)) { $fields[$k] } else { $null }
                }
                $events.Add([pscustomobject]$row)
            }

            if ($parsed) {
                for ($i = 0; $i -lt $parsed.opens.Count; $i++) {
                    $o = $parsed.opens[$i]
                    $kind = if ($i -gt 0) { 'reopen' }
                    elseif (-not $firstLaunchSeen.ContainsKey("$($clip.FullName)|$variant")) { 'launch_first' }
                    else { 'launch_repeat' }
                    Add-Event @{
                        event = 'open'; index = $i; open_kind = $kind
                        first_frame_ms = $o.first_frame_ms; first_audio_ms = $o.first_audio_ms
                        decode_mode = $o.decode_mode; presentation_path = $o.presentation_path; hw_fallback_count = $o.hw_fallback_count
                        status = if ($o.start_ms -ne 0) { 'resumed' } elseif ($null -ne $o.first_frame_ms) { 'ok' } else { 'no_first_frame' }
                    }
                }
                $expectedOpens = 1 + $WarmReopens
                # The playthrough reopen is not a latency sample: its open
                # follows the pause/resume and would skew the reopen group.
                if ($Playthrough -and $parsed.opens.Count -gt $expectedOpens) {
                    $parsed.opens.RemoveAt($parsed.opens.Count - 1)
                }
                for ($i = $parsed.opens.Count; $i -lt $expectedOpens; $i++) {
                    Add-Event @{ event = 'open'; index = $i; open_kind = if ($i) { 'reopen' } else { 'launch' }; status = 'missing' }
                }
                $openMode = if ($parsed.opens.Count) { $parsed.opens[-1].decode_mode } else { $null }
                for ($i = 0; $i -lt $seekTargetsMs.Count; $i++) {
                    $target = $seekTargetsMs[$i]
                    $s = if ($i -lt $parsed.seeks.Count) { $parsed.seeks[$i] } else { $null }
                    $st = if (-not $s) { 'missing' }
                    elseif ([math]::Abs($s.pos_s * 1000 - $target) -gt 2) { 'target_mismatch' }
                    elseif ($null -eq $s.first_frame_ms) { if ($i + 1 -lt $parsed.seeks.Count) { 'superseded' } else { 'no_first_frame' } }
                    elseif ($null -eq $s.settled_ms) { if ($i + 1 -lt $parsed.seeks.Count) { 'superseded_before_settle' } else { 'not_settled' } }
                    else { 'ok' }
                    Add-Event @{
                        event = 'seek'; index = $i; target_ms = $target
                        keyframe_distance_ms = Get-KeyframeDistanceMs $info ($target / 1000.0)
                        first_frame_ms = if ($s) { $s.first_frame_ms } else { $null }; settled_ms = if ($s) { $s.settled_ms } else { $null }
                        decode_mode = if ($s -and $s.decode_mode) { $s.decode_mode } else { $openMode }
                        status = $st
                    }
                }
                $expectedSeeks = $seekTargetsMs.Count + [int][bool]$Playthrough
                if ($parsed.seeks.Count -gt $expectedSeeks) {
                    $parsed.run.interference += "unexpected extra seeks: $($parsed.seeks.Count) > $expectedSeeks"
                }
                Add-Event @{ event = 'pause'; latency_ms = @($parsed.pauses)[0]; status = if ($parsed.pauses.Count) { 'ok' } else { 'missing' } }
                Add-Event @{ event = 'resume'; latency_ms = @($parsed.resumes)[-1]; status = if ($parsed.resumes.Count) { 'ok' } else { 'missing' } }
            }
            if ($parsed -and $parsed.run.interference.Count) {
                if ($status -eq 'ok') { $status = 'interfered' }
                foreach ($ev in $events) { if ($ev.run -eq $runId) { $ev.status = 'interfered' } }
            }
            $firstLaunchSeen["$($clip.FullName)|$variant"] = $true

            $runs.Add([pscustomobject][ordered]@{
                    run = $runId; iteration = $iter; clip = $info.clip; variant = $variant; status = $status
                    startup_to_open_ms = if ($parsed) { $parsed.run.startup_to_open_ms } else { $null }
                    launch_to_first_frame_ms = if ($parsed -and $null -ne $parsed.run.startup_to_open_ms -and $parsed.opens.Count -and $null -ne $parsed.opens[0].first_frame_ms) { $parsed.run.startup_to_open_ms + $parsed.opens[0].first_frame_ms } else { $null }
                    display_hdr_active = if ($parsed) { $parsed.run.display_hdr_active } else { $null }
                    presented_frames = if ($parsed -and $parsed.run.playback_summary) { $parsed.run.playback_summary.presented_frames } else { $null }
                    dropped_video_frames = if ($parsed -and $parsed.run.playback_summary) { $parsed.run.playback_summary.dropped_video_frames } else { $null }
                    audio_underruns = if ($parsed -and $parsed.run.playback_summary) { $parsed.run.playback_summary.audio_underruns } else { $null }
                    hw_fallback_count = if ($parsed -and $parsed.run.playback_summary) { $parsed.run.playback_summary.hw_fallback_count } else { $null }
                    errors = if ($parsed) { ($parsed.run.errors -join ' | ') } else { '' }
                    interference = if ($parsed) { ($parsed.run.interference | Select-Object -First 3) -join ' | ' } else { '' }
                    log = $logPath
                })
            if ($status -ne 'ok') { Write-Warning "  $runId -> $status $(if ($parsed) { (@($parsed.run.errors) + @($parsed.run.interference | Select-Object -First 2)) -join ' | ' })" }
        }
    }
}

# ---- Aggregate ----
$summary = [System.Collections.Generic.List[object]]::new()
# Interfered runs are excluded outright; any other non-ok event counts as a
# failure (a missing sample is a result, not noise).
function Add-Summary($clip, $variant, $metric, $rows, [string]$field, $meta) {
    $rows = @($rows | Where-Object { -not $_.PSObject.Properties['status'] -or $_.status -ne 'interfered' })
    $ok = @($rows | Where-Object { $null -ne $_.$field })
    $vals = [double[]]@($ok | ForEach-Object { $_.$field })
    $summary.Add([pscustomobject][ordered]@{
            clip = $clip; variant = $variant; metric = $metric
            codec = $meta.codec; resolution = "$($meta.width)x$($meta.height)"; dynamic_range = $meta.dynamic_range
            decode_mode = (@($rows | ForEach-Object { if ($_.PSObject.Properties['decode_mode']) { $_.decode_mode } } | Where-Object { $_ } | Sort-Object -Unique) -join '+')
            n = $rows.Count; ok = $ok.Count; failed = $rows.Count - $ok.Count
            median = Get-Median $vals; p95 = Get-P95 $vals
            min = if ($vals.Count) { ($vals | Measure-Object -Minimum).Minimum } else { $null }
            max = if ($vals.Count) { ($vals | Measure-Object -Maximum).Maximum } else { $null }
        })
}
foreach ($c in $clips) {
    $meta = $media[$c.FullName]
    foreach ($variant in $Variants) {
        $e = @($events | Where-Object { $_.clip -eq $meta.clip -and $_.variant -eq $variant })
        $launch = @($e | Where-Object { $_.event -eq 'open' -and $_.open_kind -like 'launch*' })
        $reopen = @($e | Where-Object { $_.event -eq 'open' -and $_.open_kind -eq 'reopen' })
        $seeks = @($e | Where-Object event -eq 'seek')
        $r = @($runs | Where-Object { $_.clip -eq $meta.clip -and $_.variant -eq $variant -and $_.status -ne 'interfered' })
        Add-Summary $meta.clip $variant 'open_to_first_frame_ms (launch)' $launch 'first_frame_ms' $meta
        Add-Summary $meta.clip $variant 'open_to_first_frame_ms (launch_first)' @($launch | Where-Object open_kind -eq 'launch_first') 'first_frame_ms' $meta
        Add-Summary $meta.clip $variant 'open_to_first_frame_ms (reopen)' $reopen 'first_frame_ms' $meta
        Add-Summary $meta.clip $variant 'launch_to_first_frame_ms' $r 'launch_to_first_frame_ms' $meta
        Add-Summary $meta.clip $variant 'seek_to_first_frame_ms' $seeks 'first_frame_ms' $meta
        Add-Summary $meta.clip $variant 'seek_to_av_settled_ms' $seeks 'settled_ms' $meta
        Add-Summary $meta.clip $variant 'pause_to_stop_ms' @($e | Where-Object event -eq 'pause') 'latency_ms' $meta
        Add-Summary $meta.clip $variant 'play_to_motion_ms' @($e | Where-Object event -eq 'resume') 'latency_ms' $meta
    }
}

$config = [ordered]@{
    corpus = $CorpusDir; iterations = $Iterations; variants = $Variants; seek_fractions = $SeekFractions
    warm_reopens = $WarmReopens; open_wait_ms = $OpenWaitMs; seek_gap_ms = $SeekGapMs; playthrough = [bool]$Playthrough
}
$report = [ordered]@{
    environment = $environment; config = $config
    media = @($media.Values | ForEach-Object {
            $row = $_ | Select-Object * -ExcludeProperty keyframes_s
            $row | Add-Member NoteProperty keyframe_count @($_.keyframes_s).Count
            $row
        })
    summary = $summary; runs = $runs; events = $events
}
$report | ConvertTo-Json -Depth 6 | Set-Content -Path (Join-Path $resultDir 'report.json') -Encoding UTF8
$events | Export-Csv -Path (Join-Path $resultDir 'events.csv') -NoTypeInformation -Encoding UTF8
$runs | Export-Csv -Path (Join-Path $resultDir 'runs.csv') -NoTypeInformation -Encoding UTF8
$summary | Export-Csv -Path (Join-Path $resultDir 'summary.csv') -NoTypeInformation -Encoding UTF8

# ---- Console ----
function Fmt($v) { if ($null -eq $v) { '-' } else { '{0:N0}' -f $v } }
Write-Host ""
Write-Host ("FastPlay latency benchmark  commit={0}{1}  display_hdr_active={2}" -f $gitCommit, $(if ($gitDirty) { '+dirty' } else { '' }), (@($runs | ForEach-Object display_hdr_active | Sort-Object -Unique) -join '/'))
Write-Host ("{0,-22} {1,-3} {2,-12} {3,11} {4,11} {5,11} {6,11} {7,7}" -f 'clip', 'var', 'mode', 'launch p50', 'reopen p50', 'seekFF p50', 'settle p50', 'settle')
Write-Host ("{0,-22} {1,-3} {2,-12} {3,11} {4,11} {5,11} {6,11} {7,7}" -f '', '', '', '', '', '(p95)', '(p95)', 'fails')
foreach ($g in ($summary | Group-Object clip, variant)) {
    $m = @{}; foreach ($s in $g.Group) { $m[$s.metric] = $s }
    $l = $m['open_to_first_frame_ms (launch)']; $ro = $m['open_to_first_frame_ms (reopen)']
    $ff = $m['seek_to_first_frame_ms']; $st = $m['seek_to_av_settled_ms']
    $name = if ($l.clip.Length -gt 22) { $l.clip.Substring(0, 22) } else { $l.clip }
    Write-Host ("{0,-22} {1,-3} {2,-12} {3,11} {4,11} {5,11} {6,11} {7,7}" -f $name, $l.variant, $l.decode_mode, (Fmt $l.median), (Fmt $ro.median),
        ("{0} ({1})" -f (Fmt $ff.median), (Fmt $ff.p95)), ("{0} ({1})" -f (Fmt $st.median), (Fmt $st.p95)), ("{0}/{1}" -f $st.failed, $st.n))
}
$bad = @($runs | Where-Object status -ne 'ok')
Write-Host ""
Write-Host ("runs={0} non_ok_runs={1} (interfered={2}, excluded from stats)  non_ok_events={3}" -f $runs.Count, $bad.Count,
    @($runs | Where-Object status -eq 'interfered').Count, @($events | Where-Object { $_.status -ne 'ok' -and $_.status -ne 'interfered' }).Count)
Write-Host "Results: $resultDir"
