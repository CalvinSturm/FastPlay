<#
.SYNOPSIS
    Resolve the session log belonging to a specific FastPlay run.

.DESCRIPTION
    Each run writes %APPDATA%\FastPlay\session-<utc-stamp>-<pid>.log, flushed
    only on graceful exit (src/logging.rs).

    Two things can make a naive lookup return the wrong file:

      * "Newest session-*.log" races any other FastPlay instance that exits
        while the harness is running — including the dozen-instance scenarios
        these benches exist to exercise.

      * "session-*-<pid>.log" alone is not enough either. Windows recycles
        PIDs, so if the run under test dies before flushing, an *older* run
        that happened to hold the same PID can satisfy the glob. An HDR oracle
        asserting `path=HdrPqOutput` against a stale log would pass while
        testing nothing.

    So a run is identified by PID *and* by having been written after the
    process was launched, and callers are expected to clear pre-existing
    matches immediately after launch as well. Both guards are cheap; either
    alone leaves a hole.
#>

Set-StrictMode -Version Latest

function Get-FastPlayLogDir {
    Join-Path $env:APPDATA 'FastPlay'
}

<#
.SYNOPSIS
    Delete any session log already matching a PID, immediately after launch.
.DESCRIPTION
    Anything matching at that moment is necessarily from an earlier run given
    the same PID, since the run under test has not flushed yet.
#>
function Clear-FastPlayRunLog {
    param(
        [Parameter(Mandatory)][int]$ProcessId,
        [string]$LogDir = (Get-FastPlayLogDir)
    )
    Get-ChildItem -Path $LogDir -Filter "session-*-$ProcessId.log" -ErrorAction SilentlyContinue |
        Remove-Item -Force -ErrorAction SilentlyContinue
}

<#
.SYNOPSIS
    Path of the session log for $ProcessId written after $LaunchTime.
.DESCRIPTION
    Returns $null when no qualifying log exists, unless -Required is given, in
    which case it throws. A stale log is never returned: callers asserting on
    log contents must pass -Required so a missing flush fails loudly instead of
    silently passing against an earlier run's trace.
#>
function Resolve-FastPlayRunLog {
    param(
        [Parameter(Mandatory)][int]$ProcessId,
        [Parameter(Mandatory)][datetime]$LaunchTime,
        [string]$LogDir = (Get-FastPlayLogDir),
        [switch]$Required
    )

    $match = Get-ChildItem -Path $LogDir -Filter "session-*-$ProcessId.log" -ErrorAction SilentlyContinue |
        Where-Object { $_.LastWriteTime -gt $LaunchTime } |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1

    if (-not $match) {
        if ($Required) {
            $stale = @(Get-ChildItem -Path $LogDir -Filter "session-*-$ProcessId.log" -ErrorAction SilentlyContinue)
            $note = if ($stale.Count) {
                " ($($stale.Count) log(s) match this PID but predate the launch — a recycled PID, not this run; refusing to use them)"
            }
            else { "" }
            throw "no session log for pid $ProcessId written after $($LaunchTime.ToString('o')) in $LogDir$note"
        }
        return $null
    }

    return $match.FullName
}

Export-ModuleMember -Function Get-FastPlayLogDir, Clear-FastPlayRunLog, Resolve-FastPlayRunLog
