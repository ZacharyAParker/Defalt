<#
.SYNOPSIS
    Capture the console's main views to PNGs for review.

.DESCRIPTION
    Runs the built binary once per view with the DEFALT_SHOT* flags set, waits
    for each capture, and prints the paths. Exits non-zero if any view fails.
    Captures the release build unless -Configuration debug is given; -Build
    builds that configuration first.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\verify-ui.ps1 -Build
#>
[CmdletBinding()]
param(
    # Which build to capture. Named so it doesn't shadow PowerShell's $PROFILE.
    [ValidateSet('release', 'debug')]
    [string] $Configuration = 'release',
    # Build that configuration before capturing.
    [switch] $Build,
    # Kept so older command lines still work; release is the default now.
    [switch] $Release
)
$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
$buildProfile = if ($Release) { 'release' } else { $Configuration }
$binary = Join-Path $project "target\$buildProfile\defalt.exe"
$flags = @('DEFALT_SHOT', 'DEFALT_SHOT_COMPACT', 'DEFALT_SHOT_EMPTY', 'DEFALT_SHOT_RADIO', 'DEFALT_SHOT_RACKS')
$previous = @{}
$failed = $false
foreach ($flag in $flags) { $previous[$flag] = [Environment]::GetEnvironmentVariable($flag) }
try {
    if ($Build) {
        $cargoArgs = @('build')
        if ($buildProfile -eq 'release') { $cargoArgs += '--release' }
        Push-Location $project
        try {
            & cargo @cargoArgs
            if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
        } finally {
            Pop-Location
        }
    }
    if (-not (Test-Path -LiteralPath $binary)) { throw "No $buildProfile binary at $binary (pass -Build to build it)" }
    foreach ($view in @('console', 'compact', 'empty', 'radio')) {
        foreach ($flag in $flags) { [Environment]::SetEnvironmentVariable($flag, $null) }
        $capture = Join-Path $project "target\review-$view.png"
        $startedAt = Get-Date
        $env:DEFALT_SHOT = $capture
        if ($view -eq 'compact') { $env:DEFALT_SHOT_COMPACT = '1'; $env:DEFALT_SHOT_RACKS = '1' }
        if ($view -eq 'empty') { $env:DEFALT_SHOT_EMPTY = '1' }
        if ($view -eq 'radio') { $env:DEFALT_SHOT_RADIO = '1' }
        $process = Start-Process -FilePath $binary -WorkingDirectory $project -WindowStyle Hidden -PassThru
        if (-not $process.WaitForExit(45000)) {
            $process.Kill()
            throw "Capture timed out for $view"
        }
        if (-not (Test-Path -LiteralPath $capture)) { throw "No capture for $view" }
        if ((Get-Item -LiteralPath $capture).LastWriteTime -lt $startedAt) { throw "Stale capture for $view" }
        Write-Output $capture
    }
} catch {
    Write-Host "verify-ui: $($_.Exception.Message)" -ForegroundColor Red
    $failed = $true
} finally {
    foreach ($flag in $flags) { [Environment]::SetEnvironmentVariable($flag, $previous[$flag]) }
}
if ($failed) { exit 1 }
exit 0
