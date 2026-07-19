<#
.SYNOPSIS
    Verify FastPlay's HDR display-capability detection against the live
    display.

.DESCRIPTION
    Drives the env-gated capability dump (FASTPLAY_HDR_CAPS_DUMP in
    src/main.rs): FastPlay creates its ordinary window + SDR swapchain,
    queries query_hdr_presentation_capabilities against that chain's
    containing output (exactly the code path a real file open uses), prints
    the HdrPresentationCapabilities struct, and exits.

    What this instrument answers:
    - display_hdr_active must track the Windows "Use HDR" toggle
      (Settings > System > Display). Run once with HDR on (-ExpectHdr on)
      and once with HDR off (-ExpectHdr off).
    - swapchain_hdr10_color_space_supported reports whether the live
      8-bit SDR swapchain answers CheckColorSpaceSupport(G2084) with
      PRESENT support while the display is HDR-active — an open question
      the passthrough gate depends on. The script surfaces the value; it
      is informational, not asserted.

.EXAMPLE
    pwsh -File bench/verify-hdr-caps.ps1                # just dump
    pwsh -File bench/verify-hdr-caps.ps1 -ExpectHdr on  # assert active
    pwsh -File bench/verify-hdr-caps.ps1 -ExpectHdr off # assert inactive
#>
[CmdletBinding()]
param(
    # Debug binary: the release build is windows_subsystem, so its stdout
    # never reaches the console and `& $Exe` does not block.
    [string]$Exe = "$PSScriptRoot\..\target\debug\fastplay.exe",
    [ValidateSet("on", "off", "any")]
    [string]$ExpectHdr = "any"
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $Exe)) { throw "fastplay binary not found: $Exe (build first)" }

$env:FASTPLAY_HDR_CAPS_DUMP = "1"
try {
    $dump = & $Exe 2>&1 | Out-String
} finally {
    Remove-Item Env:FASTPLAY_HDR_CAPS_DUMP -ErrorAction SilentlyContinue
}
if ($LASTEXITCODE -ne 0) { throw "fastplay exited with code $LASTEXITCODE`n$dump" }

Write-Host $dump

function Get-BoolField([string]$Text, [string]$Name) {
    if ($Text -notmatch "$Name\s*:\s*(true|false)") {
        throw "capability dump is missing field '$Name'"
    }
    return $Matches[1] -eq 'true'
}

$active = Get-BoolField $dump 'display_hdr_active'
$capable = Get-BoolField $dump 'display_hdr_capable'
$swapchainHdr10 = Get-BoolField $dump 'swapchain_hdr10_color_space_supported'
$output6 = Get-BoolField $dump 'output6_available'

if (-not $output6) {
    Write-Warning "IDXGIOutput6 unavailable - display fields are the all-false default"
}

Write-Host ("display_hdr_active={0} display_hdr_capable={1} swapchain_hdr10_color_space_supported={2}" -f `
        $active, $capable, $swapchainHdr10)

switch ($ExpectHdr) {
    'on' { if (-not $active) { throw "FAIL: expected display_hdr_active=true (is the Windows HDR toggle on?)" } }
    'off' { if ($active) { throw "FAIL: expected display_hdr_active=false (is the Windows HDR toggle off?)" } }
}

Write-Host "PASS: verify-hdr-caps (ExpectHdr=$ExpectHdr)"
