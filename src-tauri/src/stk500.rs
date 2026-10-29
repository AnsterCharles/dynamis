//! Minimal STK500v1 programmer for the Arduino Uno's Optiboot bootloader (what avrdude's
//! "arduino" programmer does), so the app can install its firmware without any external tools.

use std::io::{ErrorKind, Read, Write};
use std::thread::sleep;
use std::time::Duration;

const STK_OK: u8 = 0x10;
const STK_INSYNC: u8 = 0x14;
const CRC_EOP: u8 = 0x20;

const PAGE_SIZE: usize = 128; // ATmega328P flash page, bytes
const MAX_APP_SIZE: usize = 32256; // 32 KB minus the 512-byte Optiboot section

/// Parses Intel HEX into a flash image starting at address 0 (gaps filled with 0xFF).
pub fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let mut image: Vec<u8> = Vec::new();
    let mut base: usize = 0;
    for (n, line) in text.lines().enumerate().map(|(i, l)| (i + 1, l.trim())) {
        if line.is_empty() {
            continue;
        }
        let hex = line.strip_prefix(':').ok_or(format!("hex line {n}: missing ':'"))?;
        let bytes = (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16))
            .collect::<Result<Vec<u8>, _>>()
            .map_err(|_| format!("hex line {n}: bad digit"))?;
        if bytes.len() < 5 || bytes.len() != bytes[0] as usize + 5 {
            return Err(format!("hex line {n}: bad length"));
        }
        if bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b)) != 0 {
            return Err(format!("hex line {n}: bad checksum"));
        }
        let len = bytes[0] as usize;
        let addr = ((bytes[1] as usize) << 8) | bytes[2] as usize;
        let data = &bytes[4..4 + len];
        match bytes[3] {
            0x00 => {
                let start = base + addr;
                if image.len() < start + len {
                    image.resize(start + len, 0xFF);
                }
                image[start..start + len].copy_from_slice(data);
            }
            0x01 => break,
            0x02 => base = (((data[0] as usize) << 8) | data[1] as usize) << 4,
            0x04 => base = (((data[0] as usize) << 8) | data[1] as usize) << 16,
            _ => {} // start address records: not needed
        }
    }
    if image.is_empty() {
        return Err("firmware hex is empty".into());
    }
    if image.len() > MAX_APP_SIZE {
        return Err("firmware is too large for an Uno".into());
    }
    Ok(image)
}

/// Talks STK500v1 over any byte stream whose reads fail with `TimedOut` when no data arrives.
pub struct Programmer<'a, T: Read + Write + ?Sized> {
    io: &'a mut T,
}

impl<'a, T: Read + Write + ?Sized> Programmer<'a, T> {
    pub fn new(io: &'a mut T) -> Self {
        Self { io }
    }

    /// Reads until the line goes quiet; returns everything received.
    fn drain(&mut self) -> Vec<u8> {
        let mut got = Vec::new();
        let mut buf = [0u8; 64];
        loop {
            match self.io.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
        got
    }

    /// Gets in sync with the bootloader, which only listens for about a second after reset.
    pub fn sync(&mut self) -> Result<(), String> {
        for _ in 0..8 {
            if self.io.write_all(&[0x30, CRC_EOP]).is_err() {
                continue;
            }
            let _ = self.io.flush();
            let got = self.drain();
            if got.windows(2).any(|w| w == [STK_INSYNC, STK_OK]) {
                // Late replies to earlier attempts have been drained; confirm with a clean exchange.
                return self.command(&[0x30, CRC_EOP], 0).map(|_| ());
            }
        }
        Err("the Arduino's bootloader didn't respond".into())
    }

    fn command(&mut self, cmd: &[u8], reply_len: usize) -> Result<Vec<u8>, String> {
        self.io.write_all(cmd).map_err(io_err)?;
        self.io.flush().map_err(io_err)?;
        let mut reply = vec![0u8; reply_len + 2];
        self.io.read_exact(&mut reply).map_err(io_err)?;
        if reply[0] != STK_INSYNC || reply[reply_len + 1] != STK_OK {
            return Err(format!("unexpected reply from bootloader (command 0x{:02X})", cmd[0]));
        }
        Ok(reply[1..=reply_len].to_vec())
    }

    fn load_address(&mut self, byte_addr: usize) -> Result<(), String> {
        let word = byte_addr / 2;
        self.command(&[0x55, (word & 0xFF) as u8, (word >> 8) as u8, CRC_EOP], 0).map(|_| ())
    }

    /// Checks the chip is an ATmega328P and enters programming mode.
    fn begin(&mut self) -> Result<(), String> {
        let sig = self.command(&[0x75, CRC_EOP], 3)?;
        if sig[0] != 0x1E || sig[1] != 0x95 {
            return Err(format!(
                "this board's chip ({:02X} {:02X} {:02X}) isn't an ATmega328P (Uno)",
                sig[0], sig[1], sig[2]
            ));
        }
        self.command(&[0x50, CRC_EOP], 0).map(|_| ())
    }

    /// Leaves programming mode, which makes Optiboot start the program.
    fn finish(&mut self) -> Result<(), String> {
        self.command(&[0x51, CRC_EOP], 0).map(|_| ())
    }

    /// Writes, verifies and starts `image`. `progress` gets 0.0..=1.0.
    pub fn program(&mut self, image: &[u8], progress: impl FnMut(f32)) -> Result<(), String> {
        self.begin()?;
        self.write_and_verify(image, progress)?;
        self.finish()
    }

    /// Writes and verifies `image` (call `begin` first). `progress` gets 0.0..=1.0.
    fn write_and_verify(&mut self, image: &[u8], mut progress: impl FnMut(f32)) -> Result<(), String> {
        if image.len() > MAX_APP_SIZE {
            return Err("program is too large for an Uno".into());
        }
        let pages: Vec<&[u8]> = image.chunks(PAGE_SIZE).collect();
        let steps = (pages.len() * 2) as f32;
        for (i, page) in pages.iter().enumerate() {
            let mut page = page.to_vec();
            page.resize(PAGE_SIZE, 0xFF);
            self.load_address(i * PAGE_SIZE)?;
            let mut cmd = vec![0x64, 0x00, PAGE_SIZE as u8, b'F'];
            cmd.extend_from_slice(&page);
            cmd.push(CRC_EOP);
            self.command(&cmd, 0)?;
            progress((i + 1) as f32 / steps);
        }
        for (i, page) in pages.iter().enumerate() {
            self.load_address(i * PAGE_SIZE)?;
            let read = self.command(&[0x74, 0x00, PAGE_SIZE as u8, b'F', CRC_EOP], PAGE_SIZE)?;
            if &read[..page.len()] != *page {
                return Err(format!("verify failed at 0x{:04X}", i * PAGE_SIZE));
            }
            progress((pages.len() + i + 1) as f32 / steps);
        }
        Ok(())
    }
}

fn io_err(e: std::io::Error) -> String {
    if e.kind() == ErrorKind::TimedOut {
        "the Arduino stopped responding".into()
    } else {
        e.to_string()
    }
}

/// Pulses DTR, which resets the Uno through its auto-reset capacitor.
pub fn reset_board(port: &mut dyn serialport::SerialPort) {
    let _ = port.write_data_terminal_ready(false);
    let _ = port.write_request_to_send(false);
    sleep(Duration::from_millis(250));
    let _ = port.write_data_terminal_ready(true);
    let _ = port.write_request_to_send(true);
    sleep(Duration::from_millis(50));
    let _ = port.clear(serialport::ClearBuffer::All);
}

/// Resets the board into its bootloader and runs `f` with a synced programmer. Genuine Unos
/// run Optiboot at 115200 baud; some clones carry the older 57600-baud bootloader.
pub fn with_bootloader<R>(
    port_name: &str,
    f: impl FnOnce(&mut Programmer<dyn serialport::SerialPort>) -> Result<R, String>,
) -> Result<R, String> {
    let mut last_err = String::new();
    for baud in [115_200, 57_600] {
        let mut port = serialport::new(port_name, baud)
            .timeout(Duration::from_millis(150))
            .open()
            .map_err(|e| format!("couldn't open {port_name}: {e}"))?;
        reset_board(&mut *port);
        let mut prog = Programmer::new(&mut *port);
        match prog.sync() {
            Ok(()) => return f(&mut prog),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io;

    /// Simulated Optiboot: parses STK500 commands, keeps a flash array, times out when idle.
    struct FakeOptiboot {
        pending: Vec<u8>,
        out: VecDeque<u8>,
        flash: Vec<u8>,
        addr: usize,
        started_app: bool,
    }

    impl FakeOptiboot {
        fn new() -> Self {
            // Junk on the line from the old sketch before the bootloader takes over.
            Self { pending: vec![], out: VecDeque::from(vec![0x31, 0x32, 0x0A]), flash: vec![0xFF; 32768], addr: 0, started_app: false }
        }
        fn process(&mut self) {
            loop {
                let need = match self.pending.first() {
                    None => return,
                    Some(0x30 | 0x50 | 0x51 | 0x75) => 2,
                    Some(0x55) => 4,
                    Some(0x74) => 5,
                    Some(0x64) if self.pending.len() >= 3 => 5 + self.pending[2] as usize,
                    Some(0x64) => return,
                    Some(_) => {
                        self.pending.remove(0);
                        continue;
                    }
                };
                if self.pending.len() < need {
                    return;
                }
                let cmd: Vec<u8> = self.pending.drain(..need).collect();
                assert_eq!(*cmd.last().unwrap(), CRC_EOP);
                self.out.push_back(STK_INSYNC);
                match cmd[0] {
                    0x75 => self.out.extend([0x1E, 0x95, 0x0F]),
                    0x55 => self.addr = ((cmd[1] as usize) | ((cmd[2] as usize) << 8)) * 2,
                    0x64 => {
                        let len = cmd[2] as usize;
                        self.flash[self.addr..self.addr + len].copy_from_slice(&cmd[4..4 + len]);
                    }
                    0x74 => {
                        let len = cmd[2] as usize;
                        let data = self.flash[self.addr..self.addr + len].to_vec();
                        self.out.extend(data);
                    }
                    0x51 => self.started_app = true,
                    _ => {}
                }
                self.out.push_back(STK_OK);
            }
        }
    }

    impl Read for FakeOptiboot {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.out.is_empty() {
                return Err(io::Error::new(ErrorKind::TimedOut, "timeout"));
            }
            let n = buf.len().min(self.out.len());
            for b in buf.iter_mut().take(n) {
                *b = self.out.pop_front().unwrap();
            }
            Ok(n)
        }
    }

    impl Write for FakeOptiboot {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.pending.extend_from_slice(buf);
            self.process();
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn embedded_firmware_parses() {
        let image = parse_hex(include_str!("../firmware/dynamis_fsr.hex")).unwrap();
        assert!(image.len() > 1000 && image.len() < MAX_APP_SIZE);
        assert_eq!(&image[..2], &[0x0C, 0x94]); // reset vector is a JMP
    }

    #[test]
    fn rejects_corrupt_hex() {
        assert!(parse_hex(":100000000C9461000C9489000C9489000C94890075").is_err());
    }

    #[test]
    fn flashes_and_verifies_against_fake_bootloader() {
        let image = parse_hex(include_str!("../firmware/dynamis_fsr.hex")).unwrap();
        let mut boot = FakeOptiboot::new();
        let mut last = 0.0;
        Programmer::new(&mut boot).sync().unwrap();
        Programmer::new(&mut boot).program(&image, |p| last = p).unwrap();
        assert_eq!(&boot.flash[..image.len()], &image[..]);
        assert!(boot.started_app);
        assert_eq!(last, 1.0);
    }
}
