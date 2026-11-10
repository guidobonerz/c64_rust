//! Scroller: a horizontal soft scroll. One text row moves left pixel by
//! pixel; the rest of the screen stands still. RUN/STOP ends.
//!
//! How it works: the VIC can shift the whole picture 0-7 pixels to the right
//! (XSCROLL, bits 0-2 of $D016). Each frame the shift goes down by one; when
//! it would pass 0, the row's characters move one column to the left, a new
//! character enters at the right edge and the shift starts again at 7. The
//! 38-column mode (bit 3 of $D016 clear) hides the columns where characters
//! appear and disappear.
//!
//! Two raster interrupts limit this to the scroller's row: the first one,
//! above the row, sets the shift and 38 columns; the second one, below it,
//! restores the normal 40 columns. The rows next to the scroller are empty,
//! so it does not matter on exactly which raster line the handlers act.
#![no_std]
#![no_main]

use c64::hw::{self, Color};
use c64::{irq, kernal, mem, text};

/// The screen row of the scroller.
const ROW: u8 = 12;
/// Pixels per frame (1, 2 or 4).
const SPEED: u8 = 2;
/// The first raster line of a text row: 50 + row * 8.
const ROW_LINE: u8 = 50 + ROW * 8;

/// $D016 without scrolling: 40 columns.
const NORMAL: u8 = 0xc8;
/// $D016 for the scroller: 38 columns (bit 3 clear), plus the shift.
const SCROLLING: u8 = 0xc0;

static MESSAGE: &[u8] = b"hello from rust64 on the commodore 64 ... \
    this text moves with the vic's xscroll register, set in a raster \
    interrupt for this one row ... written in rust, compiled to 6510 code \
    ... press run/stop to end ...          ";

/// The colours of the scroller's columns.
static RAINBOW: [Color; 8] = [
    Color::Red,
    Color::Orange,
    Color::Yellow,
    Color::LightGreen,
    Color::Cyan,
    Color::LightBlue,
    Color::Purple,
    Color::LightRed,
];

/// The current shift (0-7), read by the interrupt handler.
static mut XSCROLL: u8 = 7;

/// Above the scroller: shift the picture, 38 columns.
fn scroll_on() {
    hw::vic().control2.write(SCROLLING | unsafe { XSCROLL });
}

/// Below the scroller: back to the normal screen.
fn scroll_off() {
    hw::vic().control2.write(NORMAL);
}

#[c64::main]
fn main() {
    text::colors(Color::Black, Color::Black);
    text::clear();
    text::put_str(11, 4, "rust64 soft scroller", Color::White);
    text::put_str(9, 20, "press run/stop to end", Color::Grey);

    // The scroller's row: empty, in rainbow colours.
    let row = text::screen_addr() + text::offset(0, ROW);
    for col in 0..text::WIDTH {
        text::put_char(col, ROW, b' ', RAINBOW[(col / 5) as usize & 7]);
    }

    irq::add(ROW_LINE - 6, scroll_on);
    irq::add(ROW_LINE + 10, scroll_off);

    let mut next = 0usize;
    while !kernal::stop() {
        // Change things while the beam is below the scroller.
        hw::wait_frame();
        let x = unsafe { XSCROLL };
        if x >= SPEED {
            unsafe { XSCROLL = x - SPEED };
        } else {
            // One column to the left, a new character at the right edge.
            mem::copy(row, row + 1, text::WIDTH as u16 - 1);
            mem::poke(row + text::WIDTH as u16 - 1, text::screen_code(MESSAGE[next]));
            next += 1;
            if next == MESSAGE.len() {
                next = 0;
            }
            unsafe { XSCROLL = x + 8 - SPEED };
        }
    }

    irq::stop();
    hw::vic().control2.write(NORMAL);
    text::clear();
}
