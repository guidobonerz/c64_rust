//! Drawing: lines, rectangles and circles on the hires bitmap. Press a key
//! to return to the text screen.
#![no_std]
#![no_main]

use c64::gfx;
use c64::hw::{self, Color};
use c64::input;

#[c64::main]
fn main() {
    hw::vic().border.write(Color::Black);
    let g = gfx::hires(Color::LightGreen, Color::Black);
    for i in (0..320i16).step_by(16) {
        g.line(i, 0, 319 - i, 199);
    }
    g.rect(40, 30, 240, 140);
    for r in (10..=60i16).step_by(10) {
        g.circle(160, 100, r);
    }
    for c in 0..40u8 {
        g.set_cell_colors(c, 24, Color::from_u8(c & 15 | 1), Color::Black);
    }
    input::wait_key();
    gfx::text();
}
