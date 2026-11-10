//! Music: plays a melody on voice 1 with a bass on voice 2, timed with the
//! jiffy clock.
#![no_std]
#![no_main]

use c64::hw::{self, Color};
use c64::sound::{self, Note, Voice};
use c64::{kernal, text};

/// A note: pitch (`None` = rest) and length in jiffies.
#[derive(Clone, Copy)]
struct Tone(Option<Note>, u8);

const fn n(note: Note, len: u8) -> Tone {
    Tone(Some(note), len)
}

static MELODY: [Tone; 26] = [
    n(Note::E4, 15), n(Note::D4, 15), n(Note::C4, 15), n(Note::D4, 15),
    n(Note::E4, 15), n(Note::E4, 15), n(Note::E4, 30),
    n(Note::D4, 15), n(Note::D4, 15), n(Note::D4, 30),
    n(Note::E4, 15), n(Note::G4, 15), n(Note::G4, 30),
    n(Note::E4, 15), n(Note::D4, 15), n(Note::C4, 15), n(Note::D4, 15),
    n(Note::E4, 15), n(Note::E4, 15), n(Note::E4, 15), n(Note::E4, 15),
    n(Note::D4, 15), n(Note::D4, 15), n(Note::E4, 15), n(Note::D4, 15),
    n(Note::C4, 60),
];

static BASS: [Note; 4] = [Note::C3, Note::G3, Note::A3, Note::F3];

fn wait(jiffies: u8) {
    let start = kernal::jiffies();
    while kernal::jiffies().wrapping_sub(start) < jiffies as u16 {}
}

#[c64::main]
fn main() {
    text::colors(Color::Blue, Color::Black);
    text::clear();
    c64::println!("rust64 music");
    sound::reset();
    sound::volume(15);
    let (lead, bass) = (Voice(0), Voice(1));
    lead.adsr(0, 9, 4, 4);
    lead.pulse_width(0x800);
    bass.adsr(0, 4, 10, 6);
    for (i, t) in MELODY.iter().enumerate() {
        if i % 4 == 0 {
            bass.note_on(BASS[(i / 4) % 4], sound::TRIANGLE);
        }
        if let Some(note) = t.0 {
            lead.note_on(note, sound::PULSE);
            hw::vic().border.write(Color::from_u8(note.0));
        }
        wait(t.1 - 3);
        lead.note_off();
        wait(3);
    }
    bass.note_off();
    wait(60);
    sound::reset();
    c64::println!("done");
}
