//! End-to-end test of `c64-sd2iec` against mimic's SD2IEC (drive 8, the
//! folder `card` as its SD card): detection, directories, loading a file
//! from a subdirectory, errors. Border green = all passed; the number of
//! the first failing check is at $07C0, the number of checks at $07C2.
#![no_std]
#![no_main]

use c64::hw::{self, Color};
use c64::{dos, mem};
use c64_sd2iec as sd;

static mut TEST: u8 = 0;
static mut FAILS: u8 = 0;

fn check(ok: bool) {
    unsafe {
        TEST += 1;
        mem::poke(0x07c2, TEST);
        if !ok {
            if FAILS == 0 {
                mem::poke(0x07c0, TEST);
            }
            FAILS += 1;
        }
    }
}

/// Whether the memory at `addr` holds `text`.
fn mem_is(addr: u16, text: &[u8]) -> bool {
    let mut a = addr;
    for &c in text {
        if mem::peek(a) != c {
            return false;
        }
        a += 1;
    }
    true
}

#[c64::main]
fn main() {
    const DEV: u8 = 8;
    // 1: it is an SD2IEC
    check(sd::detect(DEV));
    // 2: a file in the root directory
    check(dos::load("top", DEV, 0xc000) == Ok(0xc004) && mem_is(0xc000, b"ROOT"));
    // 3-4: into a subdirectory, and a file there
    check(sd::cd(DEV, "sub").is_ok() && sd::check(DEV) == Ok(0));
    check(dos::load("data", DEV, 0xc100) == Ok(0xc110) && mem_is(0xc100, b"RUST64 ON SD2IEC"));
    // 5: a directory that does not exist: path not found
    check(sd::cd(DEV, "nowhere").is_ok() && sd::check(DEV) == Err(39));
    // 6: back up; the file of the subdirectory is not here
    check(sd::cd_parent(DEV).is_ok() && sd::check(DEV) == Ok(0));
    let _ = dos::load("data", DEV, 0xc200);
    check(sd::check(DEV) == Err(62));
    // 8-9: create a directory, enter it by its path, leave, remove it
    check(sd::mkdir(DEV, "fresh").is_ok() && sd::check(DEV) == Ok(0));
    check(
        sd::cd(DEV, "/fresh").is_ok()
            && sd::check(DEV) == Ok(0)
            && sd::cd_root(DEV).is_ok()
            && sd::check(DEV) == Ok(0)
            && sd::rmdir(DEV, "fresh").is_ok()
            && sd::check(DEV) == Ok(0)
            && sd::cd(DEV, "fresh").is_ok()
            && sd::check(DEV) == Err(39),
    );
    // 10: settings and the version
    let mut s = [0u8; 40];
    check(
        sd::hide_extensions(DEV, false).is_ok()
            && sd::check(DEV) == Ok(0)
            && sd::images_as(DEV, sd::Images::Files).is_ok()
            && sd::check(DEV) == Ok(0)
            && sd::version(DEV).is_ok()
            && matches!(sd::status(DEV, &mut s), Some((n, 3)) if n > 10 && &s[3..9] == b"SD2IEC"),
    );
    unsafe {
        hw::vic().border.write(if FAILS == 0 { Color::Green } else { Color::Red });
    }
}
