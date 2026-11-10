//! Diagonal: the whole screen scrolls smoothly up and to the left over an
//! endless pattern drawn with a custom character set. RUN/STOP ends.
//!
//! How it works:
//!
//! * **Custom characters.** Twelve 8x8 characters (`CHARS`) are copied to a
//!   character set at $3000, and the VIC is pointed at it ($D018).
//! * **The world** is a 16x8 tile of these characters (`TILE`) that repeats
//!   in both directions.
//! * **Fine scroll.** XSCROLL ($D016) and YSCROLL ($D011) shift the picture
//!   0-7 pixels; both go down by one every frame, so the picture moves one
//!   pixel left and one pixel up. The 38-column / 24-row modes hide the
//!   edges where the picture is shifted in.
//! * **Double buffering.** After 8 frames the characters have to move by a
//!   whole cell. Copying 1000 bytes in one frame would tear, so there are
//!   two screens ($0400 and $3C00): while one is shown and fine-scrolled,
//!   the next position is built in the other one, a few rows per frame, and
//!   then the VIC is switched to it ($D018) in the border.
//! * **Colours** are fixed bands in the colour RAM, which is not scrolled:
//!   the pattern moves through them.
#![no_std]
#![no_main]

use c64::hw::{self, Color};
use c64::{kernal, mem, text};
use core::mem::MaybeUninit;

// The memory the demo uses, placed (not loaded): the build fails if the
// program grows into it.
#[c64::at(0x3000)]
static mut CHARSET: MaybeUninit<[u8; 2048]> = MaybeUninit::uninit();
#[c64::at(0x3c00)]
static mut SCREEN_B: MaybeUninit<[u8; 1000]> = MaybeUninit::uninit();

const SCREEN_A: u16 = 0x0400;
/// $D018: screen at $0400 / $3C00, character set at $3000.
const SHOW_A: u8 = 0x1c;
const SHOW_B: u8 = 0xfc;
/// $D018 at power-on: screen at $0400, the ROM character set.
const SHOW_DEFAULT: u8 = 0x15;

const TILE_W: usize = 16;
const TILE_H: usize = 8;
/// A tile row repeated often enough to copy 40 columns from any offset.
const WIDE: usize = 40 + TILE_W;
/// Rows of the hidden screen built per frame.
const ROWS_PER_FRAME: u8 = 7;

/// The custom characters, 8 bytes each (one byte per pixel row).
static CHARS: [[u8; 8]; 12] = [
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // 0: empty
    [0xc0, 0xe0, 0x70, 0x38, 0x1c, 0x0e, 0x07, 0x03], // 1: \
    [0x03, 0x07, 0x0e, 0x1c, 0x38, 0x70, 0xe0, 0xc0], // 2: /
    [0x18, 0x3c, 0x7e, 0xff, 0xff, 0x7e, 0x3c, 0x18], // 3: diamond
    [0x00, 0x00, 0x18, 0x3c, 0x3c, 0x18, 0x00, 0x00], // 4: dot
    [0x3c, 0x66, 0xc3, 0xc3, 0xc3, 0xc3, 0x66, 0x3c], // 5: ring
    [0xfc, 0xc6, 0xc6, 0xfc, 0xd8, 0xcc, 0xc6, 0x00], // 6: R
    [0xc6, 0xc6, 0xc6, 0xc6, 0xc6, 0xc6, 0x7c, 0x00], // 7: U
    [0x7c, 0xc6, 0xc0, 0x7c, 0x06, 0xc6, 0x7c, 0x00], // 8: S
    [0xfe, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x00], // 9: T
    [0x3c, 0x60, 0xc0, 0xfc, 0xc6, 0xc6, 0x7c, 0x00], // 10: 6
    [0x1c, 0x3c, 0x6c, 0xcc, 0xfe, 0x0c, 0x0c, 0x00], // 11: 4
];

/// The tile as text; `code` turns each character into a character number.
static TILE: [&[u8; TILE_W]; TILE_H] = [
    b"RUST64    /\\    ",
    b"         /  \\   ",
    b"  .     /  # \\  ",
    b"        \\    /  ",
    b"   o     \\  /   ",
    b"          \\/    ",
    b" #     .        ",
    b"            o   ",
];

const fn code(c: u8) -> u8 {
    match c {
        b'\\' => 1,
        b'/' => 2,
        b'#' => 3,
        b'.' => 4,
        b'o' => 5,
        b'R' => 6,
        b'U' => 7,
        b'S' => 8,
        b'T' => 9,
        b'6' => 10,
        b'4' => 11,
        _ => 0,
    }
}

/// The colour bands, top to bottom.
static BANDS: [Color; 5] = [Color::LightRed, Color::Yellow, Color::LightGreen, Color::Cyan, Color::LightBlue];

/// Each tile row as character numbers, repeated to `WIDE` columns.
static mut ROWS: [[u8; WIDE]; TILE_H] = [[0; WIDE]; TILE_H];
/// The address of each row of `ROWS`.
static mut ROW_ADDR: [u16; TILE_H] = [0; TILE_H];

fn charset_addr() -> u16 {
    (&raw mut CHARSET) as usize as u16
}

fn screen_b_addr() -> u16 {
    (&raw mut SCREEN_B) as usize as u16
}

/// Copies the custom characters into the character set and prepares the
/// tile rows.
fn init() {
    let mut a = charset_addr();
    for ch in &CHARS {
        for &b in ch {
            mem::poke(a, b);
            a += 1;
        }
    }
    unsafe {
        let rows = &mut *(&raw mut ROWS);
        let addrs = &mut *(&raw mut ROW_ADDR);
        for (r, row) in rows.iter_mut().enumerate() {
            for (i, cell) in row.iter_mut().enumerate() {
                *cell = code(TILE[r][i % TILE_W]);
            }
            addrs[r] = row.as_ptr() as usize as u16;
        }
    }
    // colour RAM: bands of five rows
    for row in 0..text::HEIGHT {
        mem::fill(0xd800 + text::offset(0, row), text::WIDTH as u16, BANDS[(row / 5) as usize] as u8);
    }
}

/// Builds rows `first..first + count` of the screen at `screen` for the
/// world position (`ox`, `oy`): row y shows tile row `oy + y` from column
/// `ox` on.
fn build(screen: u16, ox: u8, oy: u8, first: u8, count: u8) {
    let mut dst = screen + text::offset(0, first);
    for y in first..first + count {
        let src = unsafe { ROW_ADDR[((oy + y) & (TILE_H as u8 - 1)) as usize] } + ox as u16;
        mem::copy(dst, src, text::WIDTH as u16);
        dst += text::WIDTH as u16;
    }
}

#[c64::main]
fn main() {
    text::colors(Color::Black, Color::Black);
    init();
    build(SCREEN_A, 0, 0, 0, text::HEIGHT);

    let v = hw::vic();
    let (mut ox, mut oy) = (0u8, 0u8); // world position of the visible screen
    let mut fine = 7u8; // the pixel shift, 7 down to 0
    let mut a_visible = true;
    let mut built = 0u8; // rows of the hidden screen that are ready
    v.memory.write(SHOW_A);

    while !kernal::stop() {
        hw::wait_frame();
        // In the border: the next pixel position, or the next screen.
        if fine == 0 {
            fine = 7;
            a_visible = !a_visible;
            v.memory.write(if a_visible { SHOW_A } else { SHOW_B });
            ox = (ox + 1) & (TILE_W as u8 - 1);
            oy = (oy + 1) & (TILE_H as u8 - 1);
            built = 0;
        } else {
            fine -= 1;
        }
        v.control2.write(0xc0 | fine); // 38 columns, XSCROLL
        v.control1.write(0x10 | fine); // screen on, 24 rows, YSCROLL

        // Build a part of the hidden screen: one cell further.
        if built < text::HEIGHT {
            let hidden = if a_visible { screen_b_addr() } else { SCREEN_A };
            let count = ROWS_PER_FRAME.min(text::HEIGHT - built);
            build(hidden, (ox + 1) & (TILE_W as u8 - 1), (oy + 1) & (TILE_H as u8 - 1), built, count);
            built += count;
        }
    }

    v.control1.write(0x1b);
    v.control2.write(0xc8);
    v.memory.write(SHOW_DEFAULT);
    text::clear();
}
