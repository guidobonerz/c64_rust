//! The C64 hardware: VIC-II, SID, the two CIAs, screen and colour RAM and
//! the CPU port, as register structs.
//!
//! ```ignore
//! hw::vic().border.write(Color::Green);    // $d020 = 5
//! let line = hw::vic().raster.read();
//! ```

use core::cell::UnsafeCell;

use crate::mem;

/// A hardware register (or any memory cell) that is read and written with
/// volatile accesses.
#[repr(transparent)]
pub struct Volatile<T: Copy>(UnsafeCell<T>);

// The C64 has a single thread; interrupt handlers are the caller's concern.
unsafe impl<T: Copy> Sync for Volatile<T> {}

impl<T: Copy> Volatile<T> {
    #[inline(always)]
    pub fn read(&self) -> T {
        unsafe { core::ptr::read_volatile(self.0.get()) }
    }

    #[inline(always)]
    pub fn write(&self, v: T) {
        unsafe { core::ptr::write_volatile(self.0.get(), v) }
    }

    /// Reads, changes and writes back the value.
    #[inline(always)]
    pub fn modify(&self, f: impl FnOnce(T) -> T) {
        self.write(f(self.read()))
    }
}

impl Volatile<u8> {
    /// Sets the bits of `mask`.
    #[inline(always)]
    pub fn set_bits(&self, mask: u8) {
        self.write(self.read() | mask)
    }

    /// Clears the bits of `mask`.
    #[inline(always)]
    pub fn clear_bits(&self, mask: u8) {
        self.write(self.read() & !mask)
    }
}

/// The 16 colours.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Color {
    Black = 0,
    White = 1,
    Red = 2,
    Cyan = 3,
    Purple = 4,
    Green = 5,
    Blue = 6,
    Yellow = 7,
    Orange = 8,
    Brown = 9,
    LightRed = 10,
    DarkGrey = 11,
    Grey = 12,
    LightGreen = 13,
    LightBlue = 14,
    LightGrey = 15,
}

impl Color {
    /// The colour with number `v & 15`.
    #[inline(always)]
    pub fn from_u8(v: u8) -> Color {
        // Every value 0..16 is a colour.
        unsafe { core::mem::transmute::<u8, Color>(v & 15) }
    }
}

impl From<Color> for u8 {
    fn from(c: Color) -> u8 {
        c as u8
    }
}

/// A colour register (only the low nibble exists; reads return the high
/// nibble as garbage).
#[repr(transparent)]
pub struct ColorReg(Volatile<u8>);

impl ColorReg {
    #[inline(always)]
    pub fn read(&self) -> Color {
        Color::from_u8(self.0.read())
    }

    #[inline(always)]
    pub fn write(&self, c: Color) {
        self.0.write(c as u8)
    }
}

/// The position registers of one sprite.
#[repr(C)]
pub struct SpritePos {
    pub x: Volatile<u8>,
    pub y: Volatile<u8>,
}

/// The VIC-II at `$D000`.
#[repr(C)]
pub struct Vic {
    /// `$D000-$D00F`
    pub sprite: [SpritePos; 8],
    /// `$D010`: bit 8 of the sprite X positions
    pub sprite_x_msb: Volatile<u8>,
    /// `$D011`: YSCROLL, 25 rows, screen on, bitmap, ECM, raster bit 8
    pub control1: Volatile<u8>,
    /// `$D012`
    pub raster: Volatile<u8>,
    pub light_pen_x: Volatile<u8>,
    pub light_pen_y: Volatile<u8>,
    /// `$D015`
    pub sprite_enable: Volatile<u8>,
    /// `$D016`: XSCROLL, 40 columns, multicolour
    pub control2: Volatile<u8>,
    /// `$D017`
    pub sprite_expand_y: Volatile<u8>,
    /// `$D018`: screen and character set / bitmap base
    pub memory: Volatile<u8>,
    /// `$D019`
    pub irq_status: Volatile<u8>,
    /// `$D01A`
    pub irq_enable: Volatile<u8>,
    /// `$D01B`: 1 = behind the background
    pub sprite_priority: Volatile<u8>,
    /// `$D01C`
    pub sprite_multicolor: Volatile<u8>,
    /// `$D01D`
    pub sprite_expand_x: Volatile<u8>,
    /// `$D01E`: sprite-sprite collisions (cleared on read)
    pub sprite_collision: Volatile<u8>,
    /// `$D01F`: sprite-background collisions (cleared on read)
    pub sprite_bg_collision: Volatile<u8>,
    /// `$D020`
    pub border: ColorReg,
    /// `$D021-$D024`
    pub background: [ColorReg; 4],
    /// `$D025-$D026`: shared sprite multicolours
    pub sprite_mc: [ColorReg; 2],
    /// `$D027-$D02E`
    pub sprite_color: [ColorReg; 8],
}

/// One SID voice.
#[repr(C)]
pub struct Voice {
    /// `$D400/$D401`
    pub freq: Volatile<u16>,
    /// `$D402/$D403` (12 bits)
    pub pulse: Volatile<u16>,
    /// `$D404`: gate, sync, ring, test, waveforms
    pub control: Volatile<u8>,
    /// `$D405`: attack/decay
    pub attack_decay: Volatile<u8>,
    /// `$D406`: sustain/release
    pub sustain_release: Volatile<u8>,
}

/// The SID at `$D400`. All registers except the last four are write-only.
#[repr(C)]
pub struct Sid {
    pub voice: [Voice; 3],
    /// `$D415/$D416`: bits 0-2 in the low byte, 3-10 in the high byte
    pub cutoff: Volatile<u16>,
    /// `$D417`: resonance (high nibble) and filter routing
    pub resonance: Volatile<u8>,
    /// `$D418`: filter mode (high nibble) and volume
    pub mode_volume: Volatile<u8>,
    pub pot_x: Volatile<u8>,
    pub pot_y: Volatile<u8>,
    /// `$D41B`: oscillator 3 (random numbers with the noise waveform)
    pub osc3: Volatile<u8>,
    pub env3: Volatile<u8>,
}

/// A 6526 CIA.
#[repr(C)]
pub struct Cia {
    /// port A
    pub pra: Volatile<u8>,
    /// port B
    pub prb: Volatile<u8>,
    pub ddra: Volatile<u8>,
    pub ddrb: Volatile<u8>,
    pub timer_a: Volatile<u16>,
    pub timer_b: Volatile<u16>,
    pub tod_10th: Volatile<u8>,
    pub tod_sec: Volatile<u8>,
    pub tod_min: Volatile<u8>,
    pub tod_hour: Volatile<u8>,
    pub sdr: Volatile<u8>,
    /// interrupt control
    pub icr: Volatile<u8>,
    pub cra: Volatile<u8>,
    pub crb: Volatile<u8>,
}

#[inline(always)]
fn at<T>(a: u16) -> &'static T {
    unsafe { &*(mem::addr(a) as *const T) }
}

/// The VIC-II.
#[inline(always)]
pub fn vic() -> &'static Vic {
    at(0xd000)
}

/// The SID.
#[inline(always)]
pub fn sid() -> &'static Sid {
    at(0xd400)
}

/// CIA 1 at `$DC00`: keyboard, joysticks, IRQ timer.
#[inline(always)]
pub fn cia1() -> &'static Cia {
    at(0xdc00)
}

/// CIA 2 at `$DD00`: VIC bank (bits 0-1 of port A, inverted), serial bus,
/// NMI.
#[inline(always)]
pub fn cia2() -> &'static Cia {
    at(0xdd00)
}

/// The default text screen at `$0400`.
#[inline(always)]
pub fn screen() -> &'static [Volatile<u8>; 1000] {
    at(0x0400)
}

/// Colour RAM at `$D800` (low nibble only).
#[inline(always)]
pub fn color_ram() -> &'static [Volatile<u8>; 1000] {
    at(0xd800)
}

/// The sprite data pointers of the default screen (`$07F8`).
#[inline(always)]
pub fn sprite_pointers() -> &'static [Volatile<u8>; 8] {
    at(0x07f8)
}

/// The 6510 port at `$01` (memory configuration, see [`ALL_ROMS`] etc.).
#[inline(always)]
pub fn cpu_port() -> &'static Volatile<u8> {
    at(0x0001)
}

/// [`cpu_port`]: BASIC, KERNAL and I/O visible (power-on).
pub const ALL_ROMS: u8 = 0x37;
/// [`cpu_port`]: RAM at `$A000-$BFFF`, KERNAL and I/O visible.
pub const NO_BASIC: u8 = 0x36;
/// [`cpu_port`]: RAM everywhere except I/O at `$D000`.
pub const IO_ONLY: u8 = 0x35;

/// The raster line of the beam (0-311 on PAL): `$D012` plus bit 7 of
/// `$D011`.
#[cfg(target_os = "c64")]
pub fn raster_line() -> u16 {
    let line: u16;
    unsafe {
        crate::asm!("jsr c64_hw_raster_line", "sta {l}", "stx {l}+1", l = out(reg) line);
    }
    line
}

#[cfg(target_os = "c64")]
crate::rat_source!(HW_RAT, "rat/hw.rat");

/// The raster line of the beam (0-311 on PAL): `$D012` plus bit 7 of
/// `$D011`.
#[cfg(not(target_os = "c64"))]
pub fn raster_line() -> u16 {
    let v = vic();
    let hi = v.control1.read() & 0x80;
    let mut lo = v.raster.read();
    let hi2 = v.control1.read() & 0x80;
    if hi2 != hi {
        // The beam crossed line 255/256 (or the frame end) meanwhile: the
        // second bit 8 and a new low byte belong together.
        lo = v.raster.read();
    }
    lo as u16 | (hi2 as u16) << 1
}

/// Busy-waits until the beam reaches `line` (0-255) in the next frame: it
/// waits for the beam to be above the line, then for it to be at or below
/// it, or for the next frame to start. (Comparing for equality would miss
/// the line when one check takes longer than a raster line, and an
/// interrupt handler may cover the whole rest of the frame.)
#[cfg(target_os = "c64")]
pub fn wait_raster(line: u8) {
    unsafe { crate::asm!("lda {l}", "jsr c64_hw_wait_raster", l = in(reg) line) }
}

/// Busy-waits until the beam reaches `line` (see above).
#[cfg(not(target_os = "c64"))]
pub fn wait_raster(line: u8) {
    let l = line as u16;
    let mut prev = raster_line();
    if prev >= l {
        // Below the line: wait for the next frame.
        loop {
            let n = raster_line();
            let wrapped = n < prev;
            prev = n;
            if wrapped {
                break;
            }
        }
    }
    loop {
        let n = raster_line();
        if n >= l || n < prev {
            return;
        }
        prev = n;
    }
}

/// Waits for the bottom of the visible screen (raster line 251), so that
/// changes made afterwards do not tear.
pub fn wait_frame() {
    wait_raster(251);
}

/// Selects the 16K bank of the VIC-II (0: `$0000`, 1: `$4000`,
/// 2: `$8000`, 3: `$C000`).
pub fn set_vic_bank(bank: u8) {
    let c = cia2();
    c.ddra.set_bits(3);
    c.pra.write(c.pra.read() & 0xfc | (3 - (bank & 3)));
}

/// Sets the screen (0-15, in units of 1K) and the character set or bitmap
/// base (0-7, in units of 2K) inside the VIC bank.
pub fn set_screen(screen: u8, charset: u8) {
    vic().memory.write(screen << 4 | charset << 1);
}

/// Makes `$A000-$BFFF` RAM.
pub fn bank_out_basic() {
    cpu_port().write(NO_BASIC);
}
