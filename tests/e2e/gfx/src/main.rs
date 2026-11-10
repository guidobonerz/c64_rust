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
    // 6: multicolour
    let m = gfx::multicolor(Color::Black, Color::Red, Color::Green, Color::Blue);
    m.plot(bb(1), 0);
    check(mem::peek(gfx::BITMAP) == 0x10 && mem::peek(gfx::MATRIX) == 0x25);
    // 7: the VIC is in bitmap mode, bank 1
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
