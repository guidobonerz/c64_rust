//! Cube3d: a cube that turns on the hires bitmap, without its hidden
//! edges. `--features pyramid` turns a pyramid, `--features perspective`
//! draws with a vanishing point.
//!
//! Every picture is a new rotation matrix from three angles. There are two
//! bitmaps (double buffering): the body is drawn into the one that is not
//! on the screen, which is shown when the picture is complete. Neither is
//! cleared (8000 bytes): each has the view that was drawn into it last,
//! and those edges are drawn again with pen 0 before the new ones.
#![no_std]
#![no_main]

use c64::g3d::{self, Matrix, View};
use c64::gfx;
use c64::hw::Color;
use c64::kernal;

#[cfg(not(feature = "pyramid"))]
use g3d::CUBE as BODY;
#[cfg(feature = "pyramid")]
use g3d::PYRAMID as BODY;

#[c64::main]
fn main() {
    let mut g = gfx::hires(Color::LightGreen, Color::Black).double_buffer();
    // what was drawn into each of the two bitmaps
    let mut views = [View::<8, 6>::new(), View::<8, 6>::new()];
    let mut hidden = 0;
    let (mut ax, mut ay, mut az) = (0u8, 0u8, 0u8);
    while !kernal::stop() {
        let view = &mut views[hidden];
        g.pen = 0;
        view.draw(&g, &BODY);
        #[cfg(not(feature = "perspective"))]
        view.project(&BODY, &Matrix::rotation(ax, ay, az), 160, 100);
        #[cfg(feature = "perspective")]
        view.project_perspective(&BODY, &Matrix::rotation(ax, ay, az), 160, 100, 127);
        g.pen = 1;
        view.draw(&g, &BODY);
        // (no wait for the frame: the change is one store to a register)
        g.flip();
        hidden ^= 1;
        ax = ax.wrapping_add(3);
        ay = ay.wrapping_add(5);
        az = az.wrapping_add(1);
    }
    drop(g);
    gfx::text();
}
