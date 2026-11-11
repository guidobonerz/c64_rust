//! End-to-end test of `c64::ui`: boxes are drawn and the screen and colour
//! memory are read back. Border green = all passed; the number of the
//! first failing check is at $07C0, the number of checks at $07C2.
#![no_std]
#![no_main]

use c64::hw::{self, Color};
use c64::mem;
use c64::text;
use c64::ui::{self, Border};

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

fn scr(col: u8, row: u8) -> u8 {
    mem::peek(0x0400 + row as u16 * 40 + col as u16)
}

fn col(col: u8, row: u8) -> u8 {
    mem::peek(0xd800 + row as u16 * 40 + col as u16) & 15
}

#[c64::main]
fn main() {
    text::fill(0x2e, Color::Blue); // dots everywhere

    // 1-4: a square frame: corners, lines, colour, the inside stays
    ui::frame(2, 1, 6, 4, Border::Square, Color::White);
    check(scr(2, 1) == 0x70 && scr(7, 1) == 0x6e && scr(2, 4) == 0x6d && scr(7, 4) == 0x7d);
    check(scr(3, 1) == 0x40 && scr(6, 1) == 0x40 && scr(3, 4) == 0x40 && scr(6, 4) == 0x40);
    check(scr(2, 2) == 0x5d && scr(7, 2) == 0x5d && scr(2, 3) == 0x5d && scr(7, 3) == 0x5d);
    check(scr(3, 2) == 0x2e && scr(6, 3) == 0x2e && col(2, 1) == 1 && col(7, 4) == 1 && col(3, 2) == 6);
    // 5: nothing outside
    check(scr(1, 1) == 0x2e && scr(8, 1) == 0x2e && scr(2, 0) == 0x2e && scr(2, 5) == 0x2e && scr(8, 4) == 0x2e);

    // 6-7: a round panel: corners, the inside is cleared in the colour
    ui::panel(10, 1, 5, 5, Border::Round, Color::Yellow);
    check(scr(10, 1) == 0x55 && scr(14, 1) == 0x49 && scr(10, 5) == 0x4a && scr(14, 5) == 0x4b);
    check(scr(11, 2) == 0x20 && scr(13, 4) == 0x20 && col(12, 3) == 7 && scr(15, 3) == 0x2e);

    // 8-9: a title, from the third column; the line goes on after it
    ui::titled_frame(20, 1, 12, 3, Border::Square, Color::Cyan, "Hi 1");
    check(scr(20, 1) == 0x70 && scr(21, 1) == 0x40 && scr(22, 1) == 0x08 && scr(23, 1) == 0x09);
    check(scr(24, 1) == 0x20 && scr(25, 1) == 0x31 && scr(26, 1) == 0x40 && scr(31, 1) == 0x6e);
    // 10: a title that is too long is cut off: w - 4 characters
    ui::titled_panel(20, 5, 7, 3, Border::Round, Color::Green, "ABCDEFG");
    check(scr(22, 5) == 0x01 && scr(24, 5) == 0x03 && scr(25, 5) == 0x40 && scr(26, 5) == 0x49);
    // 11: no room for a title: only the box
    ui::titled_frame(30, 5, 4, 2, Border::Square, Color::Green, "AB");
    check(scr(30, 5) == 0x70 && scr(31, 5) == 0x40 && scr(32, 5) == 0x40 && scr(33, 5) == 0x6e && scr(30, 6) == 0x6d);

    // 12: the smallest box, 2 x 2
    ui::frame(0, 10, 2, 2, Border::Round, Color::White);
    check(scr(0, 10) == 0x55 && scr(1, 10) == 0x49 && scr(0, 11) == 0x4a && scr(1, 11) == 0x4b);
    // 13: the whole screen
    ui::frame(0, 0, 40, 25, Border::Square, Color::Red);
    check(scr(0, 0) == 0x70 && scr(39, 0) == 0x6e && scr(0, 24) == 0x6d && scr(39, 24) == 0x7d
        && scr(0, 12) == 0x5d && scr(39, 12) == 0x5d && scr(20, 24) == 0x40 && col(39, 24) == 2);
    // 14: too small or off the screen: nothing is drawn
    text::fill(0x2e, Color::Blue);
    ui::frame(5, 5, 1, 5, Border::Square, Color::White);
    ui::frame(5, 5, 5, 1, Border::Square, Color::White);
    ui::frame(36, 5, 5, 3, Border::Square, Color::White);
    ui::frame(5, 23, 5, 3, Border::Square, Color::White);
    ui::frame(200, 5, 100, 3, Border::Square, Color::White);
    let mut clean = true;
    for a in 0x0400..0x07c0u16 {
        clean &= mem::peek(a) == 0x2e;
    }
    check(clean);
    // 15: the last column and row are reachable
    ui::frame(37, 22, 3, 3, Border::Square, Color::White);
    check(scr(37, 22) == 0x70 && scr(39, 24) == 0x7d && scr(38, 23) == 0x2e);

    unsafe {
        hw::vic().border.write(if FAILS == 0 { Color::Green } else { Color::Red });
    }
}
