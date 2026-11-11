//! End-to-end test of `c64-wic64` against mimic's WiC64 (no network):
//! detection, version, echo with a response read in pieces, errors.
//! Border green = all passed; the number of the first failing check is at
//! $07C0, the number of checks at $07C2.
#![no_std]
#![no_main]

use c64::hw::{self, Color};
use c64::mem;
use c64_wic64::{self as wic, Error, cmd};

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

static TEXT: &[u8] = b"the quick brown fox jumps over the lazy dog, twice: \
    the quick brown fox jumps over the lazy dog";

#[c64::main]
fn main() {
    let mut buf = [0u8; 128];
    // 1-2: a WiC64 answers; its version
    check(wic::present());
    check(wic::version(&mut buf) == Ok(5) && &buf[..5] == b"2.1.0");
    // 3: echo
    check(wic::echo(TEXT, &mut buf) == Ok(TEXT.len()) && &buf[..TEXT.len()] == TEXT);
    // 4-5: a response read in pieces; the rest is dropped by the next request
    check(wic::request(cmd::ECHO, TEXT) == Ok(TEXT.len() as u16));
    check(
        wic::read(&mut buf[..10]) == 10
            && &buf[..10] == b"the quick "
            && wic::remaining() == TEXT.len() as u16 - 10
            && wic::read(&mut buf[..20]) == 20
            && &buf[..20] == b"brown fox jumps over",
    );
    check(wic::echo(b"again", &mut buf) == Ok(5) && &buf[..5] == b"again" && wic::remaining() == 0);
    // 7: an empty payload and an empty response
    check(wic::echo(&[], &mut buf) == Ok(0));
    // 8: without a network: no connection, and the text for it
    check(wic::http_get("http://example.org/", &mut buf) == Err(Error::CONNECTION) && !wic::is_connected());
    let n = wic::status_message(&mut buf).unwrap_or(0);
    check(n > 3 && n <= 40);
    // 10: the default server; an unknown command
    let n = wic::server(&mut buf).unwrap_or(0);
    check(n > 7 && &buf[..7] == b"http://" && wic::command(0x7f, &[], &mut buf) == Err(Error::CLIENT));
    unsafe {
        hw::vic().border.write(if FAILS == 0 { Color::Green } else { Color::Red });
    }
}
