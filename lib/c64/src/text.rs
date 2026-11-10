//! The 40x25 text screen: printing through the KERNAL (with the cursor),
//! screen codes and colours written directly, scrolling, character sets.
//!
//! Strings are ASCII and converted to PETSCII when printed: in the default
//! upper case/graphics mode both cases print as capitals; after
//! [`lowercase`] they print as written.

use crate::fmt::Write;
use crate::hw::{self, Color};
use crate::kernal;
use crate::mem;

pub const WIDTH: u8 = 40;
pub const HEIGHT: u8 = 25;

const COLOR_RAM: u16 = 0xd800;

static mut SCREEN: u16 = 0x0400;
static mut LOWER: bool = false;

/// `row * 40`
static ROW_OFFSET: [u16; 25] = [
    0, 40, 80, 120, 160, 200, 240, 280, 320, 360, 400, 440, 480, 520, 560, 600, 640, 680, 720, 760,
    800, 840, 880, 920, 960,
];

/// The PETSCII codes that select the 16 colours with CHROUT.
static COLOR_CODES: [u8; 16] =
    [144, 5, 28, 159, 156, 30, 31, 158, 129, 149, 150, 151, 152, 153, 154, 155];

/// Whether the lower/upper case character set is selected (see
/// [`lowercase`]).
pub fn is_lowercase() -> bool {
    unsafe { LOWER }
}

/// The address of the screen in use (see [`set_screen_addr`]).
pub fn screen_addr() -> u16 {
    unsafe { SCREEN }
}

/// Converts an ASCII character to PETSCII for the current character set.
pub fn petscii(c: u8) -> u8 {
    match c {
        b'\n' => kernal::RETURN,
        b'a'..=b'z' => c - 0x20,
        b'A'..=b'Z' if is_lowercase() => c + 0x80,
        _ => c,
    }
}

/// Converts an ASCII string to PETSCII at compile time (see [`crate::p!`]):
/// `lower` selects the lower/upper case character set.
pub const fn petscii_str<const N: usize>(s: &str, lower: bool) -> [u8; N] {
    let b = s.as_bytes();
    let mut out = [0u8; N];
    let mut i = 0;
    while i < N && i < b.len() {
        let c = b[i];
        out[i] = match c {
            b'\n' => 13,
            b'a'..=b'z' => c - 0x20,
            b'A'..=b'Z' if lower => c + 0x80,
            _ => c,
        };
        i += 1;
    }
    out
}

/// Prints PETSCII bytes as they are (no conversion), e.g. a [`crate::p!`]
/// literal or a file name read from disk.
pub fn print_petscii(s: &[u8]) {
    for &c in s {
        kernal::chrout(c);
    }
}

/// Converts a PETSCII character to a screen code.
pub fn petscii_to_screen(c: u8) -> u8 {
    match c {
        0x00..=0x1f => c + 0x80,
        0x20..=0x3f => c,
        0x40..=0x5f => c - 0x40,
        0x60..=0x7f => c - 0x20,
        0x80..=0x9f => c + 0x40,
        0xa0..=0xbf => c - 0x40,
        0xff => 0x5e,
        _ => c - 0x80,
    }
}

/// Converts an ASCII character to a screen code for the current character
/// set.
pub fn screen_code(c: u8) -> u8 {
    petscii_to_screen(petscii(c))
}

/// The screen: a [`Write`] that prints at the cursor through the KERNAL.
pub struct Console;

impl Write for Console {
    fn write_byte(&mut self, b: u8) {
        kernal::chrout(petscii(b));
    }
}

/// Prints `s` at the cursor.
pub fn print(s: &str) {
    Console.write_str(s);
}

/// Prints `s` and a line break.
pub fn println(s: &str) {
    print(s);
    kernal::chrout(kernal::RETURN);
}

/// Clears the screen and homes the cursor.
pub fn clear() {
    kernal::chrout(kernal::CLEAR);
}

/// Moves the cursor.
pub fn locate(col: u8, row: u8) {
    kernal::plot(row, col);
}

/// Sets the colour of the text printed from now on.
pub fn set_color(c: Color) {
    kernal::chrout(COLOR_CODES[c as usize]);
}

/// Sets the border and background colours.
pub fn colors(border: Color, background: Color) {
    let v = hw::vic();
    v.border.write(border);
    v.background[0].write(background);
}

/// Switches to the lower/upper case character set.
pub fn lowercase() {
    unsafe { LOWER = true };
    kernal::chrout(kernal::LOWERCASE);
}

/// Switches to the upper case/graphics character set.
pub fn uppercase() {
    unsafe { LOWER = false };
    kernal::chrout(kernal::UPPERCASE);
}

/// The screen offset of a position.
pub fn offset(col: u8, row: u8) -> u16 {
    ROW_OFFSET[row as usize] + col as u16
}

/// Writes a screen code and its colour.
pub fn put_char(col: u8, row: u8, code: u8, color: Color) {
    let off = offset(col, row);
    mem::poke(screen_addr() + off, code);
    mem::poke(COLOR_RAM + off, color as u8);
}

/// Writes `s` at a position with a colour, without moving the cursor.
pub fn put_str(col: u8, row: u8, s: &str, color: Color) {
    let mut off = offset(col, row);
    let scr = screen_addr();
    for &b in s.as_bytes() {
        mem::poke(scr + off, screen_code(b));
        mem::poke(COLOR_RAM + off, color as u8);
        off += 1;
    }
}

/// Fills the whole screen with a screen code and colour.
pub fn fill(code: u8, color: Color) {
    mem::fill(screen_addr(), 1000, code);
    mem::fill(COLOR_RAM, 1000, color as u8);
}

/// Fills a rectangle with a screen code and colour.
pub fn fill_rect(col: u8, row: u8, w: u8, h: u8, code: u8, color: Color) {
    for y in row..row + h {
        let off = offset(col, y);
        mem::fill(screen_addr() + off, w as u16, code);
        mem::fill(COLOR_RAM + off, w as u16, color as u8);
    }
}

/// Moves the screen up one line and fills the last line.
pub fn scroll_up(code: u8, color: Color) {
    let scr = screen_addr();
    mem::copy(scr, scr + WIDTH as u16, 960);
    mem::copy(COLOR_RAM, COLOR_RAM + WIDTH as u16, 960);
    mem::fill(scr + 960, WIDTH as u16, code);
    mem::fill(COLOR_RAM + 960, WIDTH as u16, color as u8);
}

/// Points the VIC (and this module) at another screen: `addr` must be a
/// multiple of `$400` inside the current VIC bank. The KERNAL is told as
/// well (`$0288`), so printing goes there too.
pub fn set_screen_addr(addr: u16) {
    unsafe { SCREEN = addr };
    let m = &hw::vic().memory;
    m.write(m.read() & 0x0f | ((addr & 0x3c00) >> 6) as u8);
    mem::poke(0x0288, (addr >> 8) as u8);
}

/// Selects the character set at `addr` (a multiple of `$800` inside the VIC
/// bank; `$1000` and `$1800` are the ROM sets in banks 0 and 2).
pub fn set_charset(addr: u16) {
    let m = &hw::vic().memory;
    m.write(m.read() & 0xf1 | ((addr & 0x3800) >> 10) as u8);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        assert_eq!(screen_code(b'A'), 1);
        assert_eq!(screen_code(b'a'), 1);
        assert_eq!(screen_code(b'@'), 0);
        assert_eq!(screen_code(b' '), 32);
        assert_eq!(screen_code(b'0'), 48);
        assert_eq!(petscii(b'\n'), 13);
    }
}
