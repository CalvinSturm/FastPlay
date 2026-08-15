[CmdletBinding()]
param(
    [switch]$SkipBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

Add-Type @"
using System;
using System.Runtime.InteropServices;

public static class FramelessPersistenceProbe
{
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [DllImport("user32.dll")]
    public static extern bool GetClientRect(IntPtr hwnd, out RECT rect);

    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);

    [DllImport("user32.dll")]
    public static extern bool PostMessage(IntPtr hwnd, uint message, IntPtr wparam, IntPtr lparam);
}
"@

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$executable = Join-Path $repoRoot "target\release\fastplay.exe"
$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("fastplay-frameless-" + [guid]::NewGuid().ToString("N"))
$originalAppData = $env:APPDATA
$processes = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()

function Start-ProbeInstance {
    $process = Start-Process -FilePath $executable -PassThru
    $processes.Add($process)
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        Start-Sleep -Milliseconds 50
        $process.Refresh()
    } while ($process.MainWindowHandle -eq 0 -and [DateTime]::UtcNow -lt $deadline)

    if ($process.MainWindowHandle -eq 0) {
        throw "FastPlay window did not appear."
    }
    $process
}

function Get-WindowShape {
    param([Parameter(Mandatory)][System.Diagnostics.Process]$Process)

    $client = [FramelessPersistenceProbe+RECT]::new()
    $window = [FramelessPersistenceProbe+RECT]::new()
    if (-not [FramelessPersistenceProbe]::GetClientRect($Process.MainWindowHandle, [ref]$client) -or
        -not [FramelessPersistenceProbe]::GetWindowRect($Process.MainWindowHandle, [ref]$window)) {
        throw "Could not inspect FastPlay window geometry."
    }

    [pscustomobject]@{
        ClientWidth = $client.Right - $client.Left
        ClientHeight = $client.Bottom - $client.Top
        WindowWidth = $window.Right - $window.Left
        WindowHeight = $window.Bottom - $window.Top
    }
}

function Stop-ProbeInstance {
    param([Parameter(Mandatory)][System.Diagnostics.Process]$Process)

    [void][FramelessPersistenceProbe]::PostMessage(
        $Process.MainWindowHandle,
        0x0010,
        [IntPtr]::Zero,
        [IntPtr]::Zero
    )
    if (-not $Process.WaitForExit(5000)) {
        throw "FastPlay did not close normally."
    }
}

try {
    if (-not $SkipBuild) {
        & cargo build --release
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build --release failed with exit code $LASTEXITCODE"
        }
    }
    if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
        throw "Release executable was not found at '$executable'."
    }

    New-Item -ItemType Directory -Path $testRoot | Out-Null
    $env:APPDATA = $testRoot

    $first = Start-ProbeInstance
    $firstBefore = Get-WindowShape $first
    [void][FramelessPersistenceProbe]::PostMessage(
        $first.MainWindowHandle,
        0x8002,
        [IntPtr]::Zero,
        [IntPtr]::Zero
    )
    Start-Sleep -Milliseconds 300
    $firstAfterToggle = Get-WindowShape $first
    Stop-ProbeInstance $first

    $settingsPath = Join-Path $testRoot "FastPlay\settings.txt"
    $savedAfterEnable = Get-Content -LiteralPath $settingsPath -Raw

    $second = Start-ProbeInstance
    $secondStartup = Get-WindowShape $second
    [void][FramelessPersistenceProbe]::PostMessage(
        $second.MainWindowHandle,
        0x8002,
        [IntPtr]::Zero,
        [IntPtr]::Zero
    )
    Start-Sleep -Milliseconds 300
    Stop-ProbeInstance $second

    $third = Start-ProbeInstance
    $thirdStartup = Get-WindowShape $third
    Stop-ProbeInstance $third

    if ($firstBefore.WindowWidth -le $firstBefore.ClientWidth) {
        throw "The first instance did not start framed."
    }
    if ($firstAfterToggle.WindowWidth -ne $firstAfterToggle.ClientWidth) {
        throw "The first instance did not become frameless."
    }
    if ($secondStartup.WindowWidth -ne $secondStartup.ClientWidth) {
        throw "The second instance did not inherit frameless mode."
    }
    if ($thirdStartup.WindowWidth -le $thirdStartup.ClientWidth) {
        throw "The third instance did not inherit the disabled preference."
    }
    if ($savedAfterEnable -notmatch "(?m)^frameless_windowed=true$") {
        throw "The enabled preference was not persisted."
    }

    [pscustomobject]@{
        FirstBefore = $firstBefore
        FirstAfterToggle = $firstAfterToggle
        SecondStartup = $secondStartup
        ThirdStartupAfterDisable = $thirdStartup
        SavedAfterEnable = $savedAfterEnable.Trim()
    } | Format-List
} finally {
    $env:APPDATA = $originalAppData
    foreach ($process in $processes) {
        if (-not $process.HasExited) {
            [void][FramelessPersistenceProbe]::PostMessage(
                $process.MainWindowHandle,
                0x0010,
                [IntPtr]::Zero,
                [IntPtr]::Zero
            )
            if (-not $process.WaitForExit(2000)) {
                Stop-Process -Id $process.Id -Force
            }
        }
    }

    $resolvedTestRoot = [System.IO.Path]::GetFullPath($testRoot)
    $tempPrefix = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
    if ($resolvedTestRoot.StartsWith($tempPrefix, [System.StringComparison]::OrdinalIgnoreCase) -and
        (Split-Path -Leaf $resolvedTestRoot).StartsWith("fastplay-frameless-")) {
        Remove-Item -LiteralPath $resolvedTestRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
