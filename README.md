# Dynamis

Force meter for an Arduino Uno with a TE FX29 I2C load cell.
Plug in the Uno, open Dynamis, and it finds the board, installs its own firmware the first
time, and shows live force, peak, and a graph. No Arduino IDE needed.

```
FX29 load cell (FX29K0-100A-0010-L: I2C, 10 lbf, address 0x28) -> R4 WiFi Qwiic port
  red    -> red    (3.3 V)
  black  -> black  (GND)
  white  -> blue   (SDA)
  yellow -> yellow (SCL)
(On an Uno R3, or the R4's header: red 5V, black GND, white SDA, yellow SCL.)
```

The firmware finds the FX29 by itself (Qwiic port first, then SDA/SCL) and keeps looking while
it's missing, so it can be plugged in while running. The FX29 is factory-calibrated (1000 counts = no
force, 15000 = full scale): press **Zero** once with nothing on it (remembered), and optionally
**Check** it with a known weight. The **Average (2 s)** readout gives a steady number.

## Install

- **Windows:** run `dist/Dynamis_<version>_x64-setup.exe`. It installs for the current user only
  (no admin rights), with Start Menu and desktop icons. Or use
  `dist/Dynamis-<version>-portable-win.exe`: a single file you double-click, nothing installed.
  The app is unsigned: if SmartScreen warns, choose *More info → Run anyway* (once).
- **Mac:** open `dist/mac/Dynamis_<version>_x64.dmg` (Intel) or `_aarch64.dmg` (M1 and later)
  and drag Dynamis to Applications. The .dmg files are built on GitHub's Macs by
  `.github/workflows/build.yml` when a version tag is pushed (`git tag v0.5.0 && git push --tags`);
  download them from the run's artifacts. Or build on the Mac with `bash scripts/build_mac.sh`. Unsigned: the first time, System Settings →
  Privacy & Security → *Open Anyway*.

## Use

1. Wire the FX29 to the Qwiic port, plug the Uno in, open Dynamis.
2. Dynamis installs its firmware on the Uno (about 10 s). **This replaces the program on the
   board, and Dynamis keeps no copy of it.** When you're done, upload your own program again from
   the Arduino IDE. Close Dynamis before plugging in a board you want to leave alone.
3. Press **Zero** with nothing on the button (remembered). Optionally put a known weight on it
   and click **Check** to correct the factory calibration.
4. Press on the center of the button, straight down. **Peak** holds the highest value.
   **Start recording** saves a CSV to `Documents/Dynamis/`.

If the board drops out, Dynamis keeps the readings up and reconnects by itself; connection
events go to `dynamis.log` in the app's log folder.

Supported boards: Arduino Uno R3 and Uno-compatible clones (genuine 16U2 or CH340 USB chip),
and the Arduino Uno R4 WiFi. The Uno R4 Minima is not supported.

On the R4 WiFi, every write is checked through the board's built-in debug probe, since its
upload bootloader can't read back. If the app says the port is busy, close the Arduino IDE's
Serial Monitor (or any other app using the board).

## How it works

| Path | What |
|---|---|
| `firmware/dynamis_fsr` | Uno sketch: reads the FX29, sends 50 readings/s at 115200 baud, answers `?` with its ID |
| `src-tauri/firmware/dynamis_fsr.hex` | Compiled sketch, embedded in the app |
| `src-tauri/src/device.rs` | Finds the Uno by USB id, checks the firmware ID/version, flashes if needed, streams readings |
| `src-tauri/src/stk500.rs` | Built-in Uno flasher (the STK500/Optiboot protocol avrdude uses), with tests |
| `src-tauri/src/samba.rs` | Uno R4 WiFi writer (its "SAM-BA extended" bootloader, 230400 baud) |
| `src-tauri/src/dap.rs` | Uno R4 WiFi: verifies writes over its CMSIS-DAP debug probe, and restarts it into the bootloader |
| `src-tauri/examples/r4tool.rs` | Command-line tool for testing the R4 paths on a real board |
| `src/` | The window: readout, chart, zero/check, recording |
| `design/icon.svg` | App icon (regenerate sizes with `npx tauri icon design/icon.svg`) |

Changing the firmware: edit the sketch, bump `FW_VERSION` in both the sketch and `device.rs`,
run `scripts/build_firmware.ps1`, rebuild. Boards with the old version are updated automatically.

Build: `powershell -ExecutionPolicy Bypass -File scripts\build_windows.ps1` (Windows) or
`bash scripts/build_mac.sh` (Mac). Develop: `npm run tauri dev`. Tests: `cargo test` in `src-tauri`.
