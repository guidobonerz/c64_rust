//! Dir: lists the directory of drive 8 and the drive status.
#![no_std]
#![no_main]

use c64::dos::{self, Dir};
use c64::text;

#[c64::main]
fn main() {
    let mut d = match Dir::open(2, 8) {
        Ok(d) => d,
        Err(e) => {
            c64::println!("cannot open directory, error ", e.0);
            return;
        }
    };
    c64::print!("disk: ");
    text::print_petscii(d.header.name());
    c64::println!();
    for e in &mut d {
        c64::print!(e.blocks, " ");
        text::print_petscii(e.name());
        c64::print!(" ");
        text::print_petscii(&e.kind);
        c64::println!();
    }
    c64::println!(d.free(), " blocks free");
    drop(d);

    let mut buf = [0u8; 40];
    if let Some((n, _)) = dos::status(8, &mut buf) {
        text::print_petscii(&buf[..n]);
        c64::println!();
    }
}
