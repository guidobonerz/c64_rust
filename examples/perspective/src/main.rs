//! Perspective: a long bar that turns, drawn with a vanishing point: the
//! end that points to the viewer is large, the far end small. Hold the
//! fire button of joystick 2 to see the same bar without it (a parallel
//! projection); RUN/STOP ends.
//!
//! As in `cube3d`, there are two bitmaps, and each has the view that was
//! drawn into it last: its edges are taken away before the next picture
//! is drawn there.
#![no_std]
#![no_main]

use c64::g3d::{Matrix, Shape, View};
use c64::gfx;
use c64::hw::{self, Color};
use c64::input::{self, Port};
use c64::kernal;

/// How strong the perspective is (0-127): the viewer is 256 units in
/// front of the middle of the bar.
const DEPTH: u8 = 127;

/// A bar of 36 x 36 x 170: the corners (x, y, z), the faces (three corners
/// each, counter-clockwise seen from outside) and the edges (two corners
/// and the two faces they belong to). No corner is further than 89 from
/// the origin, so the picture stays on the screen at every angle.
static BAR: Shape = Shape {
    vertices: &[
        [-18, -18, -85],
        [18, -18, -85],
        [-18, 18, -85],
        [18, 18, -85],
        [-18, -18, 85],
        [18, -18, 85],
        [-18, 18, 85],
        [18, 18, 85],
    ],
    // front (+z), back, right (+x), left, top (+y), bottom
    faces: &[[4, 5, 7], [1, 0, 2], [1, 3, 7], [0, 4, 6], [2, 6, 7], [0, 1, 5]],
    edges: &[
        [0, 1, 1, 5],
        [2, 3, 1, 4],
        [4, 5, 0, 5],
        [6, 7, 0, 4],
        [0, 2, 1, 3],
        [1, 3, 1, 2],
        [4, 6, 0, 3],
        [5, 7, 0, 2],
        [0, 4, 3, 5],
        [1, 5, 2, 5],
        [2, 6, 3, 4],
        [3, 7, 2, 4],
    ],
};

#[c64::main]
fn main() {
    let mut g = gfx::hires(Color::Yellow, Color::Black).double_buffer();
    // what was drawn into each of the two bitmaps
    let mut views = [View::<8, 6>::new(), View::<8, 6>::new()];
    let mut hidden = 0;
    let (mut ax, mut ay) = (0u8, 0u8);
    while !kernal::stop() {
        let view = &mut views[hidden];
        g.pen = 0;
        view.draw(&g, &BAR);
        let m = Matrix::rotation(ax, ay, 0);
        let depth = if input::joy(Port::Two).fire() { 0 } else { DEPTH };
        view.project_perspective(&BAR, &m, 160, 100, depth);
        g.pen = 1;
        view.draw(&g, &BAR);
        // change the bitmaps between two frames
        hw::wait_frame();
        g.flip();
        hidden ^= 1;
        ax = ax.wrapping_add(2);
        ay = ay.wrapping_add(3);
    }
    drop(g);
    gfx::text();
}
