//! Hello: prints a greeting and the first Fibonacci numbers, then turns the
//! border green.
#![no_std]
#![no_main]

use c64::hw::{self, Color};

fn fib(n: u8) -> u16 {
    let (mut a, mut b) = (0u16, 1u16);
    for _ in 0..n {
        (a, b) = (b, a + b);
    }
    a
}

#[c64::main]
fn main() {
    c64::println!("hello from rust64");
    for i in 0..10 {
        c64::print!(fib(i), " ");
    }
    c64::println!();
    hw::vic().border.write(Color::Green);
}
