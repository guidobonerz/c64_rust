//! Sprites: move a ball with the joystick in port 2; a raster interrupt
//! paints a colour band behind the title line. Fire changes the colour.
#![no_std]
#![no_main]

use c64::hw::{self, Color};
use c64::input::{self, Port};
use c64::sprite::{self, Sprite};
use c64::{irq, text};

/// A 24x21 ball, 3 bytes per line.
static BALL: [u8; 63] = [
    0x00, 0x7e, 0x00, 0x03, 0xff, 0xc0, 0x07, 0xff, 0xe0, 0x1f, 0xff, 0xf8, 0x1f, 0xff, 0xf8,
    0x3f, 0xff, 0xfc, 0x7f, 0xff, 0xfe, 0x7f, 0xff, 0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f, 0xff, 0xfe, 0x7f, 0xff, 0xfe,
    0x3f, 0xff, 0xfc, 0x1f, 0xff, 0xf8, 0x1f, 0xff, 0xf8, 0x07, 0xff, 0xe0, 0x03, 0xff, 0xc0,
    0x00, 0x7e, 0x00,
];

fn band_on() {
    hw::vic().background[0].write(Color::Blue);
}

fn band_off() {
    hw::vic().background[0].write(Color::Black);
}

#[c64::main]
fn main() {
    text::colors(Color::Black, Color::Black);
    text::clear();
    text::put_str(11, 1, "rust64 sprite demo", Color::White);
    text::put_str(8, 23, "joystick 2 - fire: colour", Color::Grey);

    sprite::load(13, &BALL);
    let ball = Sprite(0);
    ball.set_image(13);
    ball.set_color(Color::Yellow);
    ball.show();

    irq::add(50, band_on);
    irq::add(66, band_off);

    let (mut x, mut y) = (160u16, 140u8);
    let mut color = Color::Yellow as u8;
    let mut fire_held = false;
    loop {
        hw::wait_frame();
        let j = input::joy(Port::Two);
        if j.left() && x > 24 {
            x -= 2;
        }
        if j.right() && x < 320 {
            x += 2;
        }
        if j.up() && y > 50 {
            y -= 2;
        }
        if j.down() && y < 229 {
            y += 2;
        }
        let fire = j.fire();
        if fire && !fire_held {
            color = (color + 1) & 15;
            if color == Color::Black as u8 {
                color = Color::White as u8;
            }
            ball.set_color(Color::from_u8(color));
        }
        fire_held = fire;
        ball.move_to(x, y);
    }
}
