//! End-to-end test of the `c64` library. Each check counts up; the number
//! of the first failing check is written to the start of the last screen
//! row ($07C0), the number of checks to $07C2.
//! Border: green = all passed, red = a check failed (or a panic).
#![no_std]
#![no_main]

use c64::fmt::{Buf, Hex};
use c64::hw::{self, Color};
use c64::input::{self, Port};
use c64::{kernal, mem, sys, text};
use core::hint::black_box as bb;

c64::rat_source!(TEST_RAT, "test.rat");

unsafe extern "C" {
    fn test_add3(a: u8, b: u8, c: u8) -> u8;
    fn test_swap(v: u32) -> u32;
}

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

fn screen_is(addr: u16, ascii: &[u8]) -> bool {
    let mut a = addr;
    for &c in ascii {
        if mem::peek(a) != text::screen_code(c) {
            return false;
        }
        a += 1;
    }
    true
}

#[c64::main]
fn main() {
    // 1-3: printing through the KERNAL, the cursor, direct screen writes
    text::clear();
    c64::print!("HI ", bb(42u8), ' ', bb(-7i16));
    check(screen_is(0x0400, b"HI 42 -7"));
    text::locate(5, 3);
    check(kernal::cursor() == (3, 5));
    text::put_str(2, 10, "OK", Color::White);
    check(screen_is(0x0400 + 402, b"OK") && mem::peek(0xd800 + 403) & 15 == 1);

    // 4-5: formatting
    let mut b = Buf::<40>::new();
    c64::write!(b, bb(12345u16), ',', bb(-300i16), ',', bb(0u8), ',', bb(100000u32), ',', Hex(bb(0xd020u16)));
    check(b.as_bytes() == b"12345,-300,0,100000,D020");
    check(text::screen_code(b'A') == 1 && text::screen_code(b'z') == 26);

    // 6-7: memory
    mem::fill(0xc000, bb(300), 0xaa);
    mem::poke(0xc000, 1);
    mem::copy(0xc200, 0xc000, bb(300));
    check(mem::peek(0xc200) == 1 && mem::peek(0xc201) == 0xaa && mem::peek(0xc200 + 299) == 0xaa);
    mem::poke_word(0xc400, bb(0xbeef));
    check(mem::peek_word(0xc400) == 0xbeef);

    // 8-9: machine code calls
    mem::poke(0xc500, 0x18); // clc
    mem::poke(0xc501, 0x69); // adc #1
    mem::poke(0xc502, 0x01);
    mem::poke(0xc503, 0x60); // rts
    let r = unsafe { sys::jsr(0xc500, sys::Regs { a: 41, x: 7, y: 9, p: 0 }) };
    check(r.a == 42 && r.x == 7 && r.y == 9 && !r.carry());
    check(sys::without_interrupts(|| bb(5u8)) == 5);

    // 10-11: assembly functions of this crate
    check(unsafe { test_add3(bb(1), bb(2), bb(3)) } == 6);
    check(unsafe { test_swap(bb(0x11223344)) } == 0x44332211);

    // 12-13: hardware registers
    let v = hw::vic();
    let bg = v.background[0].read();
    v.background[0].write(Color::Blue);
    check(v.background[0].read() == Color::Blue);
    v.background[0].write(bg);
    let t = kernal::jiffies();
    hw::wait_frame();
    hw::wait_frame();
    check(kernal::jiffies() != t);

    // 14-15: input
    check(input::joy(Port::One).idle() && input::joy(Port::Two).idle());
    check(input::key().is_none());

    unsafe {
        v.border.write(if FAILS == 0 { Color::Green } else { Color::Red });
    }
}
