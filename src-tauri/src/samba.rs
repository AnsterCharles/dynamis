//! Writes programs to the Uno R4 WiFi through its upload bootloader ("Arduino Bootloader
//! (SAM-BA extended) 2.0"), which the ESP32-S3 USB bridge starts after a 1200-baud touch.
//! The bootloader only writes; reading back is done over the debug probe (see dap.rs).
//!
//! Protocol (arduino-renesas-bootloader, bossa.c): 230400 baud, commands end in '#'.
//!   S<buf off>,<len>#  then <len> raw bytes      -> loads the RAM buffer (no reply)
//!   Y<buf off>,0#                                 -> selects the source offset ("Y\n\r")
//!   Y<flash off>,<len>#                           -> erases (if block-aligned) + writes ("Y\n\r")
//!   K#                                            -> resets into the program
//! Flash offsets are relative to the start of the sketch area (0x4000).

use std::io::{Read, Write};
use std::thread::sleep;
use std::time::{Duration, Instant};

pub const SKETCH_BASE: u32 = 0x4000;
pub const MAX_SKETCH_SIZE: usize = 0x40000 - 0x4000; // 256 KB flash minus the 16 KB bootloader
const CHUNK: usize = 4096; // the bootloader erases 4 KB at each 4 KB-aligned write
const BAUD: u32 = 230_400;

/// The 1200-baud "touch" that makes the USB bridge restart the RA4M1 in its bootloader.
pub fn touch(port_name: &str) -> Result<(), String> {
    let mut port = serialport::new(port_name, 1200)
        .timeout(Duration::from_millis(100))
        .open()
        .map_err(|e| format!("couldn't open {port_name}: {e}"))?;
    let _ = port.write_data_terminal_ready(true);
    sleep(Duration::from_millis(50));
    let _ = port.write_data_terminal_ready(false);
    drop(port);
    Ok(())
}

pub struct Bootloader {
    port: Box<dyn serialport::SerialPort>,
}

impl Bootloader {
    /// Restarts the board into its bootloader and connects. Uses the debug probe (works
    /// whatever program is running); falls back to the 1200-baud touch.
    pub fn enter(port_name: &str, vid: u16, pid: u16) -> Result<Self, String> {
        let via_probe = crate::dap::Probe::open(vid, pid).and_then(|mut p| p.reset_into_bootloader());
        if via_probe.is_err() {
            touch(port_name)?;
        }
        let start = Instant::now();
        let mut last_err = String::from("no answer");
        while start.elapsed() < Duration::from_secs(6) {
            sleep(Duration::from_millis(400));
            match Self::connect(port_name) {
                Ok(b) => return Ok(b),
                Err(e) => last_err = e,
            }
        }
        Err(format!("the board's upload bootloader didn't respond ({last_err})"))
    }

    fn connect(port_name: &str) -> Result<Self, String> {
        let mut port = serialport::new(port_name, BAUD)
            .timeout(Duration::from_millis(300))
            .open()
            .map_err(|e| e.to_string())?;
        let _ = port.write_data_terminal_ready(true);
        sleep(Duration::from_millis(50));
        let _ = port.clear(serialport::ClearBuffer::All);
        let mut b = Bootloader { port };
        b.send(b"V#")?;
        // Tolerate stray bytes still in flight from the program that was running.
        let mut seen = Vec::new();
        let mut byte = [0u8; 1];
        while seen.len() < 512 {
            match b.port.read(&mut byte) {
                Ok(1) => seen.push(byte[0]),
                _ => break,
            }
            if seen.ends_with(b"\n\r") && String::from_utf8_lossy(&seen).contains("Arduino Bootloader") {
                b.send(b"N#")?;
                b.expect(b"\n\r")?;
                return Ok(b);
            }
        }
        Err("not in the bootloader".into())
    }

    fn send(&mut self, data: &[u8]) -> Result<(), String> {
        self.port.write_all(data).map_err(|e| e.to_string())?;
        self.port.flush().map_err(|e| e.to_string())
    }

    fn expect(&mut self, want: &[u8]) -> Result<(), String> {
        let mut got = vec![0u8; want.len()];
        self.port.read_exact(&mut got).map_err(|e| format!("no reply from bootloader: {e}"))?;
        if got != want {
            return Err(format!("unexpected reply from bootloader: {:?}", String::from_utf8_lossy(&got)));
        }
        Ok(())
    }

    /// Writes `image` (which starts at the sketch base, 0x4000). The tail of each 4 KB block is
    /// padded with 0xFF. `progress` gets 0.0..=1.0.
    pub fn write(&mut self, image: &[u8], mut progress: impl FnMut(f32)) -> Result<(), String> {
        if image.len() > MAX_SKETCH_SIZE {
            return Err("program is too large for an Uno R4".into());
        }
        let chunks = image.len().div_ceil(CHUNK);
        for (i, chunk) in image.chunks(CHUNK).enumerate() {
            let mut data = chunk.to_vec();
            data.resize(CHUNK, 0xFF);
            self.send(format!("S{:08X},{:08X}#", 0, CHUNK).as_bytes())?;
            // Give the bootloader a moment to switch from command to data mode.
            sleep(Duration::from_millis(5));
            self.send(&data)?;
            self.send(format!("Y{:08X},{:08X}#", 0, 0).as_bytes())?;
            self.expect(b"Y\n\r")?;
            self.send(format!("Y{:08X},{:08X}#", i * CHUNK, CHUNK).as_bytes())?;
            self.expect(b"Y\n\r")?;
            progress((i + 1) as f32 / chunks as f32);
        }
        Ok(())
    }

    /// Restarts the board into the newly written program.
    pub fn reset(mut self) {
        let _ = self.send(b"K#");
    }
}
