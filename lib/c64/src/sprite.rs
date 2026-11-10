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

static BIT: [u8; 8] = [1, 2, 4, 8, 16, 32, 64, 128];

static mut POINTERS: u16 = 0x07f8;

/// Copies a sprite image (up to 64 bytes) to `block` (address `block * 64`
/// in VIC bank 0).
pub fn load(block: u8, data: &[u8]) {
    let mut a = block as u16 * 64;
    for &b in data.iter().take(64) {
        mem::poke(a, b);
        a += 1;
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

    fn set(reg: &hw::Volatile<u8>, bit: u8, on: bool) {
        if on {
            reg.set_bits(bit);
        } else {
            reg.clear_bits(bit);
        }
    }

    /// Selects the image block.
    pub fn set_image(self, block: u8) {
        mem::poke(unsafe { POINTERS } + (self.0 & 7) as u16, block);
    }

    pub fn show(self) {
        hw::vic().sprite_enable.set_bits(self.bit());
    }

    pub fn hide(self) {
        hw::vic().sprite_enable.clear_bits(self.bit());
    }

    /// Places the sprite at `x` (0-511) and `y` (0-255). The visible screen
    /// starts at x = 24, y = 50.
    pub fn move_to(self, x: u16, y: u8) {
        let v = hw::vic();
        let s = &v.sprite[(self.0 & 7) as usize];
        s.x.write(x as u8);
        s.y.write(y);
        Self::set(&v.sprite_x_msb, self.bit(), x > 255);
    }

    pub fn x(self) -> u16 {
        let v = hw::vic();
        let x = v.sprite[(self.0 & 7) as usize].x.read() as u16;
        if v.sprite_x_msb.read() & self.bit() != 0 { x + 256 } else { x }
    }

    pub fn y(self) -> u8 {
        hw::vic().sprite[(self.0 & 7) as usize].y.read()
    }

    pub fn set_color(self, c: Color) {
        hw::vic().sprite_color[(self.0 & 7) as usize].write(c);
    }

    /// Switches between hires and multicolour.
    pub fn set_multicolor(self, on: bool) {
        Self::set(&hw::vic().sprite_multicolor, self.bit(), on);
    }

    /// Doubles the width and/or the height.
    pub fn expand(self, x: bool, y: bool) {
        let v = hw::vic();
        Self::set(&v.sprite_expand_x, self.bit(), x);
        Self::set(&v.sprite_expand_y, self.bit(), y);
    }

    /// Puts the sprite behind (true) or in front of the background.
    pub fn behind(self, on: bool) {
        Self::set(&hw::vic().sprite_priority, self.bit(), on);
    }

    /// Whether this sprite is set in a collision mask ([`collisions`],
    /// [`bg_collisions`]).
    pub fn hit(self, mask: u8) -> bool {
        mask & self.bit() != 0
    }
}
