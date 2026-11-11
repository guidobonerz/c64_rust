//! Ultimate Audio: the sampler of the 1541 Ultimate-II+ and the
//! Ultimate 64. Seven voices play 8 or 16 bit samples from the memory of
//! the REU, each with its own rate, volume and stereo position.
//!
//! On the Ultimate, the REU must be enabled and "Map Ultimate Audio
//! $DF20-DFFF" switched on (C64 and cartridge settings); the outputs of
//! the sampler are routed in the audio mixer. Check with [`present`].
//!
//! ```ignore
//! use c64_uaudio::{self as ua, Voice};
//!
//! if ua::present() {
//!     // 8000 bytes of 8-bit sample data at the start of the REU
//!     let v = Voice(0);
//!     v.sample(0, 8000);
//!     v.rate(ua::rate(11025));
//!     v.volume(40);
//!     v.pan(ua::CENTER);
//!     v.play(0);
//! }
//! ```
//!
//! The sample data has to be in the REU: load it there with the Ultimate
//! (command "load REU" of the command interface, see `c64-uci`), or copy
//! it with the registers of the REU. Samples are signed; 16-bit samples
//! have the low byte first.
//!
//! The registers cannot be read back, and mimic does not emulate the
//! sampler: this crate follows the FPGA source of the Ultimate and has not
//! run against hardware.
#![no_std]

/// The number of voices a C64 program can reach.
pub const VOICES: u8 = 7;

/// Flag of [`Voice::play`]: start again at the loop start when the loop
/// end is reached ([`Voice::repeat`]).
pub const REPEAT: u8 = 0x02;
/// Flag of [`Voice::play`]: raise an interrupt when the sample ends
/// ([`finished`]).
pub const IRQ: u8 = 0x04;
/// Flag of [`Voice::play`]: 16-bit samples (the default is 8 bits).
pub const BITS_16: u8 = 0x10;
/// Flag of [`Voice::play`]: the data is one channel of interleaved
/// stereo, every other sample is skipped.
pub const INTERLEAVED: u8 = 0x40;

/// Stereo positions of [`Voice::pan`].
pub const LEFT: u8 = 0;
pub const CENTER: u8 = 8;
pub const RIGHT: u8 = 15;

const ENABLE: u8 = 0x01;
// The registers of a voice.
const CONTROL: u8 = 0x00;
const VOLUME: u8 = 0x01;
const PAN: u8 = 0x02;
const START: u8 = 0x04;
const LENGTH: u8 = 0x09;
const RATE: u8 = 0x0e;
const REPEAT_A: u8 = 0x11;
const REPEAT_B: u8 = 0x15;
const CLEAR_IRQ: u8 = 0x1f;

/// Where the memory of the REU is for the sampler: the top byte of a
/// start address.
const REU_BASE: u32 = 0x0100_0000;

#[cfg(target_os = "c64")]
c64::rat_source!(UAUDIO_RAT, "uaudio.rat");

#[cfg(target_os = "c64")]
unsafe extern "C" {
    fn c64_uaudio_set(voice: u8, reg: u8, value: u32, bytes: u8);
}

fn set(voice: u8, reg: u8, value: u32, bytes: u8) {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_uaudio_set(voice, reg, value, bytes)
    }
    #[cfg(not(target_os = "c64"))]
    let _ = (voice, reg, value, bytes);
}

/// Whether the sampler is mapped: its version register reads `$10`.
pub fn present() -> bool {
    #[cfg(target_os = "c64")]
    {
        let (version, again): (u8, u8);
        unsafe {
            c64::asm!(
                "lda $df21",
                "sta {v}",
                "lda $df23",
                "sta {w}",
                v = out(reg) version,
                w = out(reg) again,
            )
        };
        version == 0x10 && again == 0x10
    }
    #[cfg(not(target_os = "c64"))]
    false
}

/// The rate value for a sample rate in Hz, for 8-bit samples on a
/// 1541 Ultimate-II(+): `6 250 000 / hz - 2`. For 16-bit samples subtract
/// one more; on an Ultimate 64 the clock is 6 666 667. Use it with a
/// constant: `const R: u16 = c64_uaudio::rate(22050);`.
pub const fn rate(hz: u32) -> u16 {
    (6_250_000 / hz - 2) as u16
}

/// The voices whose sample has ended (bit n: voice n), if they were
/// started with [`IRQ`].
pub fn finished() -> u8 {
    #[cfg(target_os = "c64")]
    {
        let bits: u8;
        unsafe { c64::asm!("lda $df20", "sta {b}", b = out(reg) bits) };
        bits
    }
    #[cfg(not(target_os = "c64"))]
    0
}

/// Clears the "finished" bits (and the interrupt) of all voices.
pub fn clear_finished() {
    set(0, CLEAR_IRQ, 0xff, 1);
}

/// Stops all voices.
pub fn stop_all() {
    for v in 0..VOICES {
        Voice(v).stop();
    }
    clear_finished();
}

/// Voice 0-6.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Voice(pub u8);

impl Voice {
    fn set(self, reg: u8, value: u32, bytes: u8) {
        set(if self.0 < VOICES { self.0 } else { VOICES - 1 }, reg, value, bytes);
    }

    /// Selects the sample: `start` is its address in the REU, `length` its
    /// length in bytes (up to 16 MB).
    pub fn sample(self, start: u32, length: u32) {
        self.set(START, REU_BASE | start & 0x00ff_ffff, 4);
        self.set(LENGTH, length, 3);
    }

    /// Sets the playback speed: a value from [`rate`].
    pub fn rate(self, value: u16) {
        self.set(RATE, value as u32, 2);
    }

    /// Sets the volume (0-63).
    pub fn volume(self, v: u8) {
        self.set(VOLUME, (v & 63) as u32, 1);
    }

    /// Sets the stereo position (0 = [`LEFT`] ... 15 = [`RIGHT`]).
    pub fn pan(self, p: u8) {
        self.set(PAN, (p & 15) as u32, 1);
    }

    /// Sets the loop for [`REPEAT`]: when playback reaches `end` it goes
    /// on at `start` (both are offsets from the start of the sample).
    pub fn repeat(self, start: u32, end: u32) {
        self.set(REPEAT_A, start, 3);
        self.set(REPEAT_B, end, 3);
    }

    /// Starts the sample from its beginning, with [`REPEAT`], [`IRQ`],
    /// [`BITS_16`], [`INTERLEAVED`] or 0.
    pub fn play(self, flags: u8) {
        self.set(CONTROL, 0, 1);
        self.set(CONTROL, (flags | ENABLE) as u32, 1);
    }

    /// Stops the voice.
    pub fn stop(self) {
        self.set(CONTROL, 0, 1);
    }

    /// Clears the "finished" bit of this voice.
    pub fn clear_finished(self) {
        self.set(CLEAR_IRQ, 1, 1);
    }
}
