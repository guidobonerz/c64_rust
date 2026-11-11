//! UI: boxes drawn with the PETSCII line characters: square and round
//! corners, with titles, and a panel on top of the others.
#![no_std]
#![no_main]

use c64::hw::Color;
use c64::text;
use c64::ui::{self, Border};

#[c64::main]
fn main() {
    text::colors(Color::Black, Color::Black);
    text::fill(0x20, Color::LightGrey);

    ui::titled_frame(0, 0, 40, 25, Border::Round, Color::Blue, " C64-UI ");

    ui::frame(2, 2, 17, 7, Border::Square, Color::White);
    text::put_str(4, 4, "SQUARE", Color::LightGrey);

    ui::frame(21, 2, 17, 7, Border::Round, Color::Yellow);
    text::put_str(23, 4, "ROUND", Color::LightGrey);

    ui::titled_frame(2, 10, 17, 7, Border::Square, Color::Cyan, " FILES ");
    text::put_str(4, 12, "HELLO.PRG", Color::LightGrey);
    text::put_str(4, 13, "NOTES.SEQ", Color::LightGrey);

    ui::titled_frame(21, 10, 17, 7, Border::Round, Color::Green, " STATUS ");
    text::put_str(23, 12, "ALL IS WELL", Color::LightGrey);

    // A panel clears what is under it.
    ui::titled_panel(11, 15, 18, 6, Border::Round, Color::LightRed, " QUIT? ");
    text::put_str(14, 17, "YES     NO", Color::White);

    loop {}
}
