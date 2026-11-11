//! SD2IEC: the commands that a 1541 does not have. Subdirectories, disk
//! images, and the settings of the device, all sent on the command channel
//! of the drive.
//!
//! ```ignore
//! use c64_sd2iec as sd;
//!
//! if sd::detect(8) {
//!     sd::cd(8, "games")?;
//!     sd::mount(8, "disk1.d64")?;           // the image is now the disk
//!     let end = c64::dos::load("intro", 8, 0xc000)?;
//!     sd::unmount(8)?;
//!     sd::cd_root(8)?;
//! }
//! ```
//!
//! Files are read and written with [`c64::dos`], as on any drive. Names are
//! ASCII and sent as capitals, like there. A function returns `Ok` when the
//! command was sent; what the device thought of it is in its status
//! ([`status`], or [`check`] for the error number).
#![no_std]

use c64::dos;
use c64::fmt::{Buf, Write};
use c64::kernal::Error;

/// How disk images appear in directories ([`images_as`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Images {
    /// As files (the default).
    Files = 0,
    /// As directories.
    Dirs = 1,
    /// As both.
    Both = 2,
}

/// Sends the command made of `parts`.
fn send(dev: u8, parts: &[&str]) -> Result<(), Error> {
    let mut b = Buf::<40>::new();
    for p in parts {
        b.write_str(p);
    }
    // The parts are strings and the buffer only drops whole bytes.
    dos::command(dev, unsafe { core::str::from_utf8_unchecked(b.as_bytes()) })
}

/// Whether device `dev` is an SD2IEC: it answers `UI` with its name.
pub fn detect(dev: u8) -> bool {
    if dos::command(dev, "UI").is_err() {
        return false;
    }
    let mut s = [0u8; 40];
    let Some((n, _)) = dos::status(dev, &mut s) else {
        return false;
    };
    let mut i = 0;
    while i + 6 <= n {
        if &s[i..i + 6] == b"SD2IEC" {
            return true;
        }
        i += 1;
    }
    false
}

/// Reads the status of the last command (`"00, OK,00,00"`) into `buf` and
/// returns its length and the error number; see [`c64::dos::status`].
pub fn status(dev: u8, buf: &mut [u8]) -> Option<(usize, u8)> {
    dos::status(dev, buf)
}

/// The error number of the last command: 0-19 are no errors (`Ok`);
/// 39 = path not found, 62 = file not found, 63 = file exists, ...
/// `Err(255)` if the device does not answer.
pub fn check(dev: u8) -> Result<u8, u8> {
    let mut s = [0u8; 40];
    match dos::status(dev, &mut s) {
        Some((_, code)) if code < 20 => Ok(code),
        Some((_, code)) => Err(code),
        None => Err(255),
    }
}

/// Changes into a directory (or a disk image) of the current directory.
/// `path` can also be a path from the root: `"/games/arcade"`.
pub fn cd(dev: u8, path: &str) -> Result<(), Error> {
    if path.as_bytes().first() == Some(&b'/') {
        send(dev, &["CD/", path])
    } else {
        send(dev, &["CD:", path])
    }
}

/// Changes to the parent directory (out of a disk image, too).
pub fn cd_parent(dev: u8) -> Result<(), Error> {
    dos::command(dev, "CD_")
}

/// Changes to the root directory of the card.
pub fn cd_root(dev: u8) -> Result<(), Error> {
    dos::command(dev, "CD//")
}

/// Creates a directory.
pub fn mkdir(dev: u8, name: &str) -> Result<(), Error> {
    send(dev, &["MD:", name])
}

/// Removes an empty directory.
pub fn rmdir(dev: u8, name: &str) -> Result<(), Error> {
    send(dev, &["RD:", name])
}

/// Mounts a disk image (D64, D71, D81, ...) of the current directory: from
/// now on the device behaves like a drive with that disk.
pub fn mount(dev: u8, image: &str) -> Result<(), Error> {
    send(dev, &["CD:", image])
}

/// Leaves the mounted disk image.
pub fn unmount(dev: u8) -> Result<(), Error> {
    cd_parent(dev)
}

/// Gives the device another number (4-30) until it is reset; store it with
/// [`save_settings`].
pub fn set_device(dev: u8, new: u8) -> Result<(), Error> {
    let mut b = Buf::<4>::new();
    b.write_str("U0>");
    b.write_byte(new & 31);
    dos::command(dev, unsafe { core::str::from_utf8_unchecked(b.as_bytes()) })
}

/// Shows or hides the file extensions (`.PRG`, `.SEQ`) of plain files.
pub fn hide_extensions(dev: u8, hide: bool) -> Result<(), Error> {
    dos::command(dev, if hide { "XE+" } else { "XE-" })
}

/// Sets how disk images appear in directories.
pub fn images_as(dev: u8, mode: Images) -> Result<(), Error> {
    dos::command(
        dev,
        match mode {
            Images::Files => "XI0",
            Images::Dirs => "XI1",
            Images::Both => "XI2",
        },
    )
}

/// Stores the settings in the device's EEPROM.
pub fn save_settings(dev: u8) -> Result<(), Error> {
    dos::command(dev, "XW")
}

/// Asks for the firmware version: afterwards [`status`] reads it
/// (`"03,SD2IEC V1.0.0..."`).
pub fn version(dev: u8) -> Result<(), Error> {
    dos::command(dev, "X?")
}

/// Sends any other command (`"XS:name"`, `"CP2"`, ...).
pub fn command(dev: u8, cmd: &str) -> Result<(), Error> {
    dos::command(dev, cmd)
}
