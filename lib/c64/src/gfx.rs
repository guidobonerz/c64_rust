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
//! # Double buffering
//!
//! [`Bitmap::double_buffer`] gives a second bitmap (at `$A000`, VIC bank
//! 2): everything is drawn into the bitmap that is not on the screen, and
//! [`Double::flip`] shows it.
//!
//! ```ignore
//! let mut d = gfx::hires(Color::White, Color::Black).double_buffer();
//! loop {
//!     d.clear();
//!     d.line(0, 0, x, 199);
//!     hw::wait_frame();
//!     d.flip();
//! }
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
//! smaller (`examples/drawing`: 1677 instead of 1963 bytes, 2.6% faster),
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

#[cfg(not(target_os = "c64"))]
static HIRES_MASK: [u8; 8] = [0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01];

/// A bitmap mode.
pub trait Mode: Sized {
    /// Width in pixels.
    const WIDTH: i16;
    /// Whether a pixel has two bits (for the stand-ins on the host).
    #[doc(hidden)]
    const MC: bool;
    /// The byte a cell row has when all its pixels are drawn with `pen`.
    #[doc(hidden)]
    fn pattern(pen: u8) -> u8;
    /// Sets pixel (x, y) (inside the screen) with `pen`.
    #[inline]
    fn put(x: u16, y: u8, pen: u8) {
        put::<Self>(x as i16, y as i16, Self::pattern(pen));
    }
    // The assembly routines of the mode (rat/gfx_hires.rat, rat/gfx_mc.rat).
    #[cfg(target_os = "c64")]
    #[doc(hidden)]
    fn asm_put(x: i16, y: i16, pattern: u8);
    #[cfg(target_os = "c64")]
    #[doc(hidden)]
    fn asm_line(x0: i16, y0: i16, x1: i16, y1: i16, pattern: u8);
    #[cfg(target_os = "c64")]
    #[doc(hidden)]
    fn asm_fline(x0: i16, y0: i16, x1: i16, y1: i16, pattern: u8);
    #[cfg(target_os = "c64")]
    #[doc(hidden)]
    fn asm_hline(x0: i16, x1: i16, y: i16, pattern: u8);
    #[cfg(target_os = "c64")]
    #[doc(hidden)]
    fn asm_vline(x: i16, y0: i16, y1: i16, pattern: u8);
    #[cfg(target_os = "c64")]
    #[doc(hidden)]
    fn asm_area(x0: i16, y0: i16, x1: i16, y1: i16, pattern: u8);
    #[cfg(target_os = "c64")]
    #[doc(hidden)]
    fn asm_rect(x: i16, y: i16, r: i16, b: i16, pattern: u8);
    #[cfg(target_os = "c64")]
    #[doc(hidden)]
    fn asm_circle(cx: i16, cy: i16, r: i16, pattern: u8);
}

// Declared here (not inside the methods): the backend defines the argument
// areas of the assembly functions it finds in the modules, and the routines
// pass their arguments on to each other.
#[cfg(target_os = "c64")]
unsafe extern "C" {
    fn c64_gfh_put(x: i16, y: i16, pattern: u8);
    fn c64_gfh_line(x0: i16, y0: i16, x1: i16, y1: i16, pattern: u8);
    fn c64_gfh_fline(x0: i16, y0: i16, x1: i16, y1: i16, pattern: u8);
    fn c64_gfh_hline(x0: i16, x1: i16, y: i16, pattern: u8);
    fn c64_gfh_vline(x: i16, y0: i16, y1: i16, pattern: u8);
    fn c64_gfh_area(x0: i16, y0: i16, x1: i16, y1: i16, pattern: u8);
    fn c64_gfh_rect(x: i16, y: i16, r: i16, b: i16, pattern: u8);
    fn c64_gfh_circle(cx: i16, cy: i16, r: i16, pattern: u8);
    fn c64_gfm_put(x: i16, y: i16, pattern: u8);
    fn c64_gfm_line(x0: i16, y0: i16, x1: i16, y1: i16, pattern: u8);
    fn c64_gfm_fline(x0: i16, y0: i16, x1: i16, y1: i16, pattern: u8);
    fn c64_gfm_hline(x0: i16, x1: i16, y: i16, pattern: u8);
    fn c64_gfm_vline(x: i16, y0: i16, y1: i16, pattern: u8);
    fn c64_gfm_area(x0: i16, y0: i16, x1: i16, y1: i16, pattern: u8);
    fn c64_gfm_rect(x: i16, y: i16, r: i16, b: i16, pattern: u8);
    fn c64_gfm_circle(cx: i16, cy: i16, r: i16, pattern: u8);
    fn c64_gfh_point(x: u16, y: u8) -> u8;
    // double buffering (rat/gfxtab.rat)
    fn c64_gft_double();
    fn c64_gft_single();
    fn c64_gft_flip();
    fn c64_gft_clear();
    fn c64_gft_colors(value: u8);
}

/// The methods of [`Mode`] that call the assembly routines of a mode.
#[cfg(target_os = "c64")]
macro_rules! asm_routines {
    ($put:ident, $line:ident, $fline:ident, $hline:ident, $vline:ident, $area:ident, $rect:ident, $circle:ident) => {
        #[inline(always)]
        fn asm_put(x: i16, y: i16, pattern: u8) {
            unsafe { $put(x, y, pattern) }
        }
        #[inline(always)]
        fn asm_line(x0: i16, y0: i16, x1: i16, y1: i16, pattern: u8) {
            unsafe { $line(x0, y0, x1, y1, pattern) }
        }
        #[inline(always)]
        fn asm_fline(x0: i16, y0: i16, x1: i16, y1: i16, pattern: u8) {
            unsafe { $fline(x0, y0, x1, y1, pattern) }
        }
        #[inline(always)]
        fn asm_hline(x0: i16, x1: i16, y: i16, pattern: u8) {
            unsafe { $hline(x0, x1, y, pattern) }
        }
        #[inline(always)]
        fn asm_vline(x: i16, y0: i16, y1: i16, pattern: u8) {
            unsafe { $vline(x, y0, y1, pattern) }
        }
        #[inline(always)]
        fn asm_area(x0: i16, y0: i16, x1: i16, y1: i16, pattern: u8) {
            unsafe { $area(x0, y0, x1, y1, pattern) }
        }
        #[inline(always)]
        fn asm_rect(x: i16, y: i16, r: i16, b: i16, pattern: u8) {
            unsafe { $rect(x, y, r, b, pattern) }
        }
        #[inline(always)]
        fn asm_circle(cx: i16, cy: i16, r: i16, pattern: u8) {
            unsafe { $circle(cx, cy, r, pattern) }
        }
    };
}

/// 320x200, pen 1 sets and 0 clears pixels.
pub struct Hires;
/// 160x200, pens 0-3 select the background or one of the cell's colours.
pub struct Multicolor;

impl Mode for Hires {
    const WIDTH: i16 = 320;
    const MC: bool = false;
    #[inline]
    fn pattern(pen: u8) -> u8 {
        if pen != 0 { 0xff } else { 0 }
    }
    #[cfg(target_os = "c64")]
    asm_routines!(
        c64_gfh_put,
        c64_gfh_line,
        c64_gfh_fline,
        c64_gfh_hline,
        c64_gfh_vline,
        c64_gfh_area,
        c64_gfh_rect,
        c64_gfh_circle
    );
}

impl Mode for Multicolor {
    const WIDTH: i16 = 160;
    const MC: bool = true;
    #[inline]
    fn pattern(pen: u8) -> u8 {
        (pen & 3) * 0x55
    }
    #[cfg(target_os = "c64")]
    asm_routines!(
        c64_gfm_put,
        c64_gfm_line,
        c64_gfm_fline,
        c64_gfm_hline,
        c64_gfm_vline,
        c64_gfm_area,
        c64_gfm_rect,
        c64_gfm_circle
    );
}

// The drawing routines. On the C64 they are assembly, which is much
// smaller and faster than compiled 16-bit arithmetic: one set of routines
// per mode (rat/gfx_hires.rat: `c64_gfh_...`, rat/gfx_mc.rat:
// `c64_gfm_...`), so that a program only carries the routines of the
// modes it draws in, and no routine asks for the mode at run time. The
// Rust versions here are what the host uses, and they draw the same
// pixels.

#[cfg(target_os = "c64")]
crate::rat_source!(GFX_HIRES_RAT, "rat/gfx_hires.rat");
#[cfg(target_os = "c64")]
crate::rat_source!(GFX_MC_RAT, "rat/gfx_mc.rat");
#[cfg(target_os = "c64")]
crate::rat_source!(GFX_TAB_RAT, "rat/gfxtab.rat");

// The feature `gfx-unclipped`: the definition that switches the checks of
// the assembly off (see rat/gfx_hires.rat).
#[cfg(all(target_os = "c64", feature = "gfx-unclipped"))]
#[doc(hidden)]
#[allow(dead_code)]
#[unsafe(link_section = "rat")]
static GFX_UNCLIPPED_RAT: [u8; 22] = *b"C64_GFX_UNCLIPPED = 1\n";

/// The width of the screen in pixels of the mode.
#[cfg(not(target_os = "c64"))]
fn width(mc: bool) -> i16 {
    if mc { 160 } else { 320 }
}

/// One pixel; nothing outside the screen.
#[inline(always)]
fn put<M: Mode>(x: i16, y: i16, pattern: u8) {
    #[cfg(target_os = "c64")]
    M::asm_put(x, y, pattern);
    #[cfg(not(target_os = "c64"))]
    {
        let mc = M::MC;
        if x < 0 || y < 0 || y >= 200 || x >= width(mc) {
            return;
        }
        // The position in bits, and the bits of the pixel.
        let (bit, mask) = if mc {
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
fn line<M: Mode>(x0: i16, y0: i16, x1: i16, y1: i16, pattern: u8) {
    #[cfg(target_os = "c64")]
    M::asm_line(x0, y0, x1, y1, pattern);
    #[cfg(not(target_os = "c64"))]
    {
        // Horizontal and vertical lines are areas one pixel high or wide.
        if y0 == y1 || x0 == x1 {
            let (l, r) = if x0 < x1 { (x0, x1) } else { (x1, x0) };
            let (t, b) = if y0 < y1 { (y0, y1) } else { (y1, y0) };
            area::<M>(l, t, r, b, pattern);
            return;
        }
        let (mut x0, mut y0) = (x0, y0);
        let dx = abs(x1 - x0);
        let dy = -abs(y1 - y0);
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            put::<M>(x0, y0, pattern);
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
fn area<M: Mode>(x0: i16, y0: i16, x1: i16, y1: i16, pattern: u8) {
    #[cfg(target_os = "c64")]
    M::asm_area(x0, y0, x1, y1, pattern);
    #[cfg(not(target_os = "c64"))]
    {
        let mc = M::MC;
        let x0 = if x0 < 0 { 0 } else { x0 };
        let y0 = if y0 < 0 { 0 } else { y0 };
        let x1 = if x1 >= width(mc) { width(mc) - 1 } else { x1 };
        let y1 = if y1 >= 200 { 199 } else { y1 };
        for j in y0..=y1 {
            for i in x0..=x1 {
                put::<M>(i, j, pattern);
            }
        }
    }
}

/// The outline of the rectangle with the corners (x, y) and (r, b);
/// nothing if r < x or b < y.
#[inline(always)]
fn rect<M: Mode>(x: i16, y: i16, r: i16, b: i16, pattern: u8) {
    #[cfg(target_os = "c64")]
    M::asm_rect(x, y, r, b, pattern);
    #[cfg(not(target_os = "c64"))]
    {
        if r < x || b < y {
            return;
        }
        area::<M>(x, y, r, y, pattern);
        area::<M>(x, b, r, b, pattern);
        area::<M>(x, y, x, b, pattern);
        area::<M>(r, y, r, b, pattern);
    }
}

/// A circle (midpoint algorithm).
#[inline(always)]
fn circle<M: Mode>(cx: i16, cy: i16, r: i16, pattern: u8) {
    #[cfg(target_os = "c64")]
    M::asm_circle(cx, cy, r, pattern);
    #[cfg(not(target_os = "c64"))]
    {
        let (mut x, mut y) = (r, 0i16);
        let mut err = 1 - r;
        while x >= y {
            put::<M>(cx + x, cy + y, pattern);
            put::<M>(cx + y, cy + x, pattern);
            put::<M>(cx - y, cy + x, pattern);
            put::<M>(cx - x, cy + y, pattern);
            put::<M>(cx - x, cy - y, pattern);
            put::<M>(cx - y, cy - x, pattern);
            put::<M>(cx + y, cy - x, pattern);
            put::<M>(cx + x, cy - y, pattern);
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
        put::<M>(x, y, M::pattern(self.pen));
    }

    /// Draws a line (Bresenham); the parts outside the screen are left
    /// out. Horizontal and vertical lines are the fastest.
    pub fn line(&self, x0: i16, y0: i16, x1: i16, y1: i16) {
        line::<M>(x0, y0, x1, y1, M::pattern(self.pen));
    }

    /// Draws the same line as [`Bitmap::line`], the fast way: a flat line
    /// (at least 8 pixels across for each line down, both ends on the
    /// screen) is stored in horizontal runs, whole bytes at a time, about
    /// three times as fast. The price is some 300 bytes of code in the
    /// program.
    pub fn fline(&self, x0: i16, y0: i16, x1: i16, y1: i16) {
        #[cfg(target_os = "c64")]
        M::asm_fline(x0, y0, x1, y1, M::pattern(self.pen));
        #[cfg(not(target_os = "c64"))]
        line::<M>(x0, y0, x1, y1, M::pattern(self.pen));
    }

    /// Draws the horizontal line from `x0` to `x1` (in any order) at `y`:
    /// whole bytes at a time, and without the code of [`Bitmap::line`].
    pub fn hline(&self, x0: i16, x1: i16, y: i16) {
        #[cfg(target_os = "c64")]
        M::asm_hline(x0, x1, y, M::pattern(self.pen));
        #[cfg(not(target_os = "c64"))]
        {
            let (l, r) = if x0 < x1 { (x0, x1) } else { (x1, x0) };
            area::<M>(l, y, r, y, M::pattern(self.pen));
        }
    }

    /// Draws the vertical line from `y0` to `y1` (in any order) at `x`,
    /// going down one column, and without the code of [`Bitmap::line`].
    pub fn vline(&self, x: i16, y0: i16, y1: i16) {
        #[cfg(target_os = "c64")]
        M::asm_vline(x, y0, y1, M::pattern(self.pen));
        #[cfg(not(target_os = "c64"))]
        {
            let (t, b) = if y0 < y1 { (y0, y1) } else { (y1, y0) };
            area::<M>(x, t, x, b, M::pattern(self.pen));
        }
    }

    /// Draws the outline of a rectangle (nothing if `w` or `h` is not
    /// positive).
    pub fn rect(&self, x: i16, y: i16, w: i16, h: i16) {
        rect::<M>(x, y, x + w - 1, y + h - 1, M::pattern(self.pen));
    }

    /// Fills a rectangle.
    pub fn fill_rect(&self, x: i16, y: i16, w: i16, h: i16) {
        area::<M>(x, y, x + w - 1, y + h - 1, M::pattern(self.pen));
    }

    /// Draws a circle (midpoint algorithm).
    pub fn circle(&self, cx: i16, cy: i16, r: i16) {
        circle::<M>(cx, cy, r, M::pattern(self.pen));
    }
}

impl Bitmap<Hires> {
    /// Whether a pixel is set.
    pub fn point(&self, x: u16, y: u8) -> bool {
        if x >= 320 || y >= 200 {
            return false;
        }
        #[cfg(target_os = "c64")]
        unsafe {
            c64_gfh_point(x, y) != 0
        }
        #[cfg(not(target_os = "c64"))]
        {
            let off = (y >> 3) as u16 * 320 + (x & !7) + (y & 7) as u16;
            unsafe { *bitmap().add(off as usize) & HIRES_MASK[(x & 7) as usize] != 0 }
        }
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

/// Address of the second bitmap of a [`Double`] (RAM under the BASIC ROM,
/// which is switched off while there are two bitmaps).
pub const BITMAP2: u16 = 0xa000;
/// Address of its colour matrix.
pub const MATRIX2: u16 = 0x8000;

impl<M: Mode> Bitmap<M> {
    /// Starts double buffering: there are two bitmaps now, the one on the
    /// screen and a hidden one, and everything is drawn into the hidden
    /// one until [`Double::flip`] shows it. A picture that moves is then
    /// never seen half drawn.
    ///
    /// The second bitmap is at `$A000` with its colour matrix at `$8000`
    /// (VIC bank 2); it starts cleared, with the colours of the first.
    /// `$A000` is under the BASIC ROM, which is switched off until the
    /// double buffering ends (the program does not need it; the KERNAL
    /// stays). Sprites need their pointers in both matrices (`$43F8`,
    /// `$83F8`).
    pub fn double_buffer(self) -> Double<M> {
        #[cfg(target_os = "c64")]
        unsafe {
            c64_gft_double()
        }
        Double { g: self }
    }
}

/// A [`Bitmap`] with double buffering ([`Bitmap::double_buffer`]): it
/// draws like the bitmap (all its methods and the pen are there), into the
/// hidden one of two. Dropping it shows the first bitmap and draws there
/// again.
///
/// The two bitmaps take turns, so the hidden one holds the picture before
/// the last: clear it for every picture ([`Double::clear`], five frames),
/// or keep what was drawn into each of the two and take that away again
/// (`examples/cube3d`).
///
/// On the host there is one bitmap, and `flip` does nothing.
pub struct Double<M: Mode> {
    g: Bitmap<M>,
}

impl<M: Mode> Double<M> {
    /// Shows what was drawn; the bitmap that was on the screen is the one
    /// that is drawn into now. It is one store to a register; call it
    /// after [`hw::wait_frame`] for a change between two frames.
    pub fn flip(&mut self) {
        #[cfg(target_os = "c64")]
        unsafe {
            c64_gft_flip()
        }
    }

    /// Clears the hidden bitmap.
    pub fn clear(&self) {
        #[cfg(target_os = "c64")]
        unsafe {
            c64_gft_clear()
        }
        #[cfg(not(target_os = "c64"))]
        self.g.clear();
    }

    /// Ends double buffering: the first bitmap is on the screen and is
    /// drawn into.
    pub fn single_buffer(self) -> Bitmap<M> {
        let g = Bitmap { pen: self.g.pen, _mode: PhantomData };
        drop(self);
        g
    }
}

impl Double<Hires> {
    /// Sets the colours of all cells of the hidden bitmap.
    pub fn set_colors(&self, fg: Color, bg: Color) {
        #[cfg(target_os = "c64")]
        unsafe {
            c64_gft_colors((fg as u8) << 4 | bg as u8)
        }
        #[cfg(not(target_os = "c64"))]
        self.g.set_colors(fg, bg);
    }

    /// Sets the colours of one 8x8 cell (col 0-39, row 0-24) of the hidden
    /// bitmap.
    pub fn set_cell_colors(&self, col: u8, row: u8, fg: Color, bg: Color) {
        #[cfg(target_os = "c64")]
        {
            // which of the two is drawn into (rat/gfxtab.rat)
            let second: u8;
            unsafe { crate::asm!("lda c64_gft_back", "sta {s}", s = out(reg) second) };
            let matrix = if second != 0 { MATRIX2 } else { MATRIX };
            mem::poke(matrix + row as u16 * 40 + col as u16, (fg as u8) << 4 | bg as u8);
        }
        #[cfg(not(target_os = "c64"))]
        self.g.set_cell_colors(col, row, fg, bg);
    }
}

impl<M: Mode> core::ops::Deref for Double<M> {
    type Target = Bitmap<M>;

    fn deref(&self) -> &Bitmap<M> {
        &self.g
    }
}

impl<M: Mode> core::ops::DerefMut for Double<M> {
    fn deref_mut(&mut self) -> &mut Bitmap<M> {
        &mut self.g
    }
}

impl<M: Mode> Drop for Double<M> {
    fn drop(&mut self) {
        #[cfg(target_os = "c64")]
        unsafe {
            c64_gft_single()
        }
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
