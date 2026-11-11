//! Wrappers for the KERNAL ROM routines.
//!
//! Arguments go to the CPU registers as the KERNAL expects them. Routines
//! that report errors with the carry flag return a `Result` with the
//! KERNAL error code ([`Error`]).

/// PETSCII control codes for [`chrout`].
pub const RETURN: u8 = 13;
pub const CLEAR: u8 = 147;
pub const HOME: u8 = 19;
pub const DOWN: u8 = 17;
pub const UP: u8 = 145;
pub const LEFT: u8 = 157;
pub const RIGHT: u8 = 29;
pub const REVERSE_ON: u8 = 18;
pub const REVERSE_OFF: u8 = 146;
pub const LOWERCASE: u8 = 14;
pub const UPPERCASE: u8 = 142;
pub const DELETE: u8 = 20;

/// A KERNAL error code (from OPEN, LOAD, SAVE, CHKIN, CHKOUT).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Error(pub u8);

impl Error {
    pub const TOO_MANY_FILES: Error = Error(1);
    pub const FILE_OPEN: Error = Error(2);
    pub const FILE_NOT_OPEN: Error = Error(3);
    pub const FILE_NOT_FOUND: Error = Error(4);
    pub const DEVICE_NOT_PRESENT: Error = Error(5);
    pub const NOT_INPUT_FILE: Error = Error(6);
    pub const NOT_OUTPUT_FILE: Error = Error(7);
    pub const MISSING_FILE_NAME: Error = Error(8);
    pub const ILLEGAL_DEVICE: Error = Error(9);
}

fn result(err: u8) -> Result<(), Error> {
    if err == 0 { Ok(()) } else { Err(Error(err)) }
}

/// Writes a character to the current output channel (CHROUT, `$FFD2`).
pub fn chrout(c: u8) {
    #[cfg(target_os = "c64")]
    unsafe {
        crate::asm!("lda {c}", "jsr $ffd2", c = in(reg) c)
    }
    #[cfg(not(target_os = "c64"))]
    host::chrout(c)
}

/// Reads a character from the current input channel (CHRIN, `$FFCF`).
pub fn chrin() -> u8 {
    #[cfg(target_os = "c64")]
    {
        let c: u8;
        unsafe { crate::asm!("jsr $ffcf", "sta {c}", c = out(reg) c) };
        c
    }
    #[cfg(not(target_os = "c64"))]
    RETURN
}

/// Reads a key from the keyboard buffer, 0 if there is none (GETIN, `$FFE4`).
pub fn getin() -> u8 {
    #[cfg(target_os = "c64")]
    {
        let c: u8;
        unsafe { crate::asm!("jsr $ffe4", "sta {c}", c = out(reg) c) };
        c
    }
    #[cfg(not(target_os = "c64"))]
    0
}

/// Scans the keyboard (SCNKEY, `$FF9F`).
pub fn scnkey() {
    #[cfg(target_os = "c64")]
    unsafe {
        crate::asm!("jsr $ff9f")
    }
}

/// Whether RUN/STOP is pressed (STOP, `$FFE1`).
pub fn stop() -> bool {
    #[cfg(target_os = "c64")]
    {
        // STOP returns with the zero flag set if the key is pressed.
        let pressed: u8;
        unsafe {
            crate::asm!(
                "jsr $ffe1",
                "beq @stop",
                "lda #0",
                "beq @done",
                "@stop: lda #1",
                "@done: sta {p}",
                p = out(reg) pressed,
            )
        };
        pressed != 0
    }
    #[cfg(not(target_os = "c64"))]
    false
}

/// Moves the cursor (PLOT, `$FFF0` with carry clear).
pub fn plot(row: u8, col: u8) {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_plot(row, col)
    }
    #[cfg(not(target_os = "c64"))]
    let _ = (row, col);
}

/// The cursor position as `(row, col)` (PLOT, `$FFF0` with carry set).
pub fn cursor() -> (u8, u8) {
    #[cfg(target_os = "c64")]
    {
        let p = unsafe { c64_cursor() };
        (p.row, p.col)
    }
    #[cfg(not(target_os = "c64"))]
    (0, 0)
}

/// Sets logical file number, device and secondary address (SETLFS, `$FFBA`).
pub fn setlfs(lfn: u8, dev: u8, sa: u8) {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_setlfs(lfn, dev, sa)
    }
    #[cfg(not(target_os = "c64"))]
    let _ = (lfn, dev, sa);
}

/// Sets the file name (SETNAM, `$FFBD`). The bytes are PETSCII (see
/// [`crate::p!`]) and must stay valid until the file is opened, loaded or
/// saved.
pub fn setnam(name: &[u8]) {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_setnam(name.as_ptr(), name.len() as u8)
    }
    #[cfg(not(target_os = "c64"))]
    let _ = name;
}

/// Opens the file set up with [`setlfs`]/[`setnam`] (OPEN, `$FFC0`).
pub fn open() -> Result<(), Error> {
    #[cfg(target_os = "c64")]
    {
        result(unsafe { c64_open() })
    }
    #[cfg(not(target_os = "c64"))]
    result(Error::DEVICE_NOT_PRESENT.0)
}

/// Closes a logical file (CLOSE, `$FFC3`).
pub fn close(lfn: u8) {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_close(lfn)
    }
    #[cfg(not(target_os = "c64"))]
    let _ = lfn;
}

/// Makes a file the input channel (CHKIN, `$FFC6`).
pub fn chkin(lfn: u8) -> Result<(), Error> {
    #[cfg(target_os = "c64")]
    {
        result(unsafe { c64_chkin(lfn) })
    }
    #[cfg(not(target_os = "c64"))]
    {
        let _ = lfn;
        result(Error::FILE_NOT_OPEN.0)
    }
}

/// Makes a file the output channel (CHKOUT, `$FFC9`).
pub fn chkout(lfn: u8) -> Result<(), Error> {
    #[cfg(target_os = "c64")]
    {
        result(unsafe { c64_chkout(lfn) })
    }
    #[cfg(not(target_os = "c64"))]
    {
        let _ = lfn;
        result(Error::FILE_NOT_OPEN.0)
    }
}

/// Restores the default channels (CLRCHN, `$FFCC`).
pub fn clrchn() {
    #[cfg(target_os = "c64")]
    unsafe {
        crate::asm!("jsr $ffcc")
    }
}

/// Closes all files (CLALL, `$FFE7`).
pub fn clall() {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_clall()
    }
}

/// The I/O status byte (READST, `$FFB7`).
pub fn readst() -> u8 {
    #[cfg(target_os = "c64")]
    {
        let st: u8;
        unsafe { crate::asm!("jsr $ffb7", "sta {st}", st = out(reg) st) };
        st
    }
    #[cfg(not(target_os = "c64"))]
    0x40
}

/// Loads (or with `verify` compares) the file set up with
/// [`setlfs`]/[`setnam`] to `addr` (used when the secondary address is 0)
/// and returns the end address (LOAD, `$FFD5`).
pub fn load(verify: bool, addr: u16) -> Result<u16, Error> {
    #[cfg(target_os = "c64")]
    {
        let r = unsafe { c64_load(verify as u8, addr) };
        result(r.err).map(|()| r.end)
    }
    #[cfg(not(target_os = "c64"))]
    {
        let _ = (verify, addr);
        Err(Error::DEVICE_NOT_PRESENT)
    }
}

/// Saves the memory `start..end` to the file set up with
/// [`setlfs`]/[`setnam`] (SAVE, `$FFD8`).
pub fn save(start: u16, end: u16) -> Result<(), Error> {
    #[cfg(target_os = "c64")]
    {
        result(unsafe { c64_save(start, end) })
    }
    #[cfg(not(target_os = "c64"))]
    {
        let _ = (start, end);
        Err(Error::DEVICE_NOT_PRESENT)
    }
}

/// The jiffy clock (1/60 s since power-on, 24 bits) (RDTIM, `$FFDE`).
pub fn rdtim() -> u32 {
    #[cfg(target_os = "c64")]
    {
        // RDTIM returns the three bytes in A, X, Y (low byte first).
        let (lo, mid, hi): (u8, u8, u8);
        unsafe {
            crate::asm!(
                "jsr $ffde",
                "sta {lo}",
                "stx {mid}",
                "sty {hi}",
                lo = out(reg) lo,
                mid = out(reg) mid,
                hi = out(reg) hi,
            )
        };
        u32::from_le_bytes([lo, mid, hi, 0])
    }
    #[cfg(not(target_os = "c64"))]
    0
}

/// Sets the jiffy clock (SETTIM, `$FFDB`).
pub fn settim(t: u32) {
    #[cfg(target_os = "c64")]
    {
        let [lo, mid, hi, _] = t.to_le_bytes();
        unsafe {
            crate::asm!(
                "lda {lo}",
                "ldx {mid}",
                "ldy {hi}",
                "jsr $ffdb",
                lo = in(reg) lo,
                mid = in(reg) mid,
                hi = in(reg) hi,
            )
        }
    }
    #[cfg(not(target_os = "c64"))]
    let _ = t;
}

/// The low 16 bits of the jiffy clock.
pub fn jiffies() -> u16 {
    #[cfg(target_os = "c64")]
    {
        let t: u16;
        unsafe { crate::asm!("jsr $ffde", "sta {t}", "stx {t}+1", t = out(reg) t) };
        t
    }
    #[cfg(not(target_os = "c64"))]
    {
        rdtim() as u16
    }
}

#[cfg(target_os = "c64")]
#[repr(C)]
struct Pos {
    row: u8,
    col: u8,
}

#[cfg(target_os = "c64")]
#[repr(C)]
struct LoadResult {
    err: u8,
    end: u16,
}

#[cfg(target_os = "c64")]
crate::rat_source!(KERNAL_RAT, "rat/kernal.rat");

#[cfg(target_os = "c64")]
unsafe extern "C" {
    fn c64_plot(row: u8, col: u8);
    fn c64_cursor() -> Pos;
    fn c64_setlfs(lfn: u8, dev: u8, sa: u8);
    fn c64_setnam(name: *const u8, len: u8);
    fn c64_open() -> u8;
    fn c64_close(lfn: u8);
    fn c64_chkin(lfn: u8) -> u8;
    fn c64_chkout(lfn: u8) -> u8;
    fn c64_clall();
    fn c64_load(verify: u8, addr: u16) -> LoadResult;
    fn c64_save(start: u16, end: u16) -> u8;
}

#[cfg(not(target_os = "c64"))]
mod host {
    use std::io::Write;

    /// PETSCII to stdout, roughly.
    pub fn chrout(c: u8) {
        let ch = match c {
            super::RETURN => b'\n',
            0x41..=0x5a if crate::text::is_lowercase() => c + 0x20,
            0xc1..=0xda => c - 0x80,
            0x20..=0x7e => c,
            _ => return,
        };
        let _ = std::io::stdout().write_all(&[ch]);
    }
}
