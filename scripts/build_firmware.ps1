# Compile the firmware for both boards into src-tauri/firmware/, which the app embeds and
# installs by itself. Only needed after editing firmware/dynamis_fsr.
#   dynamis_fsr.hex      Uno R3 (ATmega328P)
#   dynamis_fsr_r4.bin   Uno R4 WiFi (RA4M1)
#
#   powershell -ExecutionPolicy Bypass -File scripts\build_firmware.ps1
#
# Uses the arduino-cli bundled with Arduino IDE 2, with the "Arduino AVR Boards" and
# "Arduino UNO R4 Boards" cores installed.
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$cli = Get-Command arduino-cli -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source
if (-not $cli) { $cli = "$env:LOCALAPPDATA\Programs\Arduino IDE\resources\app\lib\backend\resources\arduino-cli.exe" }
if (-not (Test-Path $cli)) { throw "arduino-cli not found: install Arduino IDE 2 or arduino-cli" }

$sketch = Join-Path $root "firmware\dynamis_fsr"
$dest = Join-Path $root "src-tauri\firmware"
New-Item -ItemType Directory -Force $dest | Out-Null

foreach ($target in @(
    @{ fqbn = "arduino:avr:uno"; file = "dynamis_fsr.ino.hex"; out = "dynamis_fsr.hex" },
    @{ fqbn = "arduino:renesas_uno:unor4wifi"; file = "dynamis_fsr.ino.bin"; out = "dynamis_fsr_r4.bin" }
)) {
    $build = Join-Path $env:TEMP ("dynamis-fw-" + ($target.fqbn -replace '[:]', '-'))
    & $cli compile --fqbn $target.fqbn --output-dir $build $sketch
    if ($LASTEXITCODE) { throw "firmware compile failed for $($target.fqbn)" }
    Copy-Item (Join-Path $build $target.file) (Join-Path $dest $target.out) -Force
    Write-Host "Wrote src-tauri\firmware\$($target.out)"
}
