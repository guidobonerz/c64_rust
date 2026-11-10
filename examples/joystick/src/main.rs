//! Joystick in port 2 moves a ball (fire turns it red), pressed keys are
//! shown, RUN/STOP returns to BASIC.
#![no_std]
#![no_main]

use c64::hw::{self, Color};
use c64::input::{self, Port};
use c64::{kernal, text};


const BALL: u8 = 81; // screen code of the filled circle
const TOP: u8 = 5;

#[c64::main]
fn main() {
    text::clear();
    c64::println!("joystick 2 moves the ball, keys are");
    c64::println!("shown, run/stop ends.");
    text::put_str(0, 3, "key:", Color::LightBlue);
    let (mut x, mut y) = (20u8, 14u8);
    let mut frame = 0u8;
    loop {
        hw::wait_frame();
        if kernal::stop() {
            break;
        }
        if let Some(k) = input::key() {
            text::put_char(5, 3, text::petscii_to_screen(k), Color::Yellow);
        }
        let j = input::joy(Port::Two);
        frame = frame.wrapping_add(1);
        if frame & 3 == 0 {
            text::put_char(x, y, b' ', Color::White);
            if j.up() && y > TOP {
                y -= 1;
            }
            if j.down() && y < text::HEIGHT - 1 {
                y += 1;
            }
            if j.left() && x > 0 {
                x -= 1;
            }
            if j.right() && x < text::WIDTH - 1 {
                x += 1;
            }
        }
        text::put_char(x, y, BALL, if j.fire() { Color::Red } else { Color::White });
    }
    text::clear();
}
