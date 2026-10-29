//! Developer tool for testing the Uno R4 WiFi code paths against a real board.
//!   cargo run --example r4tool -- read  <out.bin>           back up the sketch area over SWD
//!   cargo run --example r4tool -- boot  <COMx>              enter the bootloader, then reset back
//!   cargo run --example r4tool -- write <COMx> <in.bin>     write through the bootloader, then reset
//!   cargo run --example r4tool -- verify <in.bin>           compare the board's flash with a file

use std::time::Instant;

use dynamis_lib::{dap::Probe, samba};

const VID: u16 = 0x2341;
const PID: u16 = 0x1002;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let t0 = Instant::now();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["read", out] => {
            let mut probe = Probe::open(VID, PID)?;
            let data = probe.read(samba::SKETCH_BASE, samba::MAX_SKETCH_SIZE, |_| {})?;
            std::fs::write(out, &data).map_err(|e| e.to_string())?;
            println!("read {} bytes in {:.1} s", data.len(), t0.elapsed().as_secs_f32());
        }
        ["boot", port] => {
            let boot = samba::Bootloader::enter(port, VID, PID)?;
            println!("bootloader ready after {:.1} s; resetting back to the program", t0.elapsed().as_secs_f32());
            boot.reset();
        }
        ["write", port, input] => {
            let image = std::fs::read(input).map_err(|e| e.to_string())?;
            let mut boot = samba::Bootloader::enter(port, VID, PID)?;
            println!("bootloader ready after {:.1} s", t0.elapsed().as_secs_f32());
            boot.write(&image, |_| {})?;
            boot.reset();
            println!("wrote {} bytes in {:.1} s", image.len(), t0.elapsed().as_secs_f32());
        }
        ["verify", input] => {
            let image = std::fs::read(input).map_err(|e| e.to_string())?;
            let len = image.len().div_ceil(4) * 4;
            let mut probe = Probe::open(VID, PID)?;
            let flash = probe.read(samba::SKETCH_BASE, len, |_| {})?;
            match image.iter().zip(&flash).position(|(a, b)| a != b) {
                None => println!("verified {} bytes OK in {:.1} s", image.len(), t0.elapsed().as_secs_f32()),
                Some(i) => return Err(format!("MISMATCH at offset 0x{i:X}: file {:02X}, board {:02X}", image[i], flash[i])),
            }
        }
        _ => return Err("usage: read <out.bin> | boot <COMx> | write <COMx> <in.bin> | verify <in.bin>".into()),
    }
    Ok(())
}
