//! End-to-end test of the backend features and library modules of phases
//! 3 and 4: calls through function pointers, `at`/`zp` statics, statics
//! exported to assembly, raster interrupts, SID tunes, sprites, sound,
//! PETSCII literals. Reporting as in the lib test: first failing check at
//! $07C0, number of checks at $07C2; border green = all passed.
#![no_std]
#![no_main]

use c64::hw::{self, Color};
use c64::sound::{self, Note, Voice};
use c64::sprite::Sprite;
use c64::{irq, mem, text};
use core::hint::black_box as bb;
use core::mem::MaybeUninit;

c64::rat_source!(TEST_RAT, "test.rat");
c64::sid!(TICK, "tick.sid");

unsafe extern "C" {
    fn test_get_value() -> u8;
}

#[unsafe(no_mangle)]
static mut test_value: u8 = 0;

#[c64::at(0x8000)]
static TABLE: [u8; 4] = [11, 22, 33, 44];

#[c64::at(0x8100)]
static mut PLACED: MaybeUninit<[u8; 16]> = MaybeUninit::uninit();

#[c64::zp]
static mut ZP_WORD: u16 = 0x1234;

static mut TEST: u8 = 0;
static mut FAILS: u8 = 0;
static mut TICKS: u16 = 0;

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

fn mul(a: u8, b: u8) -> u16 {
    a as u16 * b as u16
}

fn add(a: u8, b: u8) -> u16 {
    a as u16 + b as u16
}

/// Called by the main program and by the interrupt handler.
fn bump(n: u16) -> u16 {
    let x = bb(n) * 3;
    x / 3 + 1
}

fn tick() {
    unsafe {
        TICKS = bump(TICKS);
    }
}

fn wait_frames(n: u8) {
    for _ in 0..n {
        hw::wait_frame();
    }
}

#[c64::main]
fn main() {
    text::clear();

    // 1-3: calls through function pointers
    let ops: [fn(u8, u8) -> u16; 2] = [mul, add];
    check(ops[bb(0)](bb(6), 7) == 42 && ops[bb(1)](bb(6), 7) == 13);
    let double: fn(u16) -> u16 = |x| x * 2;
    check(bb(double)(bb(21)) == 42);
    let f = bb(mul as fn(u8, u8) -> u16);
    check(f(200, 2) == 400);

    // 4-6: statics at fixed addresses and in the zero page
    check(mem::peek(0x8000) == 11 && TABLE[bb(3)] == 44);
    unsafe {
        let p = (&raw mut PLACED).cast::<u8>();
        check(p as usize == 0x8100);
        *p.add(bb(5)) = 99;
        check(mem::peek(0x8105) == 99 && ZP_WORD == 0x1234 && ((&raw const ZP_WORD) as usize) < 0x100);
    }

    // 7: an exported static read by assembly
    unsafe {
        test_value = bb(77);
        check(test_get_value() == 77);
    }

    // 8-9: raster interrupts; bump() runs in both (separate frames)
    irq::add(100, tick);
    let mut s = 0u16;
    for i in 0..bb(200u16) {
        s = bump(s) + (i & 1);
    }
    let t0 = unsafe { TICKS };
    wait_frames(10);
    irq::stop();
    let t = unsafe { TICKS } - t0;
    check(t >= 9 && t <= 11 && t0 > 0);
    check(s == 300);

    // 10: a SID tune: init stores the song at $9100, play counts at $9101
    TICK.start(0);
    wait_frames(10);
    TICK.stop();
    let plays = mem::peek(0x9101);
    check(mem::peek(0x9100) == 0 && plays >= 8 && plays <= 12 && TICK.play == 0x9009);

    // 11-12: sprites
    let sp = Sprite(2);
    sp.move_to(bb(300), 100);
    sp.set_color(Color::Yellow);
    check(sp.x() == 300 && sp.y() == 100 && hw::vic().sprite_x_msb.read() & 4 != 0);
    sp.move_to(bb(20), 100);
    check(sp.x() == 20 && hw::vic().sprite_x_msb.read() & 4 == 0);

    // 13: sound
    sound::volume(15);
    let v = Voice(1);
    v.adsr(0, 9, 0, 0);
    v.note_on(Note::A4, sound::PULSE);
    v.note_off();
    sound::reset();
    check(Note::new(4, 9) == Note::A4 && Note::A4.freq() == 7493 && Note(200).freq() == 65535);

    // 14: PETSCII literals
    check(c64::p!("Ab1") == &[0x41, 0x42, 0x31] && c64::pm!("Ab") == &[0xc1, 0x42]);

    unsafe {
        hw::vic().border.write(if FAILS == 0 { Color::Green } else { Color::Red });
    }
}
