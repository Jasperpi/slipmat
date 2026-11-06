# Build Slipmat on Windows and add Start menu and desktop shortcuts.
# Run from the slipmat folder:  powershell -ExecutionPolicy Bypass -File scripts\windows-install.ps1
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host 'Rust is not installed. Get it from https://rustup.rs (take the default MSVC choice;'
    Write-Host 'it will offer to install the Visual Studio C++ build tools it needs). Then run this again.'
    exit 1
}

cargo build --release
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# DirectML.dll (for running stems on the graphics card) is built next to the exe,
# so the shortcuts point at the build folder rather than a copy elsewhere.
$exe = Join-Path (Get-Location) 'target\release\slipmat.exe'
$shell = New-Object -ComObject WScript.Shell
foreach ($dir in @([Environment]::GetFolderPath('Programs'), [Environment]::GetFolderPath('Desktop'))) {
    $link = $shell.CreateShortcut((Join-Path $dir 'Slipmat.lnk'))
    $link.TargetPath = $exe
    $link.WorkingDirectory = Split-Path $exe
    $link.Description = 'DJ with Spotify on your Pioneer controller'
    $link.Save()
}
Write-Host ''
Write-Host 'Slipmat is built. Start it from the Start menu or the desktop shortcut.'
Write-Host 'Closing its console window quits it.'
