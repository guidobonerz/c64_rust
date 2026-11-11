//! End-to-end test of `c64-uaudio`. mimic has no Ultimate Audio, so this
//! only checks what can be checked without it: the sampler is reported as
//! absent, the rate values are right, and the calls come back.
//! Border green = all passed.
#![no_std]
#![no_main]

use c64::hw::{self, Color};
use c64_uaudio::{self as ua, Voice};
use core::hint::black_box as bb;

#[c64::main]
fn main() {
    const R22: u16 = ua::rate(22050);
    let mut ok = !ua::present();
    ok &= R22 == 281 && ua::rate(bb(11025)) == 564 && ua::rate(8000) == 779;
    // Nothing is mapped at $DF20: the writes go nowhere.
    let v = Voice(bb(2));
    v.sample(0x012345, 8000);
    v.rate(R22);
    v.volume(40);
    v.pan(ua::CENTER);
    v.repeat(0, 4000);
    v.play(ua::REPEAT);
    v.stop();
    ua::stop_all();
    hw::vic().border.write(if ok { Color::Green } else { Color::Red });
}
