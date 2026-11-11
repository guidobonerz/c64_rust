//! SD2IEC: lists the root directory of the card in drive 8, then walks
//! into its first subdirectory, lists that, and comes back.
//!
//! Try it in mimic with a folder as the SD card:
//! `mimic --machine c64 --option drive8=sd2iec --sd2iec-dir <folder>`
#![no_std]
#![no_main]

use c64::dos::Dir;
use c64::text;
use c64_sd2iec as sd;

const DEV: u8 = 8;

/// Prints the directory and returns the name of its first subdirectory.
fn list() -> Option<([u8; 16], usize)> {
    let mut first = None;
    let mut d = match Dir::open(2, DEV) {
        Ok(d) => d,
        Err(e) => {
            c64::println!("cannot read the directory: error ", e.0);
            return None;
        }
    };
    for e in &mut d {
        c64::print!("  ", e.blocks, " ");
        text::print_petscii(e.name());
        c64::print!(" ");
        text::print_petscii(&e.kind);
        c64::println!();
        if first.is_none() && &e.kind == b"DIR" {
            let mut name = [0u8; 16];
            for (to, &from) in name.iter_mut().zip(e.name()) {
                *to = from;
            }
            first = Some((name, e.name().len()));
        }
    }
    first
}

/// Prints the status of the drive.
fn status() {
    let mut s = [0u8; 40];
    if let Some((n, _)) = sd::status(DEV, &mut s) {
        text::print_petscii(&s[..n]);
        c64::println!();
    }
}

#[c64::main]
fn main() {
    if !sd::detect(DEV) {
        c64::println!("drive ", DEV, " is not an sd2iec");
        return;
    }
    let _ = sd::version(DEV);
    status();

    let _ = sd::cd_root(DEV);
    c64::println!("root:");
    let Some((name, len)) = list() else {
        c64::println!("no subdirectories");
        return;
    };

    // The name came from the directory: it is PETSCII already, and the
    // command wants it as it is.
    let name = unsafe { core::str::from_utf8_unchecked(&name[..len]) };
    if sd::cd(DEV, name).is_ok() && sd::check(DEV).is_ok() {
        text::print_petscii(name.as_bytes());
        c64::println!(":");
        list();
        let _ = sd::cd_parent(DEV);
    }
    status();
}
