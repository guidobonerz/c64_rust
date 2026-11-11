//! The scroller example (examples/scroller) with its work measured by CIA 2's
//! timers: runs 256 frames and prints the cycles; then what the
//! interrupts cost: the time of a fixed loop with them and without any
//! (screen off, so the VIC takes no cycles). The counterpart of
//! `../oscar64/scroller.c -dTIMED`.
//!
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
//! restores the normal 40 columns. The rows around the scroller are empty,
//! so it does not matter on exactly which raster line the handlers act:
//! each has some 50 lines until the next text. A debug build needs them,
//! its handlers are slow.
#![no_std]
#![no_main]

use c64::hw::{self, Color};
use c64::{irq, mem, text};

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

/// CIA 2's timer A counts the cycles of one frame's work.
fn frame_start() {
    let c = hw::cia2();
    c.cra.write(0);
    c.timer_a.write(0xffff);
    c.cra.write(0x11);
}

fn frame_stop() -> u16 {
    let c = hw::cia2();
    c.cra.write(0);
    0xffff - c.timer_a.read()
}

/// Timer A counts cycles, timer B its underflows.
fn long_start() {
    let c = hw::cia2();
    c.cra.write(0);
    c.crb.write(0);
    c.timer_a.write(0xffff);
    c.timer_b.write(0xffff);
    c.crb.write(0x51);
    c.cra.write(0x11);
}

fn long_stop() -> u32 {
    let c = hw::cia2();
    c.cra.write(0);
    let (a, b) = (c.timer_a.read(), c.timer_b.read());
    (!b as u32) << 16 | !a as u32
}
/// A fixed amount of work: 3 * 65536 times dex/bne.
fn work() {
    unsafe {
        c64::asm!(
            "lda #3",
            "sta rt_t",
            "ldx #0",
            "ldy #0",
            "@l: dex",
            "bne @l",
            "dey",
            "bne @l",
            "dec rt_t",
            "bne @l",
        );
    }
}

fn timed_work() -> u32 {
    long_start();
    work();
    long_stop()
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

    // Below the title (row 4), and just below the scroller.
    irq::add(ROW_LINE - 54, scroll_on);
    irq::add(ROW_LINE + 10, scroll_off);

    let mut next = 0usize;
    let (mut sum, mut max) = (0u32, 0u16);
    for _ in 0..256u16 {
        // Change things while the beam is below the scroller.
        hw::wait_frame();
        frame_start();
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
        let t = frame_stop();
        sum += t as u32;
        max = max.max(t);
    }

    let v = hw::vic();
    // screen off: no bad lines (bit 7 is the raster compare's bit 8: keep it 0)
    v.control1.write(v.control1.read() & 0x6f);
    let with_irq = timed_work();
    let without = c64::sys::without_interrupts(timed_work);
    v.control1.write(v.control1.read() & 0x7f | 0x10);

    irq::stop();
    hw::vic().control2.write(NORMAL);
    text::clear();
    c64::println!("SUM ", sum);
    c64::println!("MAX ", max);
    c64::println!("WIRQ ", with_irq);
    c64::println!("WSEI ", without);
}
