# Build the Windows installer and the portable (USB) copy of Dynamis.
#
#   powershell -ExecutionPolicy Bypass -File scripts\build_windows.ps1
#
# Needs: Node and Rust (stable, MSVC). Produces, in dist\:
#   Dynamis_<version>_x64-setup.exe     installer: per-user, no admin rights, Start Menu + desktop icon
#   Dynamis-<version>-portable-win.exe  single file: double-click to run, nothing to install
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$version = (Get-Content (Join-Path $root "src-tauri\tauri.conf.json") -Raw | ConvertFrom-Json).version

Push-Location $root
if (-not (Test-Path "node_modules")) { npm install }
npx tauri build --bundles nsis
$code = $LASTEXITCODE
Pop-Location
if ($code) { throw "tauri build failed" }

$dist = Join-Path $root "dist"
New-Item -ItemType Directory -Force $dist | Out-Null
$release = Join-Path $root "src-tauri\target\release"
$setup = Get-ChildItem (Join-Path $release "bundle\nsis") -Filter "*-setup.exe" | Sort-Object LastWriteTime | Select-Object -Last 1
Copy-Item $setup.FullName $dist -Force
Copy-Item (Join-Path $release "dynamis.exe") (Join-Path $dist "Dynamis-$version-portable-win.exe") -Force
Write-Host "Done:"
Get-ChildItem $dist | Format-Table Name, @{n="MB"; e={[math]::Round($_.Length / 1MB, 1)}}
