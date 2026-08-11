<#
.SYNOPSIS
    Deterministic tests for FastPlayLog.psm1 run-log resolution.

.DESCRIPTION
    Covers the recycled-PID hole directly: a stale session-*-<pid>.log left by
    an earlier run must never satisfy a lookup for the current run, because an
    HDR oracle asserting on log contents would then pass while testing nothing.

    Launches no processes and touches no real log directory — every case runs
    against a temp dir with synthesized files, so it is safe to run anywhere and
    always produces the same answer. Exits 0 when all cases pass.

.EXAMPLE
    pwsh -File bench/test-log-resolution.ps1
#>
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Import-Module (Join-Path $PSScriptRoot 'FastPlayLog.psm1') -Force

$script:failures = 0
$script:passes = 0

function Assert-That([string]$Name, [scriptblock]$Condition) {
    $ok = $false
    try { $ok = [bool](& $Condition) } catch { $ok = $false }
    if ($ok) { $script:passes++; Write-Host "  PASS  $Name" }
    else { $script:failures++; Write-Host "  FAIL  $Name" -ForegroundColor Red }
}

$dir = Join-Path ([System.IO.Path]::GetTempPath()) "fastplay-logres-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Force -Path $dir | Out-Null

try {
    $pid1 = 4242
    $launch = Get-Date

    # ---- A stale log from a recycled PID must be rejected -------------------
    $stale = Join-Path $dir "session-20260101T000000Z-$pid1.log"
    'stale run: path=HdrPqOutput' | Set-Content $stale
    (Get-Item $stale).LastWriteTime = $launch.AddMinutes(-30)

    Assert-That 'stale log is not returned' {
        $null -eq (Resolve-FastPlayRunLog -ProcessId $pid1 -LaunchTime $launch -LogDir $dir)
    }

    Assert-That '-Required throws rather than accepting a stale log' {
        try { Resolve-FastPlayRunLog -ProcessId $pid1 -LaunchTime $launch -LogDir $dir -Required | Out-Null; $false }
        catch { $_.Exception.Message -match 'recycled PID' }
    }

    Assert-That 'stale log still exists (resolution must not delete it)' { Test-Path $stale }

    # ---- A log written after launch is returned -----------------------------
    $fresh = Join-Path $dir "session-20260810T120000Z-$pid1.log"
    'this run: path=HdrPqOutput' | Set-Content $fresh
    (Get-Item $fresh).LastWriteTime = $launch.AddSeconds(30)

    Assert-That 'fresh log is returned' {
        (Resolve-FastPlayRunLog -ProcessId $pid1 -LaunchTime $launch -LogDir $dir) -eq $fresh
    }

    Assert-That 'fresh log wins over the stale one for the same PID' {
        (Get-Content (Resolve-FastPlayRunLog -ProcessId $pid1 -LaunchTime $launch -LogDir $dir -Required)) -match 'this run'
    }

    # ---- Another instance's log must not be picked up -----------------------
    $other = Join-Path $dir "session-20260810T120500Z-9999.log"
    'other instance' | Set-Content $other
    (Get-Item $other).LastWriteTime = $launch.AddSeconds(90)

    Assert-That 'a newer log from a different PID is ignored' {
        (Resolve-FastPlayRunLog -ProcessId $pid1 -LaunchTime $launch -LogDir $dir) -eq $fresh
    }

    # ---- Clear removes pre-existing matches, and only those -----------------
    Clear-FastPlayRunLog -ProcessId $pid1 -LogDir $dir
    Assert-That 'Clear removed both logs for the target PID' {
        -not (Test-Path $stale) -and -not (Test-Path $fresh)
    }
    Assert-That 'Clear left the other PID alone' { Test-Path $other }

    Assert-That 'after Clear, -Required throws for the missing run' {
        try { Resolve-FastPlayRunLog -ProcessId $pid1 -LaunchTime $launch -LogDir $dir -Required | Out-Null; $false }
        catch { $true }
    }

    # ---- Missing directory is a clean miss, not a crash ---------------------
    Assert-That 'absent log dir yields $null rather than throwing' {
        $null -eq (Resolve-FastPlayRunLog -ProcessId $pid1 -LaunchTime $launch -LogDir (Join-Path $dir 'nope'))
    }
}
finally {
    Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host ""
if ($script:failures -gt 0) {
    Write-Host "FAILED: $($script:failures) of $($script:passes + $script:failures) checks" -ForegroundColor Red
    exit 1
}
Write-Host "PASS: all $($script:passes) run-log resolution checks"
exit 0
