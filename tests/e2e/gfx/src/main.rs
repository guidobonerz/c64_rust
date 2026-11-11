//! End-to-end test of c64::gfx: draws in hires and multicolour mode, checks
//! the bitmap, returns to the text screen. Border green = all passed; the
//! number of the first failing check is at $0400.
#![no_std]
#![no_main]

use c64::gfx;
use c64::hw::{self, Color};
use c64::mem;
use core::hint::black_box as bb;

static mut TEST: u8 = 0;
static mut FAILS: u8 = 0;

fn check(ok: bool) {
    unsafe {
        TEST += 1;
        if !ok && FAILS == 0 {
            FAILS = TEST;
        }
    }
}

#[c64::main]
fn main() {
    let mut g = gfx::hires(Color::White, Color::Black);
    // 1-3: plot, line, point
    g.plot(bb(10), 5);
    check(g.point(10, 5) && !g.point(11, 5) && mem::peek(gfx::BITMAP + 5 + 8) == 0x20);
    g.line(0, 199, bb(319), 0);
    check(g.point(0, 199) && g.point(319, 0) && g.point(160, 100) || g.point(159, 100));
    g.pen = 0;
    g.plot(10, 5);
    check(!g.point(10, 5));
    // 4: circle and rectangle
    g.pen = 1;
    g.circle(100, 100, bb(20));
    g.rect(200, 20, 50, 30);
    check(g.point(120, 100) && g.point(100, 80) && g.point(249, 49) && !g.point(100, 100));
    // 5: cell colours
    g.set_cell_colors(3, 2, Color::Red, Color::Blue);
    check(mem::peek(gfx::MATRIX + 83) == 0x26);
    // 6-7: filled rectangles: whole bytes and the masks at both ends
    g.clear();
    g.fill_rect(bb(3), 8, 10, 2);
    check(
        mem::peek(gfx::BITMAP + 320) == 0x1f
            && mem::peek(gfx::BITMAP + 328) == 0xf8
            && mem::peek(gfx::BITMAP + 329) == 0xf8
            && mem::peek(gfx::BITMAP + 322) == 0
            && mem::peek(gfx::BITMAP + 336) == 0,
    );
    g.pen = 0;
    g.fill_rect(bb(5), 8, 2, 1);
    g.pen = 1;
    g.fill_rect(bb(300), 190, 100, 100);
    g.fill_rect(bb(-20), 16, 60, 1);
    check(
        mem::peek(gfx::BITMAP + 320) == 0x19
            && mem::peek(gfx::BITMAP + 7999) == 0xff
            && mem::peek(gfx::BITMAP + 7680 + 296) == 0x0f
            && mem::peek(gfx::BITMAP + 640 + 32) == 0xff
            && mem::peek(gfx::BITMAP + 640 + 40) == 0,
    );
    // 8-10: lines in all directions, to the left and up, across cell edges
    g.clear();
    g.line(bb(100), 50, 60, 40);
    g.line(bb(10), 90, 14, 60);
    g.line(bb(200), 100, 263, 100);
    check(g.point(100, 50) && g.point(60, 40) && g.point(80, 45) && !g.point(80, 44));
    check(g.point(10, 90) && g.point(14, 60) && g.point(12, 75) && !g.point(13, 75));
    check(g.point(200, 100) && g.point(263, 100) && !g.point(199, 100) && !g.point(264, 100));
    // 11: the diagonals step in x and y every time
    g.line(bb(7), 7, 9, 9);
    g.line(bb(40), 23, 38, 25);
    check(
        mem::peek(gfx::BITMAP + 7) == 0x01
            && mem::peek(gfx::BITMAP + 320 + 8) == 0x80
            && mem::peek(gfx::BITMAP + 320 + 9) == 0x40
            && g.point(40, 23)
            && g.point(39, 24)
            && g.point(38, 25),
    );
    // 12-13: flat lines, pixel by pixel (line) and in runs (fline): 8, 16
    // and 8 pixels on three lines, the same from the right; hline, vline
    g.clear();
    g.line(bb(0), 50, 31, 52);
    g.fline(bb(31), 60, 0, 58);
    g.fline(bb(40), 50, 71, 52);
    g.line(bb(71), 60, 40, 58);
    g.hline(bb(90), 85, 50);
    g.hline(bb(-9), 3, 51);
    g.vline(bb(100), 54, 49);
    g.vline(bb(101), 190, 300);
    check(
        mem::peek(gfx::BITMAP + 1920 + 40 + 2) == 0xff
            && mem::peek(gfx::BITMAP + 1920 + 48 + 2) == 0
            && mem::peek(gfx::BITMAP + 1920 + 48 + 3) == 0xff
            && mem::peek(gfx::BITMAP + 1920 + 56 + 3) == 0xff
            && mem::peek(gfx::BITMAP + 1920 + 64 + 3) == 0
            && mem::peek(gfx::BITMAP + 1920 + 64 + 4) == 0xff
            && mem::peek(gfx::BITMAP + 2240 + 40 + 2) == 0xff
            && mem::peek(gfx::BITMAP + 2240 + 48 + 3) == 0xff
            && mem::peek(gfx::BITMAP + 2240 + 56 + 3) == 0xff
            && mem::peek(gfx::BITMAP + 2240 + 64 + 4) == 0xff
            && mem::peek(gfx::BITMAP + 1920 + 80 + 2) == 0x07
            && mem::peek(gfx::BITMAP + 1920 + 88 + 2) == 0xe0
            && mem::peek(gfx::BITMAP + 1920 + 3) == 0xf0
            && mem::peek(gfx::BITMAP + 1920 + 96) == 0
            && mem::peek(gfx::BITMAP + 1920 + 96 + 1) == 0x08
            && mem::peek(gfx::BITMAP + 1920 + 96 + 6) == 0x08
            && mem::peek(gfx::BITMAP + 1920 + 96 + 7) == 0
            && mem::peek(gfx::BITMAP + 7360 + 96 + 5) == 0
            && mem::peek(gfx::BITMAP + 7360 + 96 + 6) == 0x04
            && mem::peek(gfx::BITMAP + 7680 + 96 + 7) == 0x04,
    );
    check(
        mem::peek(gfx::BITMAP + 1920 + 2) == 0xff
            && mem::peek(gfx::BITMAP + 1920 + 8 + 2) == 0
            && mem::peek(gfx::BITMAP + 1920 + 8 + 3) == 0xff
            && mem::peek(gfx::BITMAP + 1920 + 16 + 3) == 0xff
            && mem::peek(gfx::BITMAP + 1920 + 24 + 3) == 0
            && mem::peek(gfx::BITMAP + 1920 + 24 + 4) == 0xff
            && mem::peek(gfx::BITMAP + 2240 + 2) == 0xff
            && mem::peek(gfx::BITMAP + 2240 + 8 + 3) == 0xff
            && mem::peek(gfx::BITMAP + 2240 + 16 + 3) == 0xff
            && mem::peek(gfx::BITMAP + 2240 + 24 + 4) == 0xff
            && mem::peek(gfx::BITMAP + 2240 + 24 + 3) == 0,
    );
    // 14: shapes that leave the screen are cut off
    g.clear();
    g.line(bb(-10), 20, 30, 20);
    g.line(bb(310), 190, 330, 210);
    g.rect(bb(-5), 100, 20, 10);
    g.circle(bb(0), 0, 5);
    check(
        g.point(0, 20)
            && g.point(30, 20)
            && !g.point(31, 20)
            && g.point(319, 199)
            && g.point(14, 100)
            && g.point(14, 105)
            && g.point(0, 109)
            && !g.point(0, 105)
            && g.point(5, 0)
            && g.point(0, 5)
            && !g.point(0, 0),
    );
    // 15: multicolour
    let mut m = gfx::multicolor(Color::Black, Color::Red, Color::Green, Color::Blue);
    m.plot(bb(1), 0);
    check(mem::peek(gfx::BITMAP) == 0x10 && mem::peek(gfx::MATRIX) == 0x25);
    // 16-17: multicolour areas and lines, two bits per pixel
    m.pen = 3;
    m.fill_rect(bb(2), 16, 3, 1);
    m.pen = 2;
    m.line(bb(3), 24, 0, 24);
    m.line(bb(0), 32, 3, 35);
    check(
        mem::peek(gfx::BITMAP + 640) == 0x0f
            && mem::peek(gfx::BITMAP + 648) == 0xc0
            && mem::peek(gfx::BITMAP + 960) == 0xaa
            && mem::peek(gfx::BITMAP + 1280) == 0x80
            && mem::peek(gfx::BITMAP + 1281) == 0x20
            && mem::peek(gfx::BITMAP + 1282) == 0x08
            && mem::peek(gfx::BITMAP + 1283) == 0x02,
    );
    m.pen = 1;
    m.line(bb(159), 199, 150, 190);
    m.circle(bb(80), 100, 10);
    m.line(bb(-4), 8, 4, 8);
    check(
        mem::peek(gfx::BITMAP + 7999) == 0x01
            && mem::peek(gfx::BITMAP + 7360 + 296 + 6) == 0x04
            && mem::peek(gfx::BITMAP + 320 * 12 + 176 + 4) == 0x04
            && mem::peek(gfx::BITMAP + 320) == 0x55
            && mem::peek(gfx::BITMAP + 328) == 0x40,
    );
    // 18: the VIC is in bitmap mode, bank 1
    let v = hw::vic();
    check(v.control1.read() & 0x20 != 0 && hw::cia2().pra.read() & 3 == 2);
    gfx::text();
    check(v.control1.read() & 0x20 == 0);
    unsafe {
        mem::poke(0x0400, FAILS);
        mem::poke(0x0401, TEST);
        v.border.write(if FAILS == 0 { Color::Green } else { Color::Red });
    }
}
