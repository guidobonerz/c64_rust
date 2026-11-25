//! The Mandelbrot set
//!
//! Every point `c` of the picture is the start of its own series, from `z` = 0.
//!
//! The numbers are fixed point: 16 bits with 11 bits behind the point
//! (1.0 is 2048), products in 32 bits. A point belongs to the set if
//! `z = z * z + c` stays inside the circle of radius 2 for 16
//! rounds; the picture shows after how many rounds it left.
//!
//! * `cargo c64 run --release`: the text screen, 40 x 25 cells of the
//!   reversed space in the 16 colours (PETSCII).
//! * `--features multicolor`: the multicolour bitmap, 160 x 200 pixels in
//!   four colours.
//! * `--features timed`: measures the work with CIA 2's timers and prints
//!   the cycles and the sum of all rounds, in place of waiting for a key
//!   (for `bench/math.ps1`; the counterpart is
//!   `bench/math/oscar64/mandelbrot.c`).
#![no_std]
#![no_main]

#[cfg(feature = "multicolor")]
use c64::gfx;
use c64::hw::Color;
#[cfg(feature = "timed")]
use c64::hw;
#[cfg(not(feature = "timed"))]
use c64::kernal;
#[cfg(not(feature = "multicolor"))]
use c64::mem;
use c64::text;

/// The rounds after which a point counts as inside.
const MAX_ITER: u8 = 16;
/// 1.0 in fixed point.
const ONE: i32 = 1 << 11;

/// The size of the picture and the part of the plane it shows: the top
/// left corner and the steps per pixel.
#[cfg(not(feature = "multicolor"))]
const VIEW: (u8, u8, i16, i16, i16, i16) = (40, 25, -4506, -2400, 164, 200);
#[cfg(feature = "multicolor")]
const VIEW: (u8, u8, i16, i16, i16, i16) = (160, 200, -4506, -2488, 41, 25);

/// The rounds that `z = z * z + c` takes to leave the circle of radius 2,
/// starting at `z` = (`x`, `y`); `MAX_ITER` if it stays.
fn rounds(cx: i16, cy: i16, mut x: i16, mut y: i16) -> u8 {
    let mut n = 0;
    while n < MAX_ITER {
        let x2 = (x as i32 * x as i32) >> 11;
        let y2 = (y as i32 * y as i32) >> 11;
        if x2 + y2 > 4 * ONE {
            break;
        }
        let xy = (x as i32 * y as i32) >> 10;
        x = (x2 - y2) as i16 + cx;
        y = xy as i16 + cy;
        n += 1;
    }
    n
}

/// Draws the picture; the sum of the rounds of all points.
fn draw() -> u32 {
    let (w, h, x0, y0, dx, dy) = VIEW;
    #[cfg(feature = "multicolor")]
    let mut g = gfx::multicolor(Color::Black, Color::Blue, Color::LightBlue, Color::White);
    #[cfg(not(feature = "multicolor"))]
    let mut cell = {
        text::fill(160, Color::Black);
        0xd800u16
    };
    let mut sum = 0u32;
    let mut py = y0;
    for row in 0..h {
        let mut px = x0;
        for col in 0..w {
            let n = rounds(px, py, 0, 0);
            sum += n as u32;
            #[cfg(feature = "multicolor")]
            if n < MAX_ITER {
                g.pen = n % 3 + 1;
                g.plot(col as i16, row as i16);
            }
            #[cfg(not(feature = "multicolor"))]
            {
                let _ = (row, col);
                mem::poke(cell, if n < MAX_ITER { n % 15 + 1 } else { 0 });
                cell += 1;
            }
            px += dx;
        }
        py += dy;
    }
    sum
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
    text::colors(Color::Black, Color::Black);
    #[cfg(feature = "timed")]
    start();
    let sum = draw();
    #[cfg(feature = "timed")]
    let time = stop();
    #[cfg(not(feature = "timed"))]
    {
        let _ = sum;
        while kernal::getin() == 0 {}
    }
    #[cfg(feature = "multicolor")]
    gfx::text();
    text::set_color(Color::LightBlue);
    text::clear();
    #[cfg(feature = "timed")]
    {
        c64::println!("TIME ", time);
        c64::println!("SUM ", sum);
    }
}
