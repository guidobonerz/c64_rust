//! WiC64: shows the state of the WiFi module on the user port and fetches
//! a text from the web.
//!
//! In mimic: `mimic --machine c64 --option userport=wic64`
//! (with `--net off` the module is there, but has no connection).
#![no_std]
#![no_main]

use c64_wic64 as wic;

/// The page to fetch: plain text.
const URL: &str = "http://example.org/";

/// Prints text from the WiC64 or the web (ASCII; other bytes are left out).
fn show(bytes: &[u8]) {
    for &b in bytes {
        match b {
            b'\n' => c64::println!(),
            0x20..=0x7e => c64::print!(b as char),
            _ => {}
        }
    }
}

fn line(label: &str, r: Result<usize, wic::Error>, buf: &[u8]) {
    c64::print!(label);
    match r {
        Ok(n) => show(&buf[..n]),
        Err(e) => c64::print!("error ", e.0),
    }
    c64::println!();
}

#[c64::main]
fn main() {
    if !wic::present() {
        c64::println!("No WiC64 on the user port.");
        return;
    }
    let mut buf = [0u8; 64];
    let r = wic::version(&mut buf);
    line("WiC64 firmware ", r, &buf);

    if !wic::is_connected() {
        c64::println!("Not connected to a WiFi network.");
        let r = wic::status_message(&mut buf);
        line("Status: ", r, &buf);
        return;
    }
    let r = wic::ssid(&mut buf);
    line("Network: ", r, &buf);
    let r = wic::ip(&mut buf);
    line("Address: ", r, &buf);
    let r = wic::local_time(&mut buf);
    line("Time:    ", r, &buf);

    // A page can be larger than any buffer here: ask for it, then read
    // and print it piece by piece. This shows the first 400 bytes.
    c64::println!();
    c64::println!(URL, ":");
    match wic::request(wic::cmd::HTTP_GET, URL.as_bytes()) {
        Ok(len) => {
            let mut left = 400u16;
            while left > 0 {
                let n = wic::read(&mut buf);
                if n == 0 {
                    break;
                }
                show(&buf[..n]);
                left = left.saturating_sub(n as u16);
            }
            c64::println!();
            c64::println!("(", len, " bytes)");
        }
        Err(e) => {
            c64::println!("error ", e.0);
            let r = wic::status_message(&mut buf);
            line("", r, &buf);
        }
    }
}
