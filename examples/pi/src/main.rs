//! Pi: the first 100 digits, with the spigot algorithm of Rabinowitz and
//! Wagon in its well-known short form. It works on an array of 16-bit
//! numbers with 32-bit multiplications and divisions and gives four digits
//! per pass.
//!
//! * `cargo c64 run --release`: prints the digits.
//! * `--features timed`: measures the computation (not the printing) with
//!   CIA 2's timers and prints the cycles and the sum of the 25 groups of
//!   four digits (for `bench/math.ps1`; the counterpart is
//!   `bench/math/oscar64/pi.c`).
#![no_std]
#![no_main]

#[cfg(feature = "timed")]
use c64::hw;
use c64::kernal;

const DIGITS: usize = 100;
/// Four digits per pass.
const GROUPS: usize = DIGITS / 4;
/// 14 numbers per group.
const LEN: usize = GROUPS * 14;

static mut F: [u16; LEN + 1] = [0; LEN + 1];
static mut OUT: [u16; GROUPS] = [0; GROUPS];

/// Computes the digits, four per element of `out`.
fn compute(f: &mut [u16; LEN + 1], out: &mut [u16; GROUPS]) {
    for v in f[..LEN].iter_mut() {
        *v = 2000;
    }
    f[LEN] = 0;
    let mut e = 0u16;
    let mut c = LEN;
    for group in out.iter_mut() {
        let mut d = 0u32;
        let mut g = (c * 2) as u32;
        let mut b = c;
        loop {
            d += f[b] as u32 * 10000;
            g -= 1;
            f[b] = (d % g) as u16;
            d /= g;
            g -= 1;
            b -= 1;
            if b == 0 {
                break;
            }
            d *= b as u32;
        }
        *group = e + (d / 10000) as u16;
        e = (d % 10000) as u16;
        c -= 14;
    }
}

/// Prints `v` with four digits.
fn print4(mut v: u16) {
    for unit in [1000u16, 100, 10, 1] {
        let mut digit = b'0';
        while v >= unit {
            v -= unit;
            digit += 1;
        }
        kernal::chrout(digit);
    }
}

/// CIA 2: timer A counts cycles, timer B its underflows.
#[cfg(feature = "timed")]
fn start() {
    let c = hw::cia2();
    c.cra.write(0);
    c.crb.write(0);
    c.timer_a.write(0xffff);
    c.timer_b.write(0xffff);
    c.crb.write(0x51);
    c.cra.write(0x11);
}

#[cfg(feature = "timed")]
fn stop() -> u32 {
    let c = hw::cia2();
    c.cra.write(0);
    let (a, b) = (c.timer_a.read(), c.timer_b.read());
    (!b as u32) << 16 | !a as u32
}

#[c64::main]
fn main() {
    let (f, out) = unsafe { (&mut *(&raw mut F), &mut *(&raw mut OUT)) };
    #[cfg(feature = "timed")]
    start();
    compute(f, out);
    #[cfg(feature = "timed")]
    let time = stop();
    kernal::chrout(kernal::CLEAR);
    for &group in out.iter() {
        print4(group);
    }
    kernal::chrout(kernal::RETURN);
    #[cfg(feature = "timed")]
    {
        let mut sum = 0u32;
        for &group in out.iter() {
            sum += group as u32;
        }
        c64::println!("TIME ", time);
        c64::println!("SUM ", sum);
    }
}
