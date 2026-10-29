//! Finds the Uno by USB id, makes sure it runs the Dynamis firmware (installing it if not),
//! and streams readings to the window. Runs for the life of the app on its own thread.
//!
//! Installing replaces whatever program is on the board; Dynamis keeps no copy of it.
//!
//! Uno R3 (ATmega328P): write and verify through the Optiboot bootloader (stk500.rs).
//! Uno R4 WiFi (RA4M1): its bootloader can only write (samba.rs), so verification goes
//! through the board's built-in debug probe (dap.rs).

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::{dap, samba, stk500};

const FIRMWARE_R3_HEX: &str = include_str!("../firmware/dynamis_fsr.hex");
const FIRMWARE_R4_BIN: &[u8] = include_bytes!("../firmware/dynamis_fsr_r4.bin");
const FW_PREFIX: &str = "DYNAMIS FSR ";
const FW_VERSION: u32 = 2; // keep in step with FW_ID in firmware/dynamis_fsr/dynamis_fsr.ino
const BAUD: u32 = 115_200;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    R3,
    R4Wifi,
}

/// Boards we know how to flash: (vid, pid, name, kind).
const BOARDS: &[(u16, u16, &str, Kind)] = &[
    (0x2341, 0x0043, "Arduino Uno", Kind::R3),
    (0x2341, 0x0001, "Arduino Uno", Kind::R3),
    (0x2341, 0x0243, "Arduino Uno", Kind::R3),
    (0x2A03, 0x0043, "Arduino Uno", Kind::R3),
    (0x1A86, 0x7523, "Uno-compatible board (CH340)", Kind::R3),
    (0x2341, 0x1002, "Arduino Uno R4 WiFi", Kind::R4Wifi),
];
/// Boards we recognise but can't use, with the reason shown to the user.
const UNSUPPORTED: &[(u16, u16, &str)] = &[(
    0x2341,
    0x0069,
    "This is an Uno R4 Minima. Dynamis works with the Uno R3 (and R3 clones) and the Uno R4 WiFi.",
)];

#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Status {
    Searching,
    Connecting { port: String, board: String },
    Installing { port: String, board: String, step: String, progress: f32 },
    Connected { port: String, board: String },
    Error { message: String },
}

#[derive(Clone, Serialize, Debug, PartialEq)]
struct Reading {
    t: f64,
    /// "fx29": raw FX29 counts (1000 = no force, 15000 = full scale).
    /// "fsr": film sensor reading, 0 (no force) .. 1023.
    sensor: &'static str,
    value: f64,
}

/// Parses one line from the firmware: "F <counts>" from an FX29, or a bare FSR reading.
fn parse_reading(line: &str, t: f64) -> Option<Reading> {
    let line = line.trim();
    if let Some(counts) = line.strip_prefix("F ") {
        let value = counts.trim().parse::<f64>().ok()?;
        return Some(Reading { t, sensor: "fx29", value });
    }
    let value = line.parse::<f64>().ok()?;
    Some(Reading { t, sensor: "fsr", value })
}

pub struct Device {
    status: Mutex<Status>,
    retry: AtomicBool,
}

struct Board {
    port: String,
    name: String,
    kind: Kind,
    vid: u16,
    pid: u16,
}

const WRITING: &str = "Installing the Dynamis firmware";
const CHECKING: &str = "Checking the firmware";

impl Device {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { status: Mutex::new(Status::Searching), retry: AtomicBool::new(false) })
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    pub fn retry(&self) {
        self.retry.store(true, Ordering::Relaxed);
    }

    fn set(&self, app: &AppHandle, status: Status) {
        *self.status.lock().unwrap() = status.clone();
        let _ = app.emit("device-status", status);
    }

    pub fn start(self: &Arc<Self>, app: AppHandle) {
        let dev = self.clone();
        std::thread::spawn(move || dev.run(app));
    }

    fn run(&self, app: AppHandle) {
        let r3_image = stk500::parse_hex(FIRMWARE_R3_HEX).expect("embedded firmware is valid");
        loop {
            self.retry.store(false, Ordering::Relaxed);
            let Some(found) = find_board() else {
                if !matches!(self.status(), Status::Searching) {
                    self.set(&app, Status::Searching);
                }
                sleep(Duration::from_millis(700));
                continue;
            };
            let board = match found {
                Found::Board(board) => board,
                Found::Unsupported { port, message } => {
                    self.set(&app, Status::Error { message: message.into() });
                    self.wait_for_unplug_or_retry(&port);
                    continue;
                }
            };
            let image: &[u8] = match board.kind {
                Kind::R3 => &r3_image,
                Kind::R4Wifi => FIRMWARE_R4_BIN,
            };

            self.set(&app, Status::Connecting { port: board.port.clone(), board: board.name.clone() });
            let serial = match identify(&board.port) {
                Ok(Some(serial)) => serial,
                Ok(None) => {
                    if let Err(e) = self.install(&app, &board, image) {
                        self.fail(&app, &board.port, format!("Couldn't install the firmware on the {}: {e}. Unplug it and plug it back in to try again.", board.name));
                        continue;
                    }
                    sleep(Duration::from_millis(300));
                    match identify(&board.port) {
                        Ok(Some(serial)) => serial,
                        Ok(None) => {
                            self.fail(&app, &board.port, "The firmware was installed but the board isn't answering. Unplug it and plug it back in.".into());
                            continue;
                        }
                        Err(e) => {
                            self.fail(&app, &board.port, e);
                            continue;
                        }
                    }
                }
                Err(e) => {
                    self.fail(&app, &board.port, e);
                    continue;
                }
            };

            self.set(&app, Status::Connected { port: board.port.clone(), board: board.name.clone() });
            stream(&app, serial);
            // The stream ended because the board was unplugged or stalled: look again.
        }
    }

    /// Writes the Dynamis firmware (replacing whatever is on the board) and checks it.
    fn install(&self, app: &AppHandle, board: &Board, image: &[u8]) -> Result<(), String> {
        let status = |step: &str, progress: f32| Status::Installing {
            port: board.port.clone(),
            board: board.name.clone(),
            step: step.into(),
            progress,
        };
        // Throttled progress reporting for one step.
        let reporter = |step: &'static str, from: f32, share: f32| {
            let mut last = -1.0f32;
            move |p: f32| {
                if p - last >= 0.01 || p >= 1.0 {
                    last = p;
                    self.set(app, status(step, from + p * share));
                }
            }
        };
        self.set(app, status(WRITING, 0.0));

        match board.kind {
            Kind::R3 => stk500::with_bootloader(&board.port, |prog| prog.program(image, reporter(WRITING, 0.0, 1.0))),
            Kind::R4Wifi => {
                let mut boot = samba::Bootloader::enter(&board.port, board.vid, board.pid)?;
                boot.write(image, reporter(WRITING, 0.0, 0.4))?;
                // The R4's bootloader can't read back, so check over the debug probe while the
                // board is still in the bootloader.
                let len = image.len().div_ceil(4) * 4;
                let mut probe = dap::Probe::open(board.vid, board.pid)?;
                let flash = probe.read(samba::SKETCH_BASE, len, reporter(CHECKING, 0.4, 0.6))?;
                drop(probe);
                if let Some(i) = image.iter().zip(&flash).position(|(a, b)| a != b) {
                    return Err(format!("the check after writing failed at 0x{:X}", samba::SKETCH_BASE as usize + i));
                }
                boot.reset();
                Ok(())
            }
        }
    }

    fn fail(&self, app: &AppHandle, port: &str, message: String) {
        self.set(app, Status::Error { message });
        self.wait_for_unplug_or_retry(port);
    }

    /// Waits until the board is unplugged or the user clicks the retry button.
    fn wait_for_unplug_or_retry(&self, port: &str) {
        while !self.retry.load(Ordering::Relaxed) {
            let present = serialport::available_ports()
                .map(|ps| ps.iter().any(|p| p.port_name == port))
                .unwrap_or(false);
            if !present {
                return;
            }
            sleep(Duration::from_millis(500));
        }
    }
}

// ---------- serial ----------

enum Found {
    Board(Board),
    Unsupported { port: String, message: &'static str },
}

fn find_board() -> Option<Found> {
    let ports = serialport::available_ports().ok()?;
    for p in ports {
        // On macOS each device shows up twice; the /dev/cu.* node is the one to open.
        if p.port_name.starts_with("/dev/tty.") {
            continue;
        }
        let serialport::SerialPortType::UsbPort(usb) = &p.port_type else { continue };
        if let Some((vid, pid, name, kind)) = BOARDS.iter().find(|(v, d, ..)| *v == usb.vid && *d == usb.pid) {
            return Some(Found::Board(Board { port: p.port_name, name: name.to_string(), kind: *kind, vid: *vid, pid: *pid }));
        }
        if let Some((_, _, msg)) = UNSUPPORTED.iter().find(|(v, d, _)| *v == usb.vid && *d == usb.pid) {
            return Some(Found::Unsupported { port: p.port_name, message: msg });
        }
    }
    None
}

/// Opens the port, resets the board (R3) and waits for the firmware's ID line.
/// Ok(None) means the board isn't running the current Dynamis firmware.
fn identify(port_name: &str) -> Result<Option<Box<dyn serialport::SerialPort>>, String> {
    let mut port = serialport::new(port_name, BAUD)
        .timeout(Duration::from_millis(100))
        .open()
        .map_err(|e| {
            format!("Couldn't open {port_name} ({e}). If the Arduino IDE's Serial Monitor or another app is using the board, close it.")
        })?;
    stk500::reset_board(&mut *port);

    let start = Instant::now();
    let mut asked = 0;
    let mut reader = BufReader::new(port.try_clone().map_err(|e| e.to_string())?);
    let mut buf = Vec::new();
    // On an R3, Optiboot waits ~1 s after reset before starting the sketch, which prints its
    // ID on start-up. Ask as well, for boards that don't reset when the port opens (R4).
    while start.elapsed() < Duration::from_millis(3500) {
        let since = start.elapsed().as_millis();
        if (since > 300 && asked == 0) || (since > 1800 && asked == 1) || (since > 2700 && asked == 2) {
            let _ = port.write_all(b"?\n");
            asked += 1;
        }
        match reader.read_until(b'\n', &mut buf) {
            Ok(_) if buf.ends_with(b"\n") => {
                let line = String::from_utf8_lossy(&buf).trim().to_string();
                buf.clear();
                if let Some(v) = line.strip_prefix(FW_PREFIX) {
                    drop(reader);
                    return Ok((v.trim().parse::<u32>() == Ok(FW_VERSION)).then_some(port));
                }
            }
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::TimedOut => {}
            Err(e) => return Err(format!("Lost the connection to the board: {e}")),
        }
        if buf.len() > 256 {
            buf.clear();
        }
    }
    Ok(None)
}

/// Streams readings until the board is unplugged or stalls. Takes the port by value so it's
/// closed on return.
fn stream(app: &AppHandle, port: Box<dyn serialport::SerialPort>) {
    let mut reader = BufReader::new(port);
    let start = Instant::now();
    let mut last_data = Instant::now();
    let mut buf = Vec::new();
    loop {
        match reader.read_until(b'\n', &mut buf) {
            Ok(_) if buf.ends_with(b"\n") => {
                if let Some(reading) = std::str::from_utf8(&buf).ok().and_then(|s| parse_reading(s, start.elapsed().as_secs_f64())) {
                    last_data = Instant::now();
                    let _ = app.emit("reading", reading);
                }
                buf.clear();
            }
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::TimedOut => {}
            Err(_) => return,
        }
        if buf.len() > 256 {
            buf.clear();
        }
        // The firmware sends 50 lines a second; silence means the board is gone or hung.
        if last_data.elapsed() > Duration::from_secs(2) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_sensor_formats() {
        assert_eq!(parse_reading("F 1234\r\n", 1.0), Some(Reading { t: 1.0, sensor: "fx29", value: 1234.0 }));
        assert_eq!(parse_reading("69.7\r\n", 2.0), Some(Reading { t: 2.0, sensor: "fsr", value: 69.7 }));
        assert_eq!(parse_reading("DYNAMIS FSR 2", 0.0), None);
        assert_eq!(parse_reading("F x", 0.0), None);
    }
}
