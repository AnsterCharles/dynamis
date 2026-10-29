//! Talks to the Uno R4 WiFi's RA4M1 through the board's built-in CMSIS-DAP debug probe (the
//! ESP32-S3 USB bridge exposes one over HID). Used to back up the program on the board and
//! to verify what we write (the R4's upload bootloader can't read flash), and to restart the
//! board into its bootloader whatever program it's running.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};

const ID_INFO: u8 = 0x00;
const ID_CONNECT: u8 = 0x02;
const ID_DISCONNECT: u8 = 0x03;
const ID_TRANSFER_CONFIGURE: u8 = 0x04;
const ID_TRANSFER: u8 = 0x05;
const ID_TRANSFER_BLOCK: u8 = 0x06;
const ID_SWJ_CLOCK: u8 = 0x11;
const ID_SWJ_SEQUENCE: u8 = 0x12;
const ID_SWD_CONFIGURE: u8 = 0x13;

// DAP_Transfer request bits
const AP: u8 = 0x01;
const READ: u8 = 0x02;
// Register addresses (A[3:2] bits of the request)
const DP_ABORT_IDCODE: u8 = 0x00;
const DP_CTRL_STAT: u8 = 0x04;
const DP_SELECT: u8 = 0x08;
const AP_CSW: u8 = 0x00;
const AP_TAR: u8 = 0x04;
const AP_DRW: u8 = 0x0C;

const ACK_OK: u8 = 0x01;
const CSW_WORD: u32 = 0x2300_0012; // 32-bit accesses, auto-increment
const CSW_HALF: u32 = 0x2300_0011; // 16-bit accesses, auto-increment

// RA4M1: the Arduino bootloader stays in upload mode after a reset if this magic value is in
// the first battery-backed register (cores/arduino/boot.h, DOUBLE_TAP_MAGIC).
const PRCR: u32 = 0x4001_E3FE;
const VBTBKR0: u32 = 0x4001_E500;
const DOUBLE_TAP_MAGIC: u32 = 0x0773_8135;
const AIRCR: u32 = 0xE000_ED0C;

pub struct Probe {
    dev: HidDevice,
    packet: usize,
    queue: usize,
}

impl Probe {
    /// Opens the CMSIS-DAP interface of the USB device `vid:pid` and connects over SWD.
    pub fn open(vid: u16, pid: u16) -> Result<Self, String> {
        let api = HidApi::new().map_err(|e| e.to_string())?;
        let info = api
            .device_list()
            .filter(|d| d.vendor_id() == vid && d.product_id() == pid)
            .find(|d| d.product_string().is_some_and(|p| p.contains("CMSIS-DAP")) || d.interface_number() == 0)
            .ok_or("couldn't find the board's debug interface")?;
        let dev = info.open_device(&api).map_err(|e| format!("couldn't open the board's debug interface: {e}"))?;
        let mut probe = Probe { dev, packet: 64, queue: 1 };
        probe.flush();

        let info = probe.command(&[ID_INFO, 0xFF])?;
        if info[1] == 2 {
            probe.packet = u16::from_le_bytes([info[2], info[3]]).clamp(64, 1024) as usize;
        }
        let count = probe.command(&[ID_INFO, 0xFE])?;
        if count[1] == 1 {
            probe.queue = (count[2] as usize).clamp(1, 4);
        }
        probe.connect()?;
        Ok(probe)
    }

    /// Drops replies left over from an earlier, interrupted session.
    fn flush(&mut self) {
        let mut buf = vec![0u8; 1024];
        while matches!(self.dev.read_timeout(&mut buf, 20), Ok(n) if n > 0) {}
    }

    fn send(&mut self, cmd: &[u8]) -> Result<(), String> {
        let mut report = vec![0u8; self.packet + 1]; // leading report id 0
        report[1..=cmd.len()].copy_from_slice(cmd);
        self.dev.write(&report).map_err(|e| format!("debug probe write failed: {e}"))?;
        Ok(())
    }

    fn receive(&mut self, id: u8) -> Result<Vec<u8>, String> {
        let mut resp = vec![0u8; self.packet];
        // Skip any stray reply that doesn't belong to this command.
        for _ in 0..4 {
            let n = self.dev.read_timeout(&mut resp, 2000).map_err(|e| format!("debug probe read failed: {e}"))?;
            if n == 0 {
                break;
            }
            if resp[0] == id {
                return Ok(resp);
            }
        }
        Err("debug probe didn't answer".into())
    }

    fn command(&mut self, cmd: &[u8]) -> Result<Vec<u8>, String> {
        self.send(cmd)?;
        self.receive(cmd[0])
    }

    fn connect(&mut self) -> Result<(), String> {
        let r = self.command(&[ID_CONNECT, 1])?; // 1 = SWD
        if r[1] != 1 {
            return Err("debug probe couldn't switch to SWD".into());
        }
        self.command(&[ID_SWJ_CLOCK, 0x40, 0x42, 0x0F, 0x00])?; // 1 MHz
        self.command(&[ID_TRANSFER_CONFIGURE, 2, 0x40, 0x00, 0x00, 0x00])?;
        self.command(&[ID_SWD_CONFIGURE, 0])?;
        // Line reset, JTAG-to-SWD switch, line reset, idle.
        self.command(&[ID_SWJ_SEQUENCE, 56, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF])?;
        self.command(&[ID_SWJ_SEQUENCE, 16, 0x9E, 0xE7])?;
        self.command(&[ID_SWJ_SEQUENCE, 56, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF])?;
        self.command(&[ID_SWJ_SEQUENCE, 8, 0x00])?;

        let idcode = self.transfer(READ | DP_ABORT_IDCODE, None)?;
        if idcode == 0 || idcode == 0xFFFF_FFFF {
            return Err("no response from the board's processor over SWD".into());
        }
        self.transfer(DP_ABORT_IDCODE, Some(0x1E))?; // clear sticky errors
        self.transfer(DP_SELECT, Some(0))?;
        self.transfer(DP_CTRL_STAT, Some(0x5000_0000))?; // power up debug + system
        let start = Instant::now();
        while self.transfer(READ | DP_CTRL_STAT, None)? & 0xA000_0000 != 0xA000_0000 {
            if start.elapsed() > Duration::from_secs(1) {
                return Err("the board's debug port didn't power up".into());
            }
        }
        self.transfer(AP | AP_CSW, Some(CSW_WORD))?;
        Ok(())
    }

    fn transfer(&mut self, req: u8, data: Option<u32>) -> Result<u32, String> {
        let mut cmd = vec![ID_TRANSFER, 0, 1, req];
        if let Some(d) = data {
            cmd.extend_from_slice(&d.to_le_bytes());
        }
        let r = self.command(&cmd)?;
        if r[1] != 1 || r[2] != ACK_OK {
            return Err(format!("SWD transfer failed (ack {})", r[2]));
        }
        Ok(u32::from_le_bytes([r[3], r[4], r[5], r[6]]))
    }

    fn write32(&mut self, addr: u32, value: u32) -> Result<(), String> {
        self.transfer(AP | AP_TAR, Some(addr))?;
        self.transfer(AP | AP_DRW, Some(value)).map(|_| ())
    }

    fn write16(&mut self, addr: u32, value: u16) -> Result<(), String> {
        self.transfer(AP | AP_CSW, Some(CSW_HALF))?;
        self.transfer(AP | AP_TAR, Some(addr))?;
        // Halfword data travels on the byte lanes of its address.
        self.transfer(AP | AP_DRW, Some((value as u32) << ((addr & 2) * 8)))?;
        self.transfer(AP | AP_CSW, Some(CSW_WORD)).map(|_| ())
    }

    /// Restarts the board into its upload bootloader, whatever program it is running.
    pub fn reset_into_bootloader(&mut self) -> Result<(), String> {
        self.write16(PRCR, 0xA502)?; // unlock the battery-backed registers
        self.write32(VBTBKR0, DOUBLE_TAP_MAGIC)?;
        self.write16(PRCR, 0xA500)?;
        // SYSRESETREQ. The probe may not get an acknowledgement as the chip resets.
        let _ = self.write32(AIRCR, 0x05FA_0004);
        Ok(())
    }

    /// Stops or restarts the board's processor (for testing how the app copes with a board
    /// that goes silent).
    pub fn set_halted(&mut self, halted: bool) -> Result<(), String> {
        self.write32(0xE000_EDF0, if halted { 0xA05F_0003 } else { 0xA05F_0001 }) // DHCSR
    }

    /// Reads `len` bytes (a multiple of 4) starting at `addr` (4-byte aligned).
    /// The CPU keeps running; nothing on the board is changed.
    pub fn read(&mut self, addr: u32, len: usize, mut progress: impl FnMut(f32)) -> Result<Vec<u8>, String> {
        // Build every request up front (TAR auto-increment only works within 1 KB windows),
        // then keep several in flight at once: each USB round trip is the slow part.
        let per_packet = (self.packet - 4) / 4;
        let end = addr + len as u32;
        let mut requests: Vec<(Vec<u8>, usize)> = Vec::new(); // (command, words read)
        let mut a = addr;
        while a < end {
            let window_end = (((a / 1024) + 1) * 1024).min(end);
            let mut tar = vec![ID_TRANSFER, 0, 1, AP | AP_TAR];
            tar.extend_from_slice(&a.to_le_bytes());
            requests.push((tar, 0));
            while a < window_end {
                let words = (((window_end - a) / 4) as usize).min(per_packet);
                let mut cmd = vec![ID_TRANSFER_BLOCK, 0];
                cmd.extend_from_slice(&(words as u16).to_le_bytes());
                cmd.push(AP | READ | AP_DRW);
                requests.push((cmd, words));
                a += (words * 4) as u32;
            }
        }

        let mut out = Vec::with_capacity(len);
        let mut in_flight: VecDeque<(u8, usize)> = VecDeque::new();
        let mut next = 0;
        while next < requests.len() || !in_flight.is_empty() {
            while next < requests.len() && in_flight.len() < self.queue {
                let (cmd, words) = &requests[next];
                self.send(cmd)?;
                in_flight.push_back((cmd[0], *words));
                next += 1;
            }
            let (id, words) = in_flight.pop_front().unwrap();
            let r = self.receive(id)?;
            if id == ID_TRANSFER {
                if r[1] != 1 || r[2] != ACK_OK {
                    return Err(format!("SWD transfer failed (ack {})", r[2]));
                }
            } else {
                let got = u16::from_le_bytes([r[1], r[2]]) as usize;
                if got != words || r[3] != ACK_OK {
                    return Err(format!("SWD read failed (ack {})", r[3]));
                }
                out.extend_from_slice(&r[4..4 + words * 4]);
                progress(out.len() as f32 / len as f32);
            }
        }
        Ok(out)
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        let _ = self.command(&[ID_DISCONNECT]);
    }
}
