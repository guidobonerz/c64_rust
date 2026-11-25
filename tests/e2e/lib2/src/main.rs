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

static mut MARKS: [u8; 8] = [0; 8];
static mut MARK_AT: u8 = 0;

/// A handler that writes through a pointer: it uses the scratch pointers
/// that the main program is using when the interrupt comes.
fn mark() {
    unsafe {
        let at = core::ptr::read_volatile(&raw const MARK_AT);
        let p = bb(&raw mut MARKS as *mut u8).add((at & 7) as usize);
        *p = (*p).wrapping_add(1);
        core::ptr::write_volatile(&raw mut MARK_AT, at.wrapping_add(1));
    }
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

    // 15: inline assembly: operands in memory, a constant, a static, a
    // local label, a result through a pointer
    let (x, y) = (bb(0x12f0u16), bb(0x20u8));
    let sum: u16;
    let mut bits = bb(0xa5u8);
    let count: u8;
    let mut cell = [0u8; 3];
    let p = bb(&mut cell[1]);
    unsafe {
        c64::asm!(
            "clc",
            "lda {x}",
            "adc {y}",
            "sta {sum}",
            "lda {x}+1",
            "adc #0",
            "sta {sum}+1",
            x = in(reg) x,
            y = in(reg) y,
            sum = out(reg) sum,
        );
        c64::asm!(
            "ldx #0",
            "@bit: asl {v}",
            "bcc @zero",
            "inx",
            "@zero: lda {v}",
            "bne @bit",
            "stx {n}",
            v = inout(reg) bits,
            n = out(reg) count,
        );
        c64::asm!("lda #{k}", "sta {d}", "inc {t}", k = const 0x42, d = out(reg) *p, t = sym test_value);
    }
    check(sum == 0x1310 && count == 4 && bits == 0 && cell == [0, 0x42, 0] && unsafe { test_value } != 0);

    // 16: the assembly behind text, fmt and the clock
    let mut b = c64::fmt::Buf::<24>::new();
    c64::write!(b, 0u8, ' ', 65535u16, ' ', 4_294_967_295u32, ' ', -1234i16);
    check(
        b.as_bytes() == b"0 65535 4294967295 -1234"
            && text::petscii(b'a') == 0x41
            && text::petscii(b'\n') == 13
            && text::petscii(b'A') == 0x41
            && text::petscii_to_screen(0x41) == 1
            && text::petscii_to_screen(0xff) == 0x5e
            && text::petscii_to_screen(0xa0) == 0x60
            && text::petscii_to_screen(0xc1) == 0x41
            && text::petscii_to_screen(0x90) == 0xd0
            && text::screen_code(b'z') == 26
            && text::screen_code(b' ') == 32,
    );
    text::put_str(37, 24, "ab!", Color::Red);
    text::put_char(0, 23, 0x51, Color::Cyan);
    let t0 = c64::kernal::jiffies();
    hw::wait_frame();
    hw::wait_frame();
    let t1 = c64::kernal::jiffies();
    check(
        mem::peek(0x07e5) == 1
            && mem::peek(0x07e7) == 0x21
            && mem::peek(0xdbe7) & 15 == 2
            && mem::peek(0x0400 + 23 * 40) == 0x51
            && mem::peek(0xd800 + 23 * 40) & 15 == 3
            && t1.wrapping_sub(t0) >= 1
            && t1.wrapping_sub(t0) <= 3
            && c64::kernal::rdtim() as u16 >= t1,
    );

    // The last: interrupts whose handler uses the scratch pointers, while
    // the main program copies through pointers and calls through one.
    irq::add(60, mark);
    irq::add(160, mark);
    let f: fn(u8, u8) -> u16 = bb(add);
    let mut ok = true;
    let mut src = [0u8; 48];
    let mut dst = [0u8; 48];
    let mut round = 0u8;
    while unsafe { core::ptr::read_volatile(&raw const MARK_AT) } < 20 {
        for (i, s) in bb(&mut src[..]).iter_mut().enumerate() {
            *s = round.wrapping_add(i as u8);
        }
        bb(&mut dst[..]).copy_from_slice(bb(&src[..]));
        for (i, d) in bb(&dst[..]).iter().enumerate() {
            ok &= *d == round.wrapping_add(i as u8);
        }
        ok &= f(round, 1) == round as u16 + 1;
        round = round.wrapping_add(1);
    }
    irq::stop();
    let marks: u16 = unsafe { (*(&raw const MARKS)).iter().map(|&m| m as u16).sum() };
    check(ok && marks >= 20 && round > 2);

    // Copies of an odd and of an even number of bytes, and of one
    let mut from = [0u8; 64];
    for (i, b) in from.iter_mut().enumerate() {
        *b = 0x40 + i as u8;
    }
    let mut ok = true;
    for n in [1u16, 2, 3, 11, 39, 40, 63] {
        let mut to = [0xeeu8; 64];
        mem::copy(to.as_mut_ptr() as u16, from.as_ptr() as u16, bb(n));
        for (i, &b) in to.iter().enumerate() {
            ok &= b == if (i as u16) < n { 0x40 + i as u8 } else { 0xee };
        }
    }
    let mut to = [0xeeu8; 12];
    mem::copy(to.as_mut_ptr() as u16, from.as_ptr() as u16, 11);
    ok &= to[10] == 0x4a && to[11] == 0xee && to[0] == 0x40;
    // (front to back: a move down by one where the ranges overlap)
    mem::copy(from.as_mut_ptr() as u16, from.as_ptr() as u16 + 1, 39);
    ok &= from[0] == 0x41 && from[38] == 0x67 && from[39] == 0x67;
    check(ok);

    unsafe {
        hw::vic().border.write(if FAILS == 0 { Color::Green } else { Color::Red });
    }
}
