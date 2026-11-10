//! The bitmap screen: hires (320x200, two colours per 8x8 cell) and
//! multicolour (160x200, four colours per cell), as types.
//!
//! The bitmap uses VIC bank 1: colour matrix at `$4000`, bitmap at `$6000`
//! (the load address of Koala pictures). The program and its variables must
//! stay below `$4000`; the backend checks that when this module is used.
//!
//! ```ignore
//! let mut g = gfx::hires(Color::White, Color::Black);
//! g.line(0, 0, 319, 199);
//! g.circle(160, 100, 50);
//! gfx::text();
//! ```

use core::marker::PhantomData;

use crate::hw::{self, Color};
use crate::mem;

/// Address of the bitmap.
pub const BITMAP: u16 = 0x6000;
/// Address of the colour matrix.
pub const MATRIX: u16 = 0x4000;
const COLOR_RAM: u16 = 0xd800;

// The memory the modes use: placed (not loaded), so the backend checks that
// the program ends below it.
#[cfg(target_os = "c64")]
#[unsafe(link_section = "at:$6000")]
static mut BITMAP_MEM: core::mem::MaybeUninit<[u8; 8000]> = core::mem::MaybeUninit::uninit();
#[cfg(target_os = "c64")]
#[unsafe(link_section = "at:$4000")]
static mut MATRIX_MEM: core::mem::MaybeUninit<[u8; 1000]> = core::mem::MaybeUninit::uninit();

#[inline(always)]
fn bitmap() -> *mut u8 {
    #[cfg(target_os = "c64")]
    {
        (&raw mut BITMAP_MEM).cast::<u8>()
    }
    #[cfg(not(target_os = "c64"))]
    mem::addr(BITMAP)
}

#[inline(always)]
fn matrix() -> *mut u8 {
    #[cfg(target_os = "c64")]
    {
        (&raw mut MATRIX_MEM).cast::<u8>()
    }
    #[cfg(not(target_os = "c64"))]
    mem::addr(MATRIX)
}

/// `row * 320`: the bitmap offset of a cell row.
static ROW: [u16; 25] = [
    0, 320, 640, 960, 1280, 1600, 1920, 2240, 2560, 2880, 3200, 3520, 3840, 4160, 4480, 4800, 5120,
    5440, 5760, 6080, 6400, 6720, 7040, 7360, 7680,
];
static HIRES_MASK: [u8; 8] = [0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01];
static MC_MASK: [u8; 4] = [0xc0, 0x30, 0x0c, 0x03];
static MC_SHIFT: [u8; 4] = [6, 4, 2, 0];

/// A bitmap mode.
pub trait Mode {
    /// Width in pixels.
    const WIDTH: i16;
    /// Sets pixel (x, y) (inside the screen) with `pen`.
    fn put(x: u16, y: u8, pen: u8);
}

/// 320x200, pen 1 sets and 0 clears pixels.
pub struct Hires;
/// 160x200, pens 0-3 select the background or one of the cell's colours.
pub struct Multicolor;

impl Mode for Hires {
    const WIDTH: i16 = 320;
    #[inline(always)]
    fn put(x: u16, y: u8, pen: u8) {
        let off = ROW[(y >> 3) as usize] + (x & !7) + (y & 7) as u16;
        let m = HIRES_MASK[(x & 7) as usize];
        unsafe {
            let p = bitmap().add(off as usize);
            *p = if pen != 0 { *p | m } else { *p & !m };
        }
    }
}

impl Mode for Multicolor {
    const WIDTH: i16 = 160;
    #[inline(always)]
    fn put(x: u16, y: u8, pen: u8) {
        let x = x as u8;
        let off = ROW[(y >> 3) as usize] + (x >> 2) as u16 * 8 + (y & 7) as u16;
        let k = (x & 3) as usize;
        unsafe {
            let p = bitmap().add(off as usize);
            *p = *p & !MC_MASK[k] | (pen & 3) << MC_SHIFT[k];
        }
    }
}

/// The bitmap screen in mode `M`.
pub struct Bitmap<M: Mode> {
    /// What [`Bitmap::plot`] and the shapes draw with.
    pub pen: u8,
    _mode: PhantomData<M>,
}

fn bitmap_mode(multicolor: bool) {
    let v = hw::vic();
    hw::set_vic_bank(1);
    v.memory.write(0x08); // matrix at +$0000, bitmap at +$2000
    v.control1.write(v.control1.read() & 0xbf | 0x20);
    if multicolor {
        v.control2.set_bits(0x10);
    } else {
        v.control2.clear_bits(0x10);
    }
}

/// Switches to the hires bitmap with foreground and background colours and
/// clears it.
pub fn hires(fg: Color, bg: Color) -> Bitmap<Hires> {
    let b = Bitmap { pen: 1, _mode: PhantomData };
    b.clear();
    b.set_colors(fg, bg);
    bitmap_mode(false);
    b
}

/// Switches to the multicolour bitmap: pen 0 is the background `bg`, pens
/// 1-3 are `c1`, `c2`, `c3`. Clears it.
pub fn multicolor(bg: Color, c1: Color, c2: Color, c3: Color) -> Bitmap<Multicolor> {
    let b = Bitmap { pen: 1, _mode: PhantomData };
    b.clear();
    fill(matrix(), 1000, (c1 as u8) << 4 | c2 as u8);
    mem::fill(COLOR_RAM, 1000, c3 as u8);
    hw::vic().background[0].write(bg);
    bitmap_mode(true);
    b
}

/// Shows a Koala Painter picture (a `.kla` file with its 2-byte load
/// address, 10003 bytes) in multicolour mode; `None` if it is too short.
pub fn show_koala(pic: &[u8]) -> Option<Bitmap<Multicolor>> {
    if pic.len() < 10003 {
        return None;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(pic.as_ptr().add(2), bitmap(), 8000);
        core::ptr::copy_nonoverlapping(pic.as_ptr().add(8002), matrix(), 1000);
        core::ptr::copy_nonoverlapping(pic.as_ptr().add(9002), mem::addr(COLOR_RAM), 1000);
    }
    hw::vic().background[0].write(Color::from_u8(pic[10002]));
    bitmap_mode(true);
    Some(Bitmap { pen: 1, _mode: PhantomData })
}

/// Returns to the text screen at `$0400`.
pub fn text() {
    let v = hw::vic();
    v.control1.clear_bits(0x20);
    v.control2.clear_bits(0x10);
    hw::set_vic_bank(0);
    v.memory.write(0x15);
}

fn fill(p: *mut u8, n: u16, v: u8) {
    #[cfg(target_os = "c64")]
    mem::fill(p as usize as u16, n, v);
    #[cfg(not(target_os = "c64"))]
    unsafe {
        core::ptr::write_bytes(p, v, n as usize)
    }
}

fn abs(v: i16) -> i16 {
    if v < 0 { -v } else { v }
}

impl<M: Mode> Bitmap<M> {
    /// Clears the bitmap.
    pub fn clear(&self) {
        fill(bitmap(), 8000, 0);
    }

    /// Sets a pixel with the pen; pixels outside the screen are ignored.
    #[inline]
    pub fn plot(&self, x: i16, y: i16) {
        if x < 0 || y < 0 || y >= 200 || x >= M::WIDTH {
            return;
        }
        M::put(x as u16, y as u8, self.pen);
    }

    /// Draws a line (Bresenham).
    pub fn line(&self, mut x0: i16, mut y0: i16, x1: i16, y1: i16) {
        let dx = abs(x1 - x0);
        let dy = -abs(y1 - y0);
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            self.plot(x0, y0);
            if x0 == x1 && y0 == y1 {
                return;
            }
            let e2 = err + err;
            if e2 >= dy {
                err += dy;
                x0 += sx;
            }
            if e2 <= dx {
                err += dx;
                y0 += sy;
            }
        }
    }

    /// Draws the outline of a rectangle.
    pub fn rect(&self, x: i16, y: i16, w: i16, h: i16) {
        self.line(x, y, x + w - 1, y);
        self.line(x, y + h - 1, x + w - 1, y + h - 1);
        self.line(x, y, x, y + h - 1);
        self.line(x + w - 1, y, x + w - 1, y + h - 1);
    }

    /// Fills a rectangle.
    pub fn fill_rect(&self, x: i16, y: i16, w: i16, h: i16) {
        for j in y..y + h {
            self.line(x, j, x + w - 1, j);
        }
    }

    /// Draws a circle (midpoint algorithm).
    pub fn circle(&self, cx: i16, cy: i16, r: i16) {
        let (mut x, mut y) = (r, 0i16);
        let mut err = 1 - r;
        while x >= y {
            self.plot(cx + x, cy + y);
            self.plot(cx + y, cy + x);
            self.plot(cx - y, cy + x);
            self.plot(cx - x, cy + y);
            self.plot(cx - x, cy - y);
            self.plot(cx - y, cy - x);
            self.plot(cx + y, cy - x);
            self.plot(cx + x, cy - y);
            y += 1;
            if err < 0 {
                err += 2 * y + 1;
            } else {
                x -= 1;
                err += 2 * (y - x) + 1;
            }
        }
    }
}

impl Bitmap<Hires> {
    /// Whether a pixel is set.
    pub fn point(&self, x: u16, y: u8) -> bool {
        if x >= 320 || y >= 200 {
            return false;
        }
        let off = ROW[(y >> 3) as usize] + (x & !7) + (y & 7) as u16;
        unsafe { *bitmap().add(off as usize) & HIRES_MASK[(x & 7) as usize] != 0 }
    }

    /// Sets the colours of all cells.
    pub fn set_colors(&self, fg: Color, bg: Color) {
        fill(matrix(), 1000, (fg as u8) << 4 | bg as u8);
    }

    /// Sets the colours of one 8x8 cell (col 0-39, row 0-24).
    pub fn set_cell_colors(&self, col: u8, row: u8, fg: Color, bg: Color) {
        let off = ROW[row as usize] / 8 + col as u16;
        unsafe { *matrix().add(off as usize) = (fg as u8) << 4 | bg as u8 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hires_drawing() {
        let g = Bitmap::<Hires> { pen: 1, _mode: PhantomData };
        g.clear();
        g.line(0, 0, 319, 199);
        assert!(g.point(0, 0) && g.point(319, 199) && g.point(160, 100));
        assert!(!g.point(0, 199));
        g.circle(100, 100, 10);
        assert!(g.point(110, 100) && g.point(100, 90));
    }
}
