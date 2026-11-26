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

use core::mem::MaybeUninit;

use crate::kernal::{self, Error};

/// The logical file and secondary address of the drive's command channel.
pub const COMMAND_CHANNEL: u8 = 15;

/// Status bits of [`kernal::readst`].
pub const STATUS_EOF: u8 = 0x40;
pub const STATUS_TIMEOUT: u8 = 0x02;
pub const STATUS_MISSING: u8 = 0x80;

// On the C64 the work of this module is assembly (rat/dos.rat): the names,
// the bytes of a file, the lines of the directory and the drive status.
// The Rust versions are what the host uses.
#[cfg(target_os = "c64")]
crate::rat_source!(DOS_RAT, "rat/dos.rat");

#[cfg(target_os = "c64")]
unsafe extern "C" {
    fn c64_dos_push(n: *mut Name, s: *const u8, len: u16);
    fn c64_dos_cmd(dev: u8) -> u8;
    fn c64_dos_get(f: *mut File) -> Got;
    fn c64_dos_read(f: *mut File, buf: *mut u8, max: u16) -> u16;
    fn c64_dos_write(f: *mut File, data: *const u8, len: u16) -> u8;
    fn c64_dos_write_str(f: *mut File, data: *const u8, len: u16) -> u8;
    fn c64_dos_line(d: *mut Dir) -> u8;
    fn c64_dos_status(dev: u8, buf: *mut u8, max: u16) -> Status;
}

/// What `c64_dos_get` returns.
#[cfg(target_os = "c64")]
#[repr(C)]
struct Got {
    ok: u8,
    byte: u8,
}

/// What `c64_dos_status` returns.
#[cfg(target_os = "c64")]
#[repr(C)]
struct Status {
    n: u8,
    code: u8,
    ok: u8,
}

/// A file name or command converted to PETSCII (rat/dos.rat knows the
/// layout).
#[repr(C)]
struct Name {
    len: u8,
    buf: MaybeUninit<[u8; 40]>,
}

impl Name {
    fn new(s: &str) -> Name {
        let mut n = Name { len: 0, buf: MaybeUninit::uninit() };
        n.push(s);
        n
    }

    /// Appends `s`; what does not fit is left out.
    fn push(&mut self, s: &str) {
        #[cfg(target_os = "c64")]
        unsafe {
            c64_dos_push(self, s.as_ptr(), s.len() as u16)
        }
        #[cfg(not(target_os = "c64"))]
        for &c in s.as_bytes() {
            if (self.len as usize) < 40 {
                let b = if c.is_ascii_lowercase() { c - 0x20 } else { c };
                unsafe { self.buf.as_mut_ptr().cast::<u8>().add(self.len as usize).write(b) };
                self.len += 1;
            }
        }
    }

    fn set(&self) {
        kernal::setnam_len(self.buf.as_ptr().cast::<u8>(), self.len);
    }
}

/// Loads a file to `addr` and returns the address after its last byte.
pub fn load(name: &str, dev: u8, addr: u16) -> Result<u16, Error> {
    let n = Name::new(name);
    n.set();
    kernal::setlfs(1, dev, 0);
    kernal::load(false, addr)
}

/// Loads a file to the address in its first two bytes.
pub fn load_abs(name: &str, dev: u8) -> Result<u16, Error> {
    let n = Name::new(name);
    n.set();
    kernal::setlfs(1, dev, 1);
    kernal::load(false, 0)
}

/// Saves `start..end` as a program file (with `start` as load address).
pub fn save(name: &str, dev: u8, start: u16, end: u16) -> Result<(), Error> {
    let n = Name::new(name);
    n.set();
    kernal::setlfs(1, dev, 0);
    kernal::save(start, end)
}

fn command_name(dev: u8, n: &Name) -> Result<(), Error> {
    n.set();
    #[cfg(target_os = "c64")]
    {
        kernal::result(unsafe { c64_dos_cmd(dev) })
    }
    #[cfg(not(target_os = "c64"))]
    {
        kernal::setlfs(COMMAND_CHANNEL, dev, COMMAND_CHANNEL);
        let r = kernal::open();
        kernal::close(COMMAND_CHANNEL);
        r
    }
}

/// Sends a DOS command (`"S0:NAME"`, `"R0:NEW=OLD"`, `"I0"`, `"V0"`,
/// `"N0:NAME,ID"`).
pub fn command(dev: u8, cmd: &str) -> Result<(), Error> {
    command_name(dev, &Name::new(cmd))
}

/// Sends the DOS command `cmd` followed by `arg` (`"S0:"` and a name).
pub fn command_arg(dev: u8, cmd: &str, arg: &str) -> Result<(), Error> {
    let mut n = Name::new(cmd);
    n.push(arg);
    command_name(dev, &n)
}

/// Deletes a file.
pub fn scratch(dev: u8, name: &str) -> Result<(), Error> {
    command_arg(dev, "S0:", name)
}

/// Renames a file.
pub fn rename(dev: u8, from: &str, to: &str) -> Result<(), Error> {
    let mut n = Name::new("R0:");
    n.push(to);
    n.push("=");
    n.push(from);
    command_name(dev, &n)
}

/// Reads the drive status (`"00, OK,00,00"`) into `buf` and returns the
/// number of bytes and the two-digit error number (0 = ok, 62 = file not
/// found, ...). `None` if the drive does not answer. At most 255 bytes are
/// read.
#[cfg(target_os = "c64")]
pub fn status(dev: u8, buf: &mut [u8]) -> Option<(usize, u8)> {
    let s = unsafe { c64_dos_status(dev, buf.as_mut_ptr(), buf.len() as u16) };
    if s.ok != 0 { Some((s.n as usize, s.code)) } else { None }
}

/// Reads the drive status (`"00, OK,00,00"`) into `buf` and returns the
/// number of bytes and the two-digit error number (0 = ok, 62 = file not
/// found, ...). `None` if the drive does not answer. At most 255 bytes are
/// read.
#[cfg(not(target_os = "c64"))]
pub fn status(dev: u8, buf: &mut [u8]) -> Option<(usize, u8)> {
    let max = if buf.len() > 255 { 255 } else { buf.len() };
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
        if n < max {
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
#[repr(C)]
pub struct File {
    lfn: u8,
    eof: bool,
}

impl File {
    /// Opens a file: `name` may carry type and mode (`"DATA,S,R"`,
    /// `"OUT,S,W"`).
    pub fn open(lfn: u8, dev: u8, sa: u8, name: &str) -> Result<File, Error> {
        let n = Name::new(name);
        n.set();
        kernal::setlfs(lfn, dev, sa);
        kernal::open()?;
        Ok(File { lfn, eof: false })
    }

    /// Reads one byte; `None` at the end of the file or on an error.
    #[cfg(target_os = "c64")]
    pub fn get(&mut self) -> Option<u8> {
        let g = unsafe { c64_dos_get(self) };
        if g.ok != 0 { Some(g.byte) } else { None }
    }

    /// Reads one byte; `None` at the end of the file or on an error.
    #[cfg(not(target_os = "c64"))]
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
    #[cfg(target_os = "c64")]
    pub fn read(&mut self, buf: &mut [u8]) -> usize {
        unsafe { c64_dos_read(self, buf.as_mut_ptr(), buf.len() as u16) as usize }
    }

    /// Reads up to `buf.len()` bytes and returns how many it read.
    #[cfg(not(target_os = "c64"))]
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
    #[cfg(target_os = "c64")]
    pub fn write(&mut self, data: &[u8]) -> Result<(), Error> {
        kernal::result(unsafe { c64_dos_write(self, data.as_ptr(), data.len() as u16) })
    }

    /// Writes bytes as they are.
    #[cfg(not(target_os = "c64"))]
    pub fn write(&mut self, data: &[u8]) -> Result<(), Error> {
        kernal::chkout(self.lfn)?;
        for &b in data {
            kernal::chrout(b);
        }
        kernal::clrchn();
        Ok(())
    }

    /// Writes an ASCII string converted to PETSCII.
    #[cfg(target_os = "c64")]
    pub fn write_str(&mut self, s: &str) -> Result<(), Error> {
        kernal::result(unsafe { c64_dos_write_str(self, s.as_ptr(), s.len() as u16) })
    }

    /// Writes an ASCII string converted to PETSCII.
    #[cfg(not(target_os = "c64"))]
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

/// One line of the directory (rat/dos.rat knows the layout).
#[derive(Clone, Copy)]
#[repr(C)]
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
    #[cfg(not(target_os = "c64"))]
    const EMPTY: DirEntry = DirEntry { blocks: 0, name: [0; 16], name_len: 0, kind: [b' '; 3] };

    /// The file name (PETSCII, see [`crate::text::print_petscii`]).
    pub fn name(&self) -> &[u8] {
        &self.name[..self.name_len as usize]
    }
}

/// The directory of a drive, as an iterator over its files. After the last
/// one, [`Dir::free`] is the number of free blocks.
#[repr(C)]
pub struct Dir {
    f: File,
    /// The line that was read last (rat/dos.rat writes it).
    entry: DirEntry,
    /// The disk name (`name`) and id (`kind`).
    pub header: DirEntry,
    free: u16,
    done: bool,
}

impl Dir {
    /// Opens the directory (`"$"`) on logical file `lfn` and reads the
    /// header line.
    pub fn open(lfn: u8, dev: u8) -> Result<Dir, Error> {
        kernal::setnam(b"$");
        kernal::setlfs(lfn, dev, 0);
        kernal::open()?;
        // The assembly fills in the entry, so nothing is stored twice.
        #[cfg(target_os = "c64")]
        unsafe {
            let mut d = MaybeUninit::<Dir>::uninit();
            let p = d.as_mut_ptr();
            (&raw mut (*p).f).write(File { lfn, eof: false });
            (&raw mut (*p).free).write(0);
            c64_dos_get(&raw mut (*p).f); // load address
            c64_dos_get(&raw mut (*p).f);
            let found = c64_dos_line(p) != 0;
            core::ptr::copy_nonoverlapping(&raw const (*p).entry, &raw mut (*p).header, 1);
            (&raw mut (*p).done).write(!found);
            Ok(d.assume_init())
        }
        #[cfg(not(target_os = "c64"))]
        {
            let f = File { lfn, eof: false };
            let mut d = Dir { f, entry: DirEntry::EMPTY, header: DirEntry::EMPTY, free: 0, done: false };
            d.f.get(); // load address
            d.f.get();
            if d.read_line() {
                d.header = d.entry;
            } else {
                d.done = true;
            }
            Ok(d)
        }
    }

    /// Free blocks (known after the last entry).
    pub fn free(&self) -> u16 {
        self.free
    }

    /// Parses one BASIC line of the directory listing into `entry`, which
    /// is empty where the line says nothing; `false` at the end.
    fn read_line(&mut self) -> bool {
        #[cfg(target_os = "c64")]
        unsafe {
            c64_dos_line(self) != 0
        }
        #[cfg(not(target_os = "c64"))]
        match self.parse_line() {
            Some(e) => {
                self.entry = e;
                true
            }
            None => {
                self.entry = DirEntry::EMPTY;
                false
            }
        }
    }

    #[cfg(not(target_os = "c64"))]
    fn parse_line(&mut self) -> Option<DirEntry> {
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
        if !self.read_line() || self.entry.name_len == 0 {
            // the end, or "BLOCKS FREE."
            self.free = self.entry.blocks;
            self.done = true;
            return None;
        }
        Some(self.entry)
    }
}
