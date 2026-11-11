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
//!
//! # Clipping
//!
//! The drawing methods leave out what is outside the screen: a line may
//! start at `(-50, 100)`. A program that never draws outside can switch the
//! checks off with the feature `gfx-unclipped`, for the whole program:
//!
//! ```toml
//! c64 = { path = "...", features = ["gfx-unclipped"] }
//! ```
//!
//! Drawing is then faster (most for pixels and circles) and the program
//! smaller (`examples/drawing`: 2275 instead of 2663 bytes, 2.5% faster),
//! but **every pixel must be on the screen**: one
//! outside is written to other memory, which can destroy the program. Empty
//! rectangles (`w` or `h` not positive) still draw nothing, and
//! [`Bitmap::point`] still answers `false` outside the screen. The feature
//! only changes programs for the C64; on the host the stand-ins always
//! clip.

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

static HIRES_MASK: [u8; 8] = [0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01];

/// A bitmap mode.
pub trait Mode {
    /// Width in pixels.
    const WIDTH: i16;
    /// What the drawing routines get as the mode: 0, or `0x80` for two bits
    /// per pixel.
    #[doc(hidden)]
    const MC: u8;
    /// The byte a cell row has when all its pixels are drawn with `pen`.
    #[doc(hidden)]
    fn pattern(pen: u8) -> u8;
    /// Sets pixel (x, y) (inside the screen) with `pen`.
    #[inline]
    fn put(x: u16, y: u8, pen: u8) {
        put(x as i16, y as i16, Self::pattern(pen), Self::MC);
    }
}

/// 320x200, pen 1 sets and 0 clears pixels.
pub struct Hires;
/// 160x200, pens 0-3 select the background or one of the cell's colours.
pub struct Multicolor;

impl Mode for Hires {
    const WIDTH: i16 = 320;
    const MC: u8 = 0;
    #[inline]
    fn pattern(pen: u8) -> u8 {
        if pen != 0 { 0xff } else { 0 }
    }
}

impl Mode for Multicolor {
    const WIDTH: i16 = 160;
    const MC: u8 = 0x80;
    #[inline]
    fn pattern(pen: u8) -> u8 {
        (pen & 3) * 0x55
    }
}

// The drawing routines. They are not generic: both modes share them, with
// the pattern and the mode (`Mode::MC`) as arguments. On the C64 they are
// assembly (rat/gfx.rat), which is much smaller and faster than compiled
// 16-bit arithmetic; the Rust versions here are what the host uses, and
// they draw the same pixels.

#[cfg(all(target_os = "c64", not(feature = "gfx-unclipped")))]
crate::rat_source!(GFX_RAT, "rat/gfx.rat");

// The feature `gfx-unclipped`: the same assembly behind the definition
// that switches its checks off (see rat/gfx.rat).
#[cfg(all(target_os = "c64", feature = "gfx-unclipped"))]
mod unclipped {
    const DEFINE: &[u8] = b"C64_GFX_UNCLIPPED = 1\n";
    const SOURCE: &[u8] = include_bytes!("rat/gfx.rat");

    const fn joined() -> [u8; DEFINE.len() + SOURCE.len()] {
        let mut out = [0; DEFINE.len() + SOURCE.len()];
        let mut i = 0;
        while i < DEFINE.len() {
            out[i] = DEFINE[i];
            i += 1;
        }
        while i < out.len() {
            out[i] = SOURCE[i - DEFINE.len()];
            i += 1;
        }
        out
    }

    #[doc(hidden)]
    #[allow(dead_code)]
    #[unsafe(link_section = "rat")]
    pub static GFX_RAT: [u8; DEFINE.len() + SOURCE.len()] = joined();
}

#[cfg(target_os = "c64")]
unsafe extern "C" {
    fn c64_gfx_put(x: i16, y: i16, pattern: u8, mode: u8);
    fn c64_gfx_line(x0: i16, y0: i16, x1: i16, y1: i16, pattern: u8, mode: u8);
    fn c64_gfx_fline(x0: i16, y0: i16, x1: i16, y1: i16, pattern: u8, mode: u8);
    fn c64_gfx_hline(x0: i16, x1: i16, y: i16, pattern: u8, mode: u8);
    fn c64_gfx_vline(x: i16, y0: i16, y1: i16, pattern: u8, mode: u8);
    fn c64_gfx_area(x0: i16, y0: i16, x1: i16, y1: i16, pattern: u8, mode: u8);
    fn c64_gfx_rect(x: i16, y: i16, r: i16, b: i16, pattern: u8, mode: u8);
    fn c64_gfx_circle(cx: i16, cy: i16, r: i16, pattern: u8, mode: u8);
}

/// The width of the screen in pixels of the mode.
#[cfg(not(target_os = "c64"))]
fn width(mc: u8) -> i16 {
    if mc != 0 { 160 } else { 320 }
}

/// One pixel; nothing outside the screen.
#[inline(always)]
fn put(x: i16, y: i16, pattern: u8, mc: u8) {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_gfx_put(x, y, pattern, mc)
    }
    #[cfg(not(target_os = "c64"))]
    {
        if x < 0 || y < 0 || y >= 200 || x >= width(mc) {
            return;
        }
        // The position in bits, and the bits of the pixel.
        let (bit, mask) = if mc != 0 {
            let bit = x as u16 * 2;
            (bit, 0xc0u8 >> (bit & 7))
        } else {
            (x as u16, 0x80u8 >> (x & 7))
        };
        let off = (y as u16 >> 3) * 320 + (bit & !7) + (y as u16 & 7);
        unsafe {
            let p = bitmap().add(off as usize);
            *p = *p & !mask | pattern & mask;
        }
    }
}

/// A line (Bresenham), both ends included.
#[inline(always)]
fn line(x0: i16, y0: i16, x1: i16, y1: i16, pattern: u8, mc: u8) {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_gfx_line(x0, y0, x1, y1, pattern, mc)
    }
    #[cfg(not(target_os = "c64"))]
    {
        // Horizontal and vertical lines are areas one pixel high or wide.
        if y0 == y1 || x0 == x1 {
            let (l, r) = if x0 < x1 { (x0, x1) } else { (x1, x0) };
            let (t, b) = if y0 < y1 { (y0, y1) } else { (y1, y0) };
            area(l, t, r, b, pattern, mc);
            return;
        }
        let (mut x0, mut y0) = (x0, y0);
        let dx = abs(x1 - x0);
        let dy = -abs(y1 - y0);
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            put(x0, y0, pattern, mc);
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
}

/// All pixels from (x0, y0) to (x1, y1), both included, cut to the screen;
/// nothing if x1 < x0 or y1 < y0.
#[inline(always)]
fn area(x0: i16, y0: i16, x1: i16, y1: i16, pattern: u8, mc: u8) {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_gfx_area(x0, y0, x1, y1, pattern, mc)
    }
    #[cfg(not(target_os = "c64"))]
    {
        let x0 = if x0 < 0 { 0 } else { x0 };
        let y0 = if y0 < 0 { 0 } else { y0 };
        let x1 = if x1 >= width(mc) { width(mc) - 1 } else { x1 };
        let y1 = if y1 >= 200 { 199 } else { y1 };
        for j in y0..=y1 {
            for i in x0..=x1 {
                put(i, j, pattern, mc);
            }
        }
    }
}

/// The outline of the rectangle with the corners (x, y) and (r, b);
/// nothing if r < x or b < y.
#[inline(always)]
fn rect(x: i16, y: i16, r: i16, b: i16, pattern: u8, mc: u8) {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_gfx_rect(x, y, r, b, pattern, mc)
    }
    #[cfg(not(target_os = "c64"))]
    {
        if r < x || b < y {
            return;
        }
        area(x, y, r, y, pattern, mc);
        area(x, b, r, b, pattern, mc);
        area(x, y, x, b, pattern, mc);
        area(r, y, r, b, pattern, mc);
    }
}

/// A circle (midpoint algorithm).
#[inline(always)]
fn circle(cx: i16, cy: i16, r: i16, pattern: u8, mc: u8) {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_gfx_circle(cx, cy, r, pattern, mc)
    }
    #[cfg(not(target_os = "c64"))]
    {
        let (mut x, mut y) = (r, 0i16);
        let mut err = 1 - r;
        while x >= y {
            put(cx + x, cy + y, pattern, mc);
            put(cx + y, cy + x, pattern, mc);
            put(cx - y, cy + x, pattern, mc);
            put(cx - x, cy + y, pattern, mc);
            put(cx - x, cy - y, pattern, mc);
            put(cx - y, cy - x, pattern, mc);
            put(cx + y, cy - x, pattern, mc);
            put(cx + x, cy - y, pattern, mc);
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

#[cfg(not(target_os = "c64"))]
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
        put(x, y, M::pattern(self.pen), M::MC);
    }

    /// Draws a line (Bresenham); the parts outside the screen are left
    /// out. Horizontal and vertical lines are the fastest.
    pub fn line(&self, x0: i16, y0: i16, x1: i16, y1: i16) {
        line(x0, y0, x1, y1, M::pattern(self.pen), M::MC);
    }

    /// Draws the same line as [`Bitmap::line`], the fast way: a flat line
    /// (at least 8 pixels across for each line down, both ends on the
    /// screen) is stored in horizontal runs, whole bytes at a time, about
    /// three times as fast. The price is some 300 bytes of code in the
    /// program.
    pub fn fline(&self, x0: i16, y0: i16, x1: i16, y1: i16) {
        #[cfg(target_os = "c64")]
        unsafe {
            c64_gfx_fline(x0, y0, x1, y1, M::pattern(self.pen), M::MC)
        }
        #[cfg(not(target_os = "c64"))]
        line(x0, y0, x1, y1, M::pattern(self.pen), M::MC);
    }

    /// Draws the horizontal line from `x0` to `x1` (in any order) at `y`:
    /// whole bytes at a time, and without the code of [`Bitmap::line`].
    pub fn hline(&self, x0: i16, x1: i16, y: i16) {
        #[cfg(target_os = "c64")]
        unsafe {
            c64_gfx_hline(x0, x1, y, M::pattern(self.pen), M::MC)
        }
        #[cfg(not(target_os = "c64"))]
        {
            let (l, r) = if x0 < x1 { (x0, x1) } else { (x1, x0) };
            area(l, y, r, y, M::pattern(self.pen), M::MC);
        }
    }

    /// Draws the vertical line from `y0` to `y1` (in any order) at `x`,
    /// going down one column, and without the code of [`Bitmap::line`].
    pub fn vline(&self, x: i16, y0: i16, y1: i16) {
        #[cfg(target_os = "c64")]
        unsafe {
            c64_gfx_vline(x, y0, y1, M::pattern(self.pen), M::MC)
        }
        #[cfg(not(target_os = "c64"))]
        {
            let (t, b) = if y0 < y1 { (y0, y1) } else { (y1, y0) };
            area(x, t, x, b, M::pattern(self.pen), M::MC);
        }
    }

    /// Draws the outline of a rectangle (nothing if `w` or `h` is not
    /// positive).
    pub fn rect(&self, x: i16, y: i16, w: i16, h: i16) {
        rect(x, y, x + w - 1, y + h - 1, M::pattern(self.pen), M::MC);
    }

    /// Fills a rectangle.
    pub fn fill_rect(&self, x: i16, y: i16, w: i16, h: i16) {
        area(x, y, x + w - 1, y + h - 1, M::pattern(self.pen), M::MC);
    }

    /// Draws a circle (midpoint algorithm).
    pub fn circle(&self, cx: i16, cy: i16, r: i16) {
        circle(cx, cy, r, M::pattern(self.pen), M::MC);
    }
}

impl Bitmap<Hires> {
    /// Whether a pixel is set.
    pub fn point(&self, x: u16, y: u8) -> bool {
        if x >= 320 || y >= 200 {
            return false;
        }
        let off = (y >> 3) as u16 * 320 + (x & !7) + (y & 7) as u16;
        unsafe { *bitmap().add(off as usize) & HIRES_MASK[(x & 7) as usize] != 0 }
    }

    /// Sets the colours of all cells.
    pub fn set_colors(&self, fg: Color, bg: Color) {
        fill(matrix(), 1000, (fg as u8) << 4 | bg as u8);
    }

    /// Sets the colours of one 8x8 cell (col 0-39, row 0-24).
    pub fn set_cell_colors(&self, col: u8, row: u8, fg: Color, bg: Color) {
        let off = row as u16 * 40 + col as u16;
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
        g.hline(30, 20, 150);
        g.vline(40, 150, 140);
        assert!(g.point(40, 140) && g.point(40, 150) && !g.point(40, 139) && !g.point(40, 151));
        g.fline(0, 160, 31, 162);
        assert!(g.point(20, 150) && g.point(30, 150) && !g.point(31, 150) && !g.point(19, 150));
        assert!(g.point(7, 160) && g.point(8, 161) && g.point(31, 162));
    }
}
