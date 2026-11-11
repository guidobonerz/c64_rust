//! The drawing example (examples/drawing) with its phases measured by the
//! CIA 2 timers; prints the cycles instead of waiting for a key. The
//! counterpart of `../oscar64/drawing.c -dTIMED`.
#![no_std]
#![no_main]

use c64::gfx;
use c64::hw::{self, Color};

/// CIA 2: timer A counts cycles, timer B its underflows.
fn start() {
    let c = hw::cia2();
    c.cra.write(0);
    c.crb.write(0);
    c.timer_a.write(0xffff);
    c.timer_b.write(0xffff);
    c.crb.write(0x51);
    c.cra.write(0x11);
}

fn stop() -> u32 {
    let c = hw::cia2();
    c.cra.write(0);
    let (a, b) = (c.timer_a.read(), c.timer_b.read());
    (!b as u32) << 16 | !a as u32
}

#[c64::main]
fn main() {
    hw::vic().border.write(Color::Black);

    start();
    let g = gfx::hires(Color::LightGreen, Color::Black);
    let setup = stop();

    start();
    for i in (0..320i16).step_by(16) {
        g.line(i, 0, 319 - i, 199);
    }
    let lines = stop();

    start();
    g.rect(40, 30, 240, 140);
    let rect = stop();

    start();
    for r in (10..=60i16).step_by(10) {
        g.circle(160, 100, r);
    }
    let circles = stop();

    start();
    for c in 0..40u8 {
        g.set_cell_colors(c, 24, Color::from_u8(c & 15 | 1), Color::Black);
    }
    let cells = stop();

    gfx::text();
    c64::println!("SETUP ", setup);
    c64::println!("LINES ", lines);
    c64::println!("RECT ", rect);
    c64::println!("CIRCLES ", circles);
    c64::println!("CELLS ", cells);
    c64::println!("TOTAL ", setup + lines + rect + circles + cells);
}
