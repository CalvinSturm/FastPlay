[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [string]$OutputDirectory,
    [string]$FfmpegLicensePath,
    [string]$PkgconfLicensePath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$releaseDirectory = Join-Path $repoRoot "target\release"

if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $repoRoot "target\dist"
} elseif (-not [System.IO.Path]::IsPathRooted($OutputDirectory)) {
    $OutputDirectory = Join-Path $repoRoot $OutputDirectory
}
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)

function Invoke-Cargo {
    param([Parameter(Mandatory)][string[]]$Arguments)

    & cargo @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "cargo $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
    }
}

function Get-LicenseRoots {
    $roots = [System.Collections.Generic.List[string]]::new()

    if ($env:FFMPEG_DIR) {
        $roots.Add($env:FFMPEG_DIR)
    }
    if ($env:FFMPEG_BIN_DIR) {
        $roots.Add((Split-Path -Parent $env:FFMPEG_BIN_DIR))
    }
    if ($env:VCPKG_ROOT) {
        $roots.Add((Join-Path $env:VCPKG_ROOT "installed\x64-windows"))
    }
    if ($env:USERPROFILE) {
        $roots.Add((Join-Path $env:USERPROFILE "vcpkg\installed\x64-windows"))
    }
    $roots.Add("C:\tools\vcpkg\installed\x64-windows")

    $roots |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
        ForEach-Object { [System.IO.Path]::GetFullPath($_) } |
        Select-Object -Unique
}

function Resolve-LicenseFile {
    param(
        [string]$ExplicitPath,
        [Parameter(Mandatory)][string]$RelativePath,
        [Parameter(Mandatory)][string]$DisplayName,
        [Parameter(Mandatory)][string]$OverrideParameter
    )

    if (-not [string]::IsNullOrWhiteSpace($ExplicitPath)) {
        $resolved = [System.IO.Path]::GetFullPath($ExplicitPath)
        if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
            throw "$DisplayName license file was not found at '$resolved'."
        }
        return $resolved
    }

    foreach ($root in Get-LicenseRoots) {
        $candidate = Join-Path $root $RelativePath
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return $candidate
        }
    }

    throw "Could not locate the $DisplayName license file. Pass $OverrideParameter with the license file from the runtime DLL distribution used for this build."
}

Push-Location $repoRoot
try {
    if (-not $SkipBuild) {
        Write-Host "Building FastPlay (release)..."
        Invoke-Cargo -Arguments @("build", "--release")
    }

    $metadataJson = & cargo metadata --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) {
        throw "cargo metadata failed with exit code $LASTEXITCODE"
    }
    $metadata = ($metadataJson | Out-String) | ConvertFrom-Json
    $manifestPath = [System.IO.Path]::GetFullPath((Join-Path $repoRoot "Cargo.toml"))
    $package = $metadata.packages |
        Where-Object { [System.IO.Path]::GetFullPath($_.manifest_path) -eq $manifestPath } |
        Select-Object -First 1
    if ($null -eq $package) {
        throw "Could not resolve the FastPlay package version from Cargo metadata."
    }

    # The MSI manifest is the canonical runtime-file list. Reading it here keeps
    # the installer and portable artifact in lockstep when FFmpeg DLL versions
    # change, without globbing unrelated files from target\release.
    $wixManifestPath = Join-Path $repoRoot "wix\main.wxs"
    $wixManifest = Get-Content -LiteralPath $wixManifestPath -Raw
    $runtimePattern = "Source='`\$\(var\.CargoTargetBinDir\)\\([^']+)'"
    $runtimeFiles = [regex]::Matches($wixManifest, $runtimePattern) |
        ForEach-Object { $_.Groups[1].Value } |
        Select-Object -Unique

    if ($runtimeFiles.Count -eq 0 -or $runtimeFiles -notcontains "fastplay.exe") {
        throw "No usable runtime-file manifest was found in '$wixManifestPath'."
    }

    foreach ($fileName in $runtimeFiles) {
        $sourcePath = Join-Path $releaseDirectory $fileName
        if (-not (Test-Path -LiteralPath $sourcePath -PathType Leaf)) {
            throw "Required runtime file is missing: '$sourcePath'. Build without -SkipBuild or repair the release output."
        }
    }

    $ffmpegLicenseArgs = @{
        ExplicitPath = $FfmpegLicensePath
        RelativePath = "share\ffmpeg\copyright"
        DisplayName = "FFmpeg"
        OverrideParameter = "-FfmpegLicensePath"
    }
    $ffmpegLicense = Resolve-LicenseFile @ffmpegLicenseArgs

    $pkgconfLicenseArgs = @{
        ExplicitPath = $PkgconfLicensePath
        RelativePath = "share\pkgconf\copyright"
        DisplayName = "pkgconf"
        OverrideParameter = "-PkgconfLicensePath"
    }
    $pkgconfLicense = Resolve-LicenseFile @pkgconfLicenseArgs

    New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null

    $artifactBaseName = "fastplay-$($package.version)-windows-x86_64-portable"
    $stagingDirectory = Join-Path $OutputDirectory (".portable-staging-" + [guid]::NewGuid().ToString("N"))
    $bundleDirectory = Join-Path $stagingDirectory $artifactBaseName
    $licensesDirectory = Join-Path $bundleDirectory "licenses"
    $zipPath = Join-Path $OutputDirectory "$artifactBaseName.zip"

    $outputPrefix = $OutputDirectory.TrimEnd([System.IO.Path]::DirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
    $resolvedStaging = [System.IO.Path]::GetFullPath($stagingDirectory)
    if (-not $resolvedStaging.StartsWith($outputPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to create a staging directory outside '$OutputDirectory'."
    }

    try {
        New-Item -ItemType Directory -Path $licensesDirectory -Force | Out-Null

        foreach ($fileName in $runtimeFiles) {
            Copy-Item -LiteralPath (Join-Path $releaseDirectory $fileName) -Destination $bundleDirectory
        }
        Copy-Item -LiteralPath (Join-Path $repoRoot "packaging\PORTABLE-README.txt") -Destination (Join-Path $bundleDirectory "README.txt")
        Copy-Item -LiteralPath (Join-Path $repoRoot "packaging\THIRD-PARTY-NOTICES.txt") -Destination $bundleDirectory
        Copy-Item -LiteralPath (Join-Path $repoRoot "LICENSE") -Destination (Join-Path $licensesDirectory "FastPlay-MIT.txt")
        Copy-Item -LiteralPath $ffmpegLicense -Destination (Join-Path $licensesDirectory "FFmpeg.txt")
        Copy-Item -LiteralPath $pkgconfLicense -Destination (Join-Path $licensesDirectory "pkgconf.txt")

        Compress-Archive -LiteralPath $bundleDirectory -DestinationPath $zipPath -CompressionLevel Optimal -Force
    } finally {
        if (Test-Path -LiteralPath $stagingDirectory) {
            Remove-Item -LiteralPath $stagingDirectory -Recurse -Force
        }
    }

    if (-not (Test-Path -LiteralPath $zipPath -PathType Leaf)) {
        throw "Portable archive was not created at '$zipPath'."
    }

    $hash = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash.ToLowerInvariant()
    Write-Host "Portable archive: $zipPath"
    Write-Host "SHA-256: $hash"
} finally {
    Pop-Location
}
