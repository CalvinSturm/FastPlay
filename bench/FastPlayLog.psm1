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
    process was launched. The launch-time filter closes the recycled-PID hole
    on its own: an older run's log necessarily predates the launch.

    There used to be a companion Clear-FastPlayRunLog that deleted matching
    logs immediately after launch. It was removed because it could only
    subtract. The PID is not knowable until the process exists, so the delete
    inevitably raced the run it had just started: a player that panicked during
    open flushed its trace (the panic hook dumps the ring) well before a
    PowerShell Get-ChildItem | Remove-Item pipeline reached disk, and the clear
    then destroyed the very evidence these per-run logs exist to preserve.
#>

Set-StrictMode -Version Latest

function Get-FastPlayLogDir {
    Join-Path $env:APPDATA 'FastPlay'
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

Export-ModuleMember -Function Get-FastPlayLogDir, Resolve-FastPlayRunLog
