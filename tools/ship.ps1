<#
.SYNOPSIS
    Build the release binary and point the desktop shortcut at it.

.DESCRIPTION
    The shortcut used to point at target\debug\defalt.exe, which meant a
    release-only rebuild left the desktop launching a binary from before the
    fix -- and a debug-only rebuild did the same in the other direction. The
    shortcut points at release now, so this is the one command that makes the
    desktop current: build it, then make sure the shortcut agrees.

    The Desktop folder is asked for rather than assumed. It is redirected into
    OneDrive on this machine, so $env:USERPROFILE\Desktop is an empty path that
    exists on some machines and not others.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\ship.ps1
#>
[CmdletBinding()]
param(
    # Skip the build and only repoint the shortcut.
    [switch] $NoBuild,
    # Parallel build jobs. Capped by default because this machine is used
    # while it builds.
    [int] $Jobs = 8
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root 'target\release\defalt.exe'

if (-not $NoBuild) {
    Write-Host "Building release..." -ForegroundColor Cyan
    Push-Location $root
    try {
        & cargo build --release -j $Jobs
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
    } finally {
        Pop-Location
    }
}

if (-not (Test-Path $exe)) { throw "no release binary at $exe" }

$desktop = [Environment]::GetFolderPath('Desktop')
if (-not $desktop) { throw 'could not find the Desktop folder' }
$link = Join-Path $desktop 'Defalt.lnk'

$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($link)
$was = $shortcut.TargetPath

$shortcut.TargetPath = $exe
$shortcut.WorkingDirectory = $root
$shortcutIcon = Join-Path $root 'icons\shortcut.ico'
$shortcut.IconLocation = if (Test-Path -LiteralPath $shortcutIcon) { "$shortcutIcon,0" } else { "$exe,0" }
$shortcut.Description = 'Defalt'
$shortcut.Save()

$built = (Get-Item $exe).LastWriteTime
if ($was -eq $exe) {
    Write-Host "Shortcut already pointed at the release build." -ForegroundColor DarkGray
} elseif ($was) {
    Write-Host "Shortcut repointed:" -ForegroundColor Green
    Write-Host "  was: $was"
} else {
    Write-Host "Shortcut created." -ForegroundColor Green
}
Write-Host "  now: $exe"
Write-Host "  built: $built"
Write-Host "  link:  $link"
