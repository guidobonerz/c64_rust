//! The eight hardware sprites.
//!
//! Sprite images are 63 bytes (24x21 pixels, or 12x21 in multicolour) in a
//! 64-byte block of the VIC bank: image `n` is at address `n * 64` in bank
//! 0. [`load`] copies image data there, [`Sprite::set_image`] points a
//! sprite at it.
//!
//! ```ignore
//! sprite::load(13, &BALL);
//! let s = Sprite(0);
//! s.set_image(13);
//! s.set_color(Color::Yellow);
//! s.move_to(160, 120);
//! s.show();
//! ```

use crate::hw::{self, Color};
use crate::mem;

#[unsafe(export_name = "c64_sprite_bits")]
static BIT: [u8; 8] = [1, 2, 4, 8, 16, 32, 64, 128];

#[cfg(target_os = "c64")]
crate::rat_source!(SPRITE_RAT, "rat/sprite.rat");

#[cfg(target_os = "c64")]
unsafe extern "C" {
    fn c64_sprite_move(n: u8, x: u16, y: u8);
    fn c64_sprite_flag(reg: u8, n: u8, on: u8);
    fn c64_sprite_x(n: u8) -> u16;
}

// The VIC registers with one bit per sprite (offsets from $D000).
const ENABLE: u8 = 0x15;
const EXPAND_Y: u8 = 0x17;
const PRIORITY: u8 = 0x1b;
const MULTICOLOR: u8 = 0x1c;
const EXPAND_X: u8 = 0x1d;

static mut POINTERS: u16 = 0x07f8;

/// Copies a sprite image (up to 64 bytes) to `block` (address `block * 64`
/// in VIC bank 0).
pub fn load(block: u8, data: &[u8]) {
    let n = if data.len() > 64 { 64 } else { data.len() as u16 };
    #[cfg(target_os = "c64")]
    mem::copy(block as u16 * 64, data.as_ptr() as u16, n);
    #[cfg(not(target_os = "c64"))]
    for i in 0..n {
        mem::poke(block as u16 * 64 + i, data[i as usize]);
    }
}

/// Sets where the sprite pointers are: screen address + `$3F8` (call it
/// after moving the screen with [`crate::text::set_screen_addr`]).
pub fn set_pointer_addr(addr: u16) {
    unsafe { POINTERS = addr };
}

/// Disables all sprites.
pub fn hide_all() {
    hw::vic().sprite_enable.write(0);
}

/// Sets the two colours that all multicolour sprites share.
pub fn shared_colors(c1: Color, c2: Color) {
    let v = hw::vic();
    v.sprite_mc[0].write(c1);
    v.sprite_mc[1].write(c2);
}

/// The sprite-sprite collision bits (reading clears them).
pub fn collisions() -> u8 {
    hw::vic().sprite_collision.read()
}

/// The sprite-background collision bits (reading clears them).
pub fn bg_collisions() -> u8 {
    hw::vic().sprite_bg_collision.read()
}

/// Sprite 0-7.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sprite(pub u8);

impl Sprite {
    fn bit(self) -> u8 {
        BIT[(self.0 & 7) as usize]
    }

    /// Sets or clears the sprite's bit in the VIC register `$D000 + reg`.
    fn flag(self, reg: u8, on: bool) {
        #[cfg(target_os = "c64")]
        unsafe {
            c64_sprite_flag(reg, self.0, on as u8)
        }
        #[cfg(not(target_os = "c64"))]
        {
            let a = 0xd000 + reg as u16;
            let v = mem::peek(a);
            mem::poke(a, if on { v | self.bit() } else { v & !self.bit() });
        }
    }

    /// Selects the image block.
    pub fn set_image(self, block: u8) {
        mem::poke(unsafe { POINTERS } + (self.0 & 7) as u16, block);
    }

    pub fn show(self) {
        self.flag(ENABLE, true);
    }

    pub fn hide(self) {
        self.flag(ENABLE, false);
    }

    /// Places the sprite at `x` (0-511) and `y` (0-255). The visible screen
    /// starts at x = 24, y = 50.
    pub fn move_to(self, x: u16, y: u8) {
        #[cfg(target_os = "c64")]
        unsafe {
            c64_sprite_move(self.0, x, y)
        }
        #[cfg(not(target_os = "c64"))]
        {
            let v = hw::vic();
            let s = &v.sprite[(self.0 & 7) as usize];
            s.x.write(x as u8);
            s.y.write(y);
            self.flag(0x10, x > 255);
        }
    }

    pub fn x(self) -> u16 {
        #[cfg(target_os = "c64")]
        unsafe {
            c64_sprite_x(self.0)
        }
        #[cfg(not(target_os = "c64"))]
        {
            let v = hw::vic();
            let x = v.sprite[(self.0 & 7) as usize].x.read() as u16;
            if v.sprite_x_msb.read() & self.bit() != 0 { x + 256 } else { x }
        }
    }

    pub fn y(self) -> u8 {
        hw::vic().sprite[(self.0 & 7) as usize].y.read()
    }

    pub fn set_color(self, c: Color) {
        hw::vic().sprite_color[(self.0 & 7) as usize].write(c);
    }

    /// Switches between hires and multicolour.
    pub fn set_multicolor(self, on: bool) {
        self.flag(MULTICOLOR, on);
    }

    /// Doubles the width and/or the height.
    pub fn expand(self, x: bool, y: bool) {
        self.flag(EXPAND_X, x);
        self.flag(EXPAND_Y, y);
    }

    /// Puts the sprite behind (true) or in front of the background.
    pub fn behind(self, on: bool) {
        self.flag(PRIORITY, on);
    }

    /// Whether this sprite is set in a collision mask ([`collisions`],
    /// [`bg_collisions`]).
    pub fn hit(self, mask: u8) -> bool {
        mask & self.bit() != 0
    }
}
