//! Disk drives through the KERNAL: loading and saving memory, sequential
//! files, the command channel and the directory.
//!
//! ```ignore
//! let end = dos::load("LEVEL1", 8, 0xc000)?;
//! let mut f = dos::File::open(2, 8, 2, "SCORES,S,W")?;
//! f.write_str("12345")?;
//! drop(f);                                   // closes the file
//! dos::command(8, "S0:OLDFILE")?;
//! for e in dos::Dir::open(2, 8)? { ... }
//! ```
//!
//! File names are ASCII and converted to PETSCII (capitals). Errors are
//! KERNAL errors ([`Error`]); drive errors (file not found on the disk,
//! disk full, ...) are read with [`status`].

use crate::kernal::{self, Error};

/// The logical file and secondary address of the drive's command channel.
pub const COMMAND_CHANNEL: u8 = 15;

/// Status bits of [`kernal::readst`].
pub const STATUS_EOF: u8 = 0x40;
pub const STATUS_TIMEOUT: u8 = 0x02;
pub const STATUS_MISSING: u8 = 0x80;

/// A file name or command converted to PETSCII.
struct Name {
    buf: [u8; 40],
    len: usize,
}

impl Name {
    fn new(parts: &[&str]) -> Name {
        let mut n = Name { buf: [0; 40], len: 0 };
        for p in parts {
            for &c in p.as_bytes() {
                if n.len < n.buf.len() {
                    n.buf[n.len] = if c.is_ascii_lowercase() { c - 0x20 } else { c };
                    n.len += 1;
                }
            }
        }
        n
    }

    fn set(&self) {
        kernal::setnam(&self.buf[..self.len]);
    }
}

/// Loads a file to `addr` and returns the address after its last byte.
pub fn load(name: &str, dev: u8, addr: u16) -> Result<u16, Error> {
    let n = Name::new(&[name]);
    n.set();
    kernal::setlfs(1, dev, 0);
    kernal::load(false, addr)
}

/// Loads a file to the address in its first two bytes.
pub fn load_abs(name: &str, dev: u8) -> Result<u16, Error> {
    let n = Name::new(&[name]);
    n.set();
    kernal::setlfs(1, dev, 1);
    kernal::load(false, 0)
}

/// Saves `start..end` as a program file (with `start` as load address).
pub fn save(name: &str, dev: u8, start: u16, end: u16) -> Result<(), Error> {
    let n = Name::new(&[name]);
    n.set();
    kernal::setlfs(1, dev, 0);
    kernal::save(start, end)
}

fn command_name(dev: u8, n: &Name) -> Result<(), Error> {
    n.set();
    kernal::setlfs(COMMAND_CHANNEL, dev, COMMAND_CHANNEL);
    let r = kernal::open();
    kernal::close(COMMAND_CHANNEL);
    r
}

/// Sends a DOS command (`"S0:NAME"`, `"R0:NEW=OLD"`, `"I0"`, `"V0"`,
/// `"N0:NAME,ID"`).
pub fn command(dev: u8, cmd: &str) -> Result<(), Error> {
    command_name(dev, &Name::new(&[cmd]))
}

/// Deletes a file.
pub fn scratch(dev: u8, name: &str) -> Result<(), Error> {
    command_name(dev, &Name::new(&["S0:", name]))
}

/// Renames a file.
pub fn rename(dev: u8, from: &str, to: &str) -> Result<(), Error> {
    command_name(dev, &Name::new(&["R0:", to, "=", from]))
}

/// Reads the drive status (`"00, OK,00,00"`) into `buf` and returns the
/// number of bytes and the two-digit error number (0 = ok, 62 = file not
/// found, ...). `None` if the drive does not answer.
pub fn status(dev: u8, buf: &mut [u8]) -> Option<(usize, u8)> {
    kernal::setnam(&[]);
    kernal::setlfs(COMMAND_CHANNEL, dev, COMMAND_CHANNEL);
    if kernal::open().is_err() {
        kernal::close(COMMAND_CHANNEL);
        return None;
    }
    let _ = kernal::chkin(COMMAND_CHANNEL);
    let mut n = 0;
    loop {
        let c = kernal::chrin();
        let st = kernal::readst();
        if c == kernal::RETURN || st & STATUS_MISSING != 0 {
            break;
        }
        if n < buf.len() {
            buf[n] = c;
            n += 1;
        }
        if st != 0 {
            break;
        }
    }
    kernal::clrchn();
    kernal::close(COMMAND_CHANNEL);
    let code = if n >= 2 {
        buf[0].wrapping_sub(b'0').wrapping_mul(10).wrapping_add(buf[1].wrapping_sub(b'0'))
    } else {
        0
    };
    Some((n, code))
}

/// An open logical file; dropping it closes it.
pub struct File {
    lfn: u8,
    eof: bool,
}

impl File {
    /// Opens a file: `name` may carry type and mode (`"DATA,S,R"`,
    /// `"OUT,S,W"`).
    pub fn open(lfn: u8, dev: u8, sa: u8, name: &str) -> Result<File, Error> {
        let n = Name::new(&[name]);
        n.set();
        kernal::setlfs(lfn, dev, sa);
        kernal::open()?;
        Ok(File { lfn, eof: false })
    }

    /// Reads one byte; `None` at the end of the file or on an error.
    pub fn get(&mut self) -> Option<u8> {
        if self.eof || kernal::chkin(self.lfn).is_err() {
            return None;
        }
        let b = kernal::chrin();
        let st = kernal::readst();
        kernal::clrchn();
        if st & STATUS_EOF != 0 {
            self.eof = true;
        }
        if st & !STATUS_EOF != 0 {
            self.eof = true;
            return None;
        }
        Some(b)
    }

    /// Reads up to `buf.len()` bytes and returns how many it read.
    pub fn read(&mut self, buf: &mut [u8]) -> usize {
        if self.eof || kernal::chkin(self.lfn).is_err() {
            return 0;
        }
        let mut n = 0;
        while n < buf.len() {
            let c = kernal::chrin();
            let st = kernal::readst();
            if st & !STATUS_EOF != 0 {
                self.eof = true;
                break;
            }
            buf[n] = c;
            n += 1;
            if st & STATUS_EOF != 0 {
                self.eof = true;
                break;
            }
        }
        kernal::clrchn();
        n
    }

    /// Whether the end of the file was reached.
    pub fn eof(&self) -> bool {
        self.eof
    }

    /// Writes bytes as they are.
    pub fn write(&mut self, data: &[u8]) -> Result<(), Error> {
        kernal::chkout(self.lfn)?;
        for &b in data {
            kernal::chrout(b);
        }
        kernal::clrchn();
        Ok(())
    }

    /// Writes an ASCII string converted to PETSCII.
    pub fn write_str(&mut self, s: &str) -> Result<(), Error> {
        kernal::chkout(self.lfn)?;
        for &b in s.as_bytes() {
            kernal::chrout(crate::text::petscii(b));
        }
        kernal::clrchn();
        Ok(())
    }
}

impl Drop for File {
    fn drop(&mut self) {
        kernal::close(self.lfn);
    }
}

/// One line of the directory.
#[derive(Clone, Copy)]
pub struct DirEntry {
    /// Size in blocks.
    pub blocks: u16,
    name: [u8; 16],
    name_len: u8,
    /// The file type (`PRG`, `SEQ`, ...; PETSCII), for the header the disk
    /// id.
    pub kind: [u8; 3],
}

impl DirEntry {
    const EMPTY: DirEntry = DirEntry { blocks: 0, name: [0; 16], name_len: 0, kind: [b' '; 3] };

    /// The file name (PETSCII, see [`crate::text::print_petscii`]).
    pub fn name(&self) -> &[u8] {
        &self.name[..self.name_len as usize]
    }
}

/// The directory of a drive, as an iterator over its files. After the last
/// one, [`Dir::free`] is the number of free blocks.
pub struct Dir {
    f: File,
    /// The disk name (`name`) and id (`kind`).
    pub header: DirEntry,
    free: u16,
    done: bool,
}

impl Dir {
    /// Opens the directory (`"$"`) on logical file `lfn` and reads the
    /// header line.
    pub fn open(lfn: u8, dev: u8) -> Result<Dir, Error> {
        let f = File::open(lfn, dev, 0, "$")?;
        let mut d = Dir { f, header: DirEntry::EMPTY, free: 0, done: false };
        d.f.get(); // load address
        d.f.get();
        match d.line() {
            Some(h) => d.header = h,
            None => d.done = true,
        }
        Ok(d)
    }

    /// Free blocks (known after the last entry).
    pub fn free(&self) -> u16 {
        self.free
    }

    /// Parses one BASIC line of the directory listing.
    fn line(&mut self) -> Option<DirEntry> {
        let mut e = DirEntry::EMPTY;
        let lo = self.f.get()?;
        let hi = self.f.get()?;
        if lo == 0 && hi == 0 {
            return None;
        }
        let blo = self.f.get().unwrap_or(0);
        let bhi = self.f.get().unwrap_or(0);
        e.blocks = (bhi as u16) << 8 | blo as u16;
        let mut state = 0u8; // 0 before the name, 1 in it, 2 after it
        let mut t = 0;
        while let Some(c) = self.f.get() {
            if c == 0 {
                break;
            }
            if c == b'"' {
                state += 1;
            } else if state == 1 {
                if (e.name_len as usize) < e.name.len() {
                    e.name[e.name_len as usize] = c;
                    e.name_len += 1;
                }
            } else if state == 2 && c != b' ' && t < 3 {
                e.kind[t] = c;
                t += 1;
            }
        }
        Some(e)
    }
}

impl Iterator for Dir {
    type Item = DirEntry;

    fn next(&mut self) -> Option<DirEntry> {
        if self.done {
            return None;
        }
        let Some(e) = self.line() else {
            self.done = true;
            return None;
        };
        if e.name_len == 0 {
            // "BLOCKS FREE."
            self.free = e.blocks;
            self.done = true;
            return None;
        }
        Some(e)
    }
}
