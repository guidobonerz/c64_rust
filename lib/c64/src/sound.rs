//! The SID: voices, notes, envelopes, the filter, and SID tunes (PSID
//! files) played from a raster interrupt.
//!
//! ```ignore
//! sound::volume(15);
//! let v = Voice(0);
//! v.adsr(0, 9, 0, 0);
//! v.note_on(Note::C4, sound::PULSE);
//! ```
//!
//! The SID registers are write-only, so the module keeps copies of the
//! control and volume registers.

use crate::hw;
use crate::irq;
use crate::sys;

/// Waveforms and control bits ([`Voice::play`], [`Voice::note_on`]).
pub const GATE: u8 = 0x01;
pub const SYNC: u8 = 0x02;
pub const RING: u8 = 0x04;
pub const TEST: u8 = 0x08;
pub const TRIANGLE: u8 = 0x10;
pub const SAWTOOTH: u8 = 0x20;
pub const PULSE: u8 = 0x40;
pub const NOISE: u8 = 0x80;

/// Filter modes ([`filter`]).
pub const LOW_PASS: u8 = 0x10;
pub const BAND_PASS: u8 = 0x20;
pub const HIGH_PASS: u8 = 0x40;
pub const VOICE3_OFF: u8 = 0x80;
/// Filter routing ([`filter`]).
pub const FILTER_VOICE1: u8 = 1;
pub const FILTER_VOICE2: u8 = 2;
pub const FILTER_VOICE3: u8 = 4;

/// A note: `octave * 12 + semitone`, C0 = 0 ... B7 = 95.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Note(pub u8);

impl Note {
    /// The note `semitone` (0 = C ... 11 = B) of `octave` (0-7).
    pub const fn new(octave: u8, semitone: u8) -> Note {
        Note(octave * 12 + semitone)
    }

    pub const C3: Note = Note(36);
    pub const CS3: Note = Note(37);
    pub const D3: Note = Note(38);
    pub const DS3: Note = Note(39);
    pub const E3: Note = Note(40);
    pub const F3: Note = Note(41);
    pub const FS3: Note = Note(42);
    pub const G3: Note = Note(43);
    pub const GS3: Note = Note(44);
    pub const A3: Note = Note(45);
    pub const AS3: Note = Note(46);
    pub const B3: Note = Note(47);
    pub const C4: Note = Note(48);
    pub const CS4: Note = Note(49);
    pub const D4: Note = Note(50);
    pub const DS4: Note = Note(51);
    pub const E4: Note = Note(52);
    pub const F4: Note = Note(53);
    pub const FS4: Note = Note(54);
    pub const G4: Note = Note(55);
    pub const GS4: Note = Note(56);
    pub const A4: Note = Note(57);
    pub const AS4: Note = Note(58);
    pub const B4: Note = Note(59);
    pub const C5: Note = Note(60);

    /// The SID frequency (PAL).
    pub fn freq(self) -> u16 {
        NOTE_FREQ[self.0.min(95) as usize]
    }
}

/// Frequencies (PAL register values) of the notes C0..B7.
#[unsafe(export_name = "c64_sound_freq")]
static NOTE_FREQ: [u16; 96] = [
    278, 295, 313, 331, 351, 372, 394, 417, 442, 468, 496, 526, 557, 590, 625, 662, 702, 743, 788,
    834, 884, 937, 992, 1051, 1114, 1180, 1250, 1325, 1403, 1487, 1575, 1669, 1768, 1873, 1985,
    2103, 2228, 2360, 2500, 2649, 2807, 2973, 3150, 3338, 3536, 3746, 3969, 4205, 4455, 4720,
    5001, 5298, 5613, 5947, 6300, 6675, 7072, 7493, 7938, 8410, 8910, 9440, 10001, 10596, 11226,
    11894, 12601, 13350, 14144, 14985, 15876, 16820, 17820, 18880, 20003, 21192, 22452, 23787,
    25202, 26700, 28288, 29970, 31752, 33640, 35641, 37760, 40005, 42384, 44904, 47574, 50403,
    53401, 56576, 59940, 63504, 65535,
];

// Exported for the assembly routines (rat/sound.rat).
#[unsafe(export_name = "c64_sound_shadow")]
static mut CONTROL: [u8; 3] = [0; 3];
#[unsafe(export_name = "c64_sound_modevol")]
static mut MODE_VOL: u8 = 0;

#[cfg(target_os = "c64")]
crate::rat_source!(SOUND_RAT, "rat/sound.rat");

#[cfg(target_os = "c64")]
unsafe extern "C" {
    fn c64_sound_control(voice: u8, value: u8);
    fn c64_sound_note_off(voice: u8);
    fn c64_sound_reg16(voice: u8, reg: u8, value: u16);
    fn c64_sound_adsr(voice: u8, attack: u8, decay: u8, sustain: u8, release: u8);
    fn c64_sound_note_on(voice: u8, note: u8, wave: u8);
    fn c64_sound_reset();
}

/// Voice 0-2.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Voice(pub u8);

#[cfg(target_os = "c64")]
impl Voice {
    /// Sets the frequency register.
    pub fn freq(self, f: u16) {
        unsafe { c64_sound_reg16(self.0, 0, f) }
    }

    /// Sets the pulse width (0-4095).
    pub fn pulse_width(self, w: u16) {
        unsafe { c64_sound_reg16(self.0, 2, w) }
    }

    /// Sets the envelope (each 0-15).
    pub fn adsr(self, attack: u8, decay: u8, sustain: u8, release: u8) {
        unsafe { c64_sound_adsr(self.0, attack, decay, sustain, release) }
    }

    /// Starts the voice with a waveform at its current frequency.
    pub fn play(self, wave: u8) {
        unsafe { c64_sound_control(self.0, wave | GATE) }
    }

    /// Plays a note with a waveform.
    pub fn note_on(self, note: Note, wave: u8) {
        unsafe { c64_sound_note_on(self.0, note.0, wave) }
    }

    /// Starts the release.
    pub fn note_off(self) {
        unsafe { c64_sound_note_off(self.0) }
    }
}

/// Silences the SID.
#[cfg(target_os = "c64")]
pub fn reset() {
    unsafe { c64_sound_reset() }
}

#[cfg(not(target_os = "c64"))]
impl Voice {
    fn regs(self) -> &'static hw::Voice {
        &hw::sid().voice[(self.0 % 3) as usize]
    }

    fn set_control(self, c: u8) {
        unsafe { CONTROL[(self.0 % 3) as usize] = c };
        self.regs().control.write(c);
    }

    fn control(self) -> u8 {
        unsafe { CONTROL[(self.0 % 3) as usize] }
    }

    /// Sets the frequency register.
    pub fn freq(self, f: u16) {
        self.regs().freq.write(f);
    }

    /// Sets the pulse width (0-4095).
    pub fn pulse_width(self, w: u16) {
        self.regs().pulse.write(w);
    }

    /// Sets the envelope (each 0-15).
    pub fn adsr(self, attack: u8, decay: u8, sustain: u8, release: u8) {
        let r = self.regs();
        r.attack_decay.write(attack << 4 | decay & 15);
        r.sustain_release.write(sustain << 4 | release & 15);
    }

    /// Starts the voice with a waveform at its current frequency.
    pub fn play(self, wave: u8) {
        self.set_control(wave | GATE);
    }

    /// Plays a note with a waveform.
    pub fn note_on(self, note: Note, wave: u8) {
        self.freq(note.freq());
        self.play(wave);
    }

    /// Starts the release.
    pub fn note_off(self) {
        self.set_control(self.control() & !GATE);
    }
}

/// Silences the SID.
#[cfg(not(target_os = "c64"))]
pub fn reset() {
    let s = hw::sid();
    for v in 0..3 {
        let r = &s.voice[v];
        r.control.write(0);
        r.attack_decay.write(0);
        r.sustain_release.write(0);
        unsafe { CONTROL[v] = 0 };
    }
    unsafe { MODE_VOL = 0 };
    s.mode_volume.write(0);
    s.resonance.write(0);
}

/// Sets the main volume (0-15).
pub fn volume(v: u8) {
    unsafe {
        MODE_VOL = MODE_VOL & 0xf0 | v & 15;
        hw::sid().mode_volume.write(MODE_VOL);
    }
}

/// Sets the filter: cutoff (0-2047), resonance (0-15), the voices to
/// filter (`FILTER_VOICE1 | ...`) and the mode (`LOW_PASS`, ...).
pub fn filter(cutoff: u16, resonance: u8, voices: u8, mode: u8) {
    let s = hw::sid();
    s.cutoff.write(cutoff & 7 | (cutoff >> 3) << 8);
    s.resonance.write(resonance << 4 | voices & 15);
    unsafe {
        MODE_VOL = MODE_VOL & 15 | mode & 0xf0;
        s.mode_volume.write(MODE_VOL);
    }
}

/// A PSID tune placed at its load address: define it with
/// [`crate::sid!`].
pub struct Tune {
    pub init: u16,
    pub play: u16,
    pub songs: u8,
    /// The default song (0 = the first).
    pub start_song: u8,
    /// The tune's code and data (at its load address).
    pub data: &'static [u8],
}

static mut PLAYING: u16 = 0;

fn tick() {
    unsafe { sys::jsr(PLAYING, sys::Regs::default()) };
}

impl Tune {
    /// Initializes `song` (0 = the first) and calls the player once per
    /// frame from a raster interrupt (see [`crate::irq`]).
    pub fn start(&self, song: u8) {
        // Keeps the data in the program.
        let _ = core::hint::black_box(self.data.len());
        unsafe {
            sys::jsr(self.init, sys::Regs { a: song, ..sys::Regs::default() });
            PLAYING = self.play;
        }
        irq::on_frame(tick);
    }

    /// Removes all raster interrupts and silences the SID.
    pub fn stop(&self) {
        irq::stop();
        reset();
    }
}
