[CmdletBinding()]
param([switch] $Release)
$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
$profile = if ($Release) { 'release' } else { 'debug' }
$binary = Join-Path $project "target\$profile\defalt.exe"
$flags = @('DEFALT_SHOT', 'DEFALT_SHOT_COMPACT', 'DEFALT_SHOT_EMPTY', 'DEFALT_SHOT_RADIO', 'DEFALT_SHOT_RACKS')
$previous = @{}
foreach ($flag in $flags) { $previous[$flag] = [Environment]::GetEnvironmentVariable($flag) }
try {
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
} finally {
    foreach ($flag in $flags) { [Environment]::SetEnvironmentVariable($flag, $previous[$flag]) }
}
