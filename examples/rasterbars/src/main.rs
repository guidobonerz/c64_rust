//! Raster bars: three colour bars swing up and down behind the text, with
//! clean edges over the whole width of the screen. RUN/STOP ends.
//!
//! How it works: the VIC has one border and one background colour. To get
//! a different colour on every raster line, both registers are written once
//! per line, while the beam is outside the visible picture. That takes
//! exact timing, cycle by cycle, which only assembly can give: `raster.rat`
//! has a stable raster interrupt that copies one byte of a colour table to
//! `$D020` and `$D021` on each of 96 lines.
//!
//! Rust does the rest. Once per frame, when the bars have been drawn, it
//! builds the table for the next frame: all black, then the three bars at
//! their positions on a sine curve.
#![no_std]
#![no_main]

use c64::hw::Color;
use c64::{kernal, mem, text};

c64::rat_source!(RASTER, "raster.rat");

unsafe extern "C" {
    /// Installs the raster interrupt.
    fn raster_start();
    /// Removes it and restores the KERNAL interrupt.
    fn raster_stop();
    /// The address of the colour table: one byte per raster line,
    /// [`LINES`] bytes.
    fn raster_colors() -> u16;
    /// Waits until the bars of this frame have been drawn.
    fn raster_wait();
}

/// The raster lines with bars (`RASTER_LINES` in raster.rat), from line 99.
const LINES: u16 = 96;
/// The height of a bar in raster lines.
const BAR: u16 = 11;

/// The colours of the bars, from the top edge to the bottom edge.
static BARS: [[u8; BAR as usize]; 3] = [
    // brown, red, orange, light red, yellow, white, and back
    [9, 2, 8, 10, 7, 1, 7, 10, 8, 2, 9],
    // dark grey, green, cyan, light green, white, and back
    [11, 5, 5, 3, 13, 1, 13, 3, 5, 5, 11],
    // blue, purple, light blue, cyan, white, and back
    [6, 4, 14, 14, 3, 1, 3, 14, 14, 4, 6],
];

/// The top line of a bar over time: a sine curve from 0 to `LINES - BAR`.
static SINE: [u8; 64] = [
    42, 47, 51, 55, 59, 63, 66, 69, 73, 75, 78, 80, 82, 83, 84, 85, 85, 85, 84, 83, 82, 80, 78,
    75, 73, 69, 66, 63, 59, 55, 51, 47, 43, 38, 34, 30, 26, 22, 19, 16, 12, 10, 7, 5, 3, 2, 1, 0,
    0, 0, 1, 2, 3, 5, 7, 10, 12, 16, 19, 22, 26, 30, 34, 38,
];

#[c64::main]
fn main() {
    text::colors(Color::Black, Color::Black);
    text::clear();
    text::put_str(10, 2, "rust64 raster bars", Color::White);
    text::put_str(4, 11, "one colour per raster line, with", Color::White);
    text::put_str(4, 13, "a stable raster interrupt", Color::White);
    text::put_str(9, 22, "press run/stop to end", Color::Grey);

    let table = unsafe { raster_colors() };
    unsafe { raster_start() };

    let mut phase = 0u8;
    while !kernal::stop() {
        // The beam is below the bars: time to build the next frame.
        // It has to be done before the beam is back at the bars, so the
        // bytes are moved by the library's fill and copy routines.
        unsafe { raster_wait() };
        mem::fill(table, LINES, 0);
        let mut at = phase;
        for bar in &BARS {
            let top = SINE[(at & 63) as usize] as u16;
            mem::copy(table + top, bar.as_ptr() as u16, BAR);
            at = at.wrapping_add(9);
        }
        phase = phase.wrapping_add(1);
    }

    unsafe { raster_stop() };
    text::clear();
}
