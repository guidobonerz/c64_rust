//! Joysticks and keyboard.

use crate::hw;
use crate::kernal;

/// A joystick port.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Port {
    One,
    Two,
}

/// The state of a joystick: directions and fire button.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Joy(pub u8);

impl Joy {
    pub const UP: u8 = 1;
    pub const DOWN: u8 = 2;
    pub const LEFT: u8 = 4;
    pub const RIGHT: u8 = 8;
    pub const FIRE: u8 = 16;

    pub fn up(self) -> bool {
        self.0 & Self::UP != 0
    }
    pub fn down(self) -> bool {
        self.0 & Self::DOWN != 0
    }
    pub fn left(self) -> bool {
        self.0 & Self::LEFT != 0
    }
    pub fn right(self) -> bool {
        self.0 & Self::RIGHT != 0
    }
    pub fn fire(self) -> bool {
        self.0 & Self::FIRE != 0
    }
    /// Nothing pressed.
    pub fn idle(self) -> bool {
        self.0 == 0
    }
}

/// Reads a joystick (port 1 is CIA 1 port B, port 2 port A).
pub fn joy(port: Port) -> Joy {
    let c = hw::cia1();
    let v = match port {
        Port::One => c.prb.read(),
        Port::Two => c.pra.read(),
    };
    Joy(!v & 0x1f)
}

/// The next key from the keyboard buffer (PETSCII), `None` if there is none.
pub fn key() -> Option<u8> {
    match kernal::getin() {
        0 => None,
        k => Some(k),
    }
}

/// Waits for a key and returns it.
pub fn wait_key() -> u8 {
    loop {
        if let Some(k) = key() {
            return k;
        }
    }
}

/// Whether the key at a keyboard matrix position is held (`row` = CIA 1
/// port A bit, `col` = port B bit). Do not use it while the KERNAL scans
/// the keyboard in its interrupt with other columns selected.
pub fn pressed(row: u8, col: u8) -> bool {
    let c = hw::cia1();
    c.pra.write(!(1u8 << (row & 7)));
    c.prb.read() & (1u8 << (col & 7)) == 0
}
