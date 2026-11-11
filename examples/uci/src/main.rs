//! UCI: talks to the Ultimate (1541 Ultimate-II+, Ultimate 64) through its
//! command interface: who it is, its clock, the current directory with its
//! entries, and a small file that is written and read back.
//!
//! The command interface must be enabled in the Ultimate's menu. In mimic:
//! `mimic --machine c64 --option uci=on --uci-dir <folder>`
#![no_std]
#![no_main]

use c64_uci::{self as uci, control, dos};

/// Prints text from the Ultimate (ASCII).
fn show(label: &str, bytes: &[u8]) {
    c64::print!(label);
    for &b in bytes {
        c64::print!(b as char);
    }
    c64::println!();
}

#[c64::main]
fn main() {
    if !uci::present() {
        c64::println!("No Ultimate command interface.");
        return;
    }
    let mut buf = [0u8; 80];

    if let Ok(n) = control::hardware(&mut buf) {
        show("Hardware: ", &buf[..n]);
    }
    if let Ok(n) = dos::identify(&mut buf) {
        show("Files:    ", &buf[..n]);
    }
    if let Ok(n) = dos::time(&mut buf) {
        show("Clock:    ", &buf[..n]);
    }
    if let Ok(n) = dos::path(&mut buf) {
        show("Path:     ", &buf[..n]);
    }

    // The directory: one entry per packet, an attribute byte and the name.
    c64::println!();
    if dos::open_dir().is_ok() && dos::read_dir().is_ok() {
        let mut shown = 0u8;
        loop {
            let n = uci::read(&mut buf);
            if n > 1 && shown < 10 {
                show(if buf[0] & 0x10 != 0 { "  <dir> " } else { "        " }, &buf[1..n]);
                shown += 1;
            }
            if !uci::accept() {
                break;
            }
        }
    }

    // Write a file, read it back, delete it.
    c64::println!();
    let written = dos::open("rust64.txt", dos::WRITE | dos::CREATE_ALWAYS)
        .and_then(|_| dos::write(b"Written by rust64."))
        .and_then(|_| dos::close());
    match written {
        Ok(()) => {
            if dos::open("rust64.txt", dos::READ).is_ok() {
                if let Ok(n) = dos::read(&mut buf) {
                    show("Read back: ", &buf[..n]);
                }
                let _ = dos::close();
            }
            let _ = dos::delete("rust64.txt");
        }
        Err(e) => c64::println!("Cannot write: error ", e.0),
    }
}
