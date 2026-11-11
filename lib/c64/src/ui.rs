//! Text-mode user interface elements: boxes drawn with the PETSCII line
//! characters, with square or round corners, optionally with a title in
//! the top line and a cleared inside.
//!
//! ```ignore
//! use c64::hw::Color;
//! use c64::ui::{self, Border};
//!
//! ui::frame(2, 2, 20, 8, Border::Round, Color::White);
//! ui::titled_panel(10, 6, 24, 10, Border::Square, Color::Yellow, "OPTIONS");
//! ```
//!
//! ```text
//! ╭──────────────────╮      ┌─OPTIONS──────────────┐
//! │                  │      │                      │
//! ╰──────────────────╯      └──────────────────────┘
//! ```
//!
//! Positions are in characters: column 0-39, row 0-24. A box is `w`
//! columns wide and `h` rows high, lines included, so the inside is
//! `w - 2` by `h - 2`. A box that is smaller than 2 x 2 or does not fit on
//! the screen is not drawn.
//!
//! A *frame* draws only the lines and leaves what is inside; a *panel*
//! also clears the inside (spaces in the box's colour), for a window on
//! top of other text.
//!
//! The boxes are written straight into the screen and colour memory (the
//! screen `c64::text` uses, see `c64::text::set_screen_addr`); the cursor
//! does not move. Round corners need the upper case / graphics character
//! set (the default): with `c64::text::lowercase()` they show as letters.
//! Square corners and the lines are in both sets.

use crate::hw::Color;

/// The corners of a box.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Border {
    Square = 0,
    Round = 1,
}

/// Style bit: clear the inside.
const FILL: u8 = 2;

#[cfg(target_os = "c64")]
crate::rat_source!(UI_RAT, "rat/ui.rat");

#[cfg(target_os = "c64")]
unsafe extern "C" {
    fn c64_ui_box(col: u8, row: u8, w: u8, h: u8, style: u8, color: u8, title: *const u8, len: u8);
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn draw(col: u8, row: u8, w: u8, h: u8, style: u8, color: Color, title: &str) {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_ui_box(col, row, w, h, style, color as u8, title.as_ptr(), title.len() as u8)
    }
    #[cfg(not(target_os = "c64"))]
    host::draw(col, row, w, h, style, color, title);
}

/// Draws the lines of a box; what is inside stays.
pub fn frame(col: u8, row: u8, w: u8, h: u8, border: Border, color: Color) {
    draw(col, row, w, h, border as u8, color, "");
}

/// Draws a box and clears its inside.
pub fn panel(col: u8, row: u8, w: u8, h: u8, border: Border, color: Color) {
    draw(col, row, w, h, border as u8 | FILL, color, "");
}

/// [`frame`] with a title in the top line, from the third column. A title
/// longer than `w - 4` is cut off. Put spaces around the text for a gap
/// in the line: `" OPTIONS "`.
pub fn titled_frame(col: u8, row: u8, w: u8, h: u8, border: Border, color: Color, title: &str) {
    draw(col, row, w, h, border as u8, color, title);
}

/// [`panel`] with a title in the top line (see [`titled_frame`]).
pub fn titled_panel(col: u8, row: u8, w: u8, h: u8, border: Border, color: Color, title: &str) {
    draw(col, row, w, h, border as u8 | FILL, color, title);
}

/// The same boxes for the host (tests of programs that use this module).
#[cfg(not(target_os = "c64"))]
mod host {
    use super::FILL;
    use crate::hw::Color;
    use crate::text;

    pub fn draw(col: u8, row: u8, w: u8, h: u8, style: u8, color: Color, title: &str) {
        if w < 2 || h < 2 || col as u16 + w as u16 > 40 || row as u16 + h as u16 > 25 {
            return;
        }
        let (tl, tr, bl, br) = if style & 1 != 0 { (0x55, 0x49, 0x4a, 0x4b) } else { (0x70, 0x6e, 0x6d, 0x7d) };
        let (right, bottom) = (col + w - 1, row + h - 1);
        for x in col + 1..right {
            text::put_char(x, row, 0x40, color);
            text::put_char(x, bottom, 0x40, color);
        }
        for y in row + 1..bottom {
            text::put_char(col, y, 0x5d, color);
            text::put_char(right, y, 0x5d, color);
            if style & FILL != 0 {
                for x in col + 1..right {
                    text::put_char(x, y, 0x20, color);
                }
            }
        }
        text::put_char(col, row, tl, color);
        text::put_char(right, row, tr, color);
        text::put_char(col, bottom, bl, color);
        text::put_char(right, bottom, br, color);
        for (i, &b) in title.as_bytes().iter().take(w.saturating_sub(4) as usize).enumerate() {
            text::put_char(col + 2 + i as u8, row, text::screen_code(b), color);
        }
    }
}
