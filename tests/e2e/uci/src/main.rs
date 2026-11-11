//! End-to-end test of `c64-uci` against mimic's Ultimate command
//! interface (the folder `root` as the storage, no network): detection,
//! identification, echo, reading and writing files, directories, errors.
//! Border green = all passed; the number of the first failing check is at
//! $07C0, the number of checks at $07C2.
#![no_std]
#![no_main]

use c64::hw::{self, Color};
use c64::mem;
use c64_uci::{self as uci, control, dos, net, target};

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

fn starts_with(buf: &[u8], n: usize, text: &[u8]) -> bool {
    n >= text.len() && &buf[..text.len()] == text
}

#[c64::main]
fn main() {
    let mut buf = [0u8; 64];
    // 1: the interface is there
    check(uci::present());
    if !uci::present() {
        hw::vic().border.write(Color::Red);
        return;
    }
    // 2-3: identification of the targets
    let n = dos::identify(&mut buf).unwrap_or(0);
    check(starts_with(&buf, n, b"ULTIMATE-II DOS"));
    let n = net::identify(&mut buf).unwrap_or(0);
    let m = control::identify(&mut buf[40..]).unwrap_or(0);
    check(starts_with(&buf, n, b"ULTIMATE-II NETWORK") && starts_with(&buf[40..], m, b"CONTROL TARGET"));
    // 4: echo returns the whole command; the steps by hand, with the status
    check(uci::request(target::DOS1, 0xf0, b"abc", &mut buf) == Ok(5) && &buf[..5] == &[1, 0xf0, b'a', b'b', b'c']);
    uci::begin(target::DOS1, 0x01);
    let pushed = uci::push();
    let n = uci::read(&mut buf);
    let s = uci::status(&mut buf[40..]);
    let more = uci::accept();
    check(pushed.is_ok() && n > 10 && starts_with(&buf[40..], s, b"00,OK") && !more);
    // 6-7: read a file, whole and in two parts
    check(dos::open("hello.txt", dos::READ).is_ok());
    let n = dos::read(&mut buf).unwrap_or(0);
    check(n == 23 && &buf[..23] == b"HELLO FROM THE ULTIMATE" && dos::file_size() == Ok(23));
    check(dos::seek(6).is_ok() && dos::read(&mut buf[..4]) == Ok(4) && &buf[..4] == b"FROM");
    check(dos::read(&mut buf) == Ok(13) && &buf[..13] == b" THE ULTIMATE" && dos::close().is_ok());
    // 10: a file that is not there (the firmware says 82, mimic 88)
    let e = dos::open("missing.txt", dos::READ);
    check(e == Err(uci::Error::FILE_NOT_FOUND) || e == Err(uci::Error::NO_INFORMATION));
    // 11-12: write a file, read it back, delete it
    check(
        dos::open("fresh.txt", dos::WRITE | dos::CREATE_ALWAYS).is_ok()
            && dos::write(b"written by rust64").is_ok()
            && dos::close().is_ok(),
    );
    check(
        dos::open("fresh.txt", dos::READ).is_ok()
            && dos::read(&mut buf) == Ok(17)
            && &buf[..17] == b"written by rust64"
            && dos::close().is_ok()
            && dos::delete("fresh.txt").is_ok()
            && dos::open("fresh.txt", dos::READ).is_err(),
    );
    // 13: directories
    let n = dos::path(&mut buf).unwrap_or(0);
    check(n >= 1 && buf[0] == b'/' && dos::cd("SUB").is_ok());
    check(
        dos::open("inner.txt", dos::READ).is_ok()
            && dos::read(&mut buf) == Ok(6)
            && &buf[..6] == b"IN SUB"
            && dos::close().is_ok()
            && dos::cd("nowhere").is_err()
            && dos::cd("/").is_ok(),
    );
    // 15: the clock: "YYYY/MM/DD HH:MM:SS"
    let n = dos::time(&mut buf).unwrap_or(0);
    check(n >= 19 && buf[4] == b'/' && buf[7] == b'/' && buf[13] == b':');
    // 16: network without a network: the interface answers
    check(net::interface_count().is_ok());
    unsafe {
        hw::vic().border.write(if FAILS == 0 { Color::Green } else { Color::Red });
    }
}
