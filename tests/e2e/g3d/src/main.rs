//! End-to-end test of c64::g3d: the sine, the rotation matrix, the corners
//! on the screen and the faces that are seen, against the numbers of the
//! Rust version (the host test `g3d::tests::reference_values`), and the
//! edges on the bitmap. Border green = all passed; the number of the first
//! failing check is at $0400.
#![no_std]
#![no_main]

use c64::g3d::{self, Matrix, View};
use c64::gfx;
use c64::hw::{self, Color};
use c64::mem;
use core::hint::black_box as bb;

static mut TEST: u8 = 0;
static mut FAILS: u8 = 0;

fn check(ok: bool) {
    unsafe {
        TEST += 1;
        if !ok && FAILS == 0 {
            FAILS = TEST;
        }
    }
}

const MATRIX: [i8; 9] = [56, 106, 38, -42, 59, -102, -105, 33, 62];
const POINTS: [(i16, i16); 8] =
    [(74, 63), (122, 100), (165, 13), (213, 49), (107, 151), (155, 187), (198, 100), (246, 137)];
const SEEN: [bool; 6] = [true, false, false, true, true, false];
const PERSPECTIVE: [(i16, i16); 8] =
    [(73, 62), (131, 100), (166, 1), (203, 59), (92, 166), (156, 177), (217, 100), (245, 136)];

/// Lines in the top four cell rows: x0, y0, x1, y1.
const LINES: [(i16, i16, i16, i16); 15] = [
    // flat ones that start at every kind of bit of a byte
    (0, 4, 60, 20),
    (3, 10, 61, 20),
    (7, 12, 200, 25),
    (64, 5, 71, 7),
    (129, 31, 255, 0),
    (22, 1, 21, 0),
    (10, 2, 250, 30),
    (250, 2, 10, 30),
    (10, 30, 250, 2),
    (100, 1, 110, 31),
    (110, 1, 100, 31),
    (100, 31, 110, 1),
    (50, 0, 81, 31),
    (5, 5, 6, 7),
    (290, 0, 319, 31),
];

/// The pixels that are set in the top four cell rows of the bitmap.
fn pixels() -> u16 {
    let mut n = 0u16;
    let mut a = gfx::BITMAP;
    while a < gfx::BITMAP + 4 * 320 {
        n += mem::peek(a).count_ones() as u16;
        a += 1;
    }
    n
}

#[c64::main]
fn main() {
    // 1-2: the sine in all four quarters, the cosine
    check(g3d::sin(bb(0)) == 0 && g3d::sin(bb(64)) == 127 && g3d::sin(bb(128)) == 0 && g3d::sin(bb(192)) == -127);
    check(
        g3d::sin(bb(32)) == 90
            && g3d::sin(bb(96)) == 90
            && g3d::sin(bb(160)) == -90
            && g3d::sin(bb(224)) == -90
            && g3d::cos(bb(0)) == 127
            && g3d::cos(bb(128)) == -127,
    );
    // 3: a matrix from three angles
    let m = Matrix::rotation(bb(20), bb(40), bb(230));
    check(m.0 == MATRIX);
    // 4-5: the corners of the cube on the screen, and its faces
    let mut view = View::<8, 6>::new();
    view.project(&g3d::CUBE, &m, 160, 100);
    let mut same = true;
    for (i, p) in view.points.iter().enumerate() {
        same &= (p.x, p.y) == POINTS[i];
    }
    check(same);
    let mut same = true;
    for (i, &s) in SEEN.iter().enumerate() {
        same &= view.is_seen(i as u8) == s;
    }
    check(same);
    // 6: seen from the front: the front face only
    view.project(&g3d::CUBE, &Matrix::IDENTITY, 160, 100);
    check(view.is_seen(0) && !view.is_seen(1) && !view.is_seen(2) && !view.is_seen(3) && !view.is_seen(4) && !view.is_seen(5));
    // 7-8: drawn: the front square (corners at 160 +- 55, 100 +- 55) and
    // none of the edges behind it; taken away again with pen 0
    let mut g = gfx::hires(Color::White, Color::Black);
    view.draw(&g, &g3d::CUBE);
    check(
        g.point(105, 45) && g.point(215, 155) && g.point(160, 45) && g.point(105, 100) && !g.point(160, 100) && !g.point(104, 44),
    );
    g.pen = 0;
    view.draw(&g, &g3d::CUBE);
    check(!g.point(105, 45) && !g.point(215, 155) && !g.point(160, 45));
    // 9: turned a little: the edges that meet in the corner at the back
    // (corner 0, behind the three faces that are seen) are not drawn
    g.pen = 1;
    let m = Matrix::rotation(bb(20), bb(236), 0);
    view.project(&g3d::CUBE, &m, 160, 100);
    view.draw(&g, &g3d::CUBE);
    let seen = view.is_seen(0) && view.is_seen(2) && view.is_seen(4);
    let (back, front) = (view.points[0], view.points[7]);
    check(seen && !g.point(back.x as u16, back.y as u8) && g.point(front.x as u16, front.y as u8));
    // 10: the pyramid from the front, and from below
    let mut p = View::<5, 5>::new();
    p.project(&g3d::PYRAMID, &Matrix::IDENTITY, 160, 100);
    let front_only = !p.is_seen(0) && p.is_seen(1) && !p.is_seen(2) && !p.is_seen(3) && !p.is_seen(4);
    p.project(&g3d::PYRAMID, &Matrix::rotation(bb(192), 0, 0), 160, 100);
    check(front_only && p.is_seen(0));
    // 12-13: with a vanishing point: the corners against the Rust version,
    // the same faces; with depth 0 the corners of the parallel projection;
    // from the front, the front face is larger than the one at the back
    let m = Matrix::rotation(bb(20), bb(40), bb(230));
    view.project_perspective(&g3d::CUBE, &m, 160, 100, bb(127));
    let mut same = true;
    for (i, p) in view.points.iter().enumerate() {
        same &= (p.x, p.y) == PERSPECTIVE[i];
    }
    for (i, &s) in SEEN.iter().enumerate() {
        same &= view.is_seen(i as u8) == s;
    }
    check(same);
    view.project_perspective(&g3d::CUBE, &m, 160, 100, bb(0));
    let mut same = true;
    for (i, p) in view.points.iter().enumerate() {
        same &= (p.x, p.y) == POINTS[i];
    }
    view.project_perspective(&g3d::CUBE, &Matrix::IDENTITY, 160, 100, bb(127));
    let (front, back) = (view.points[7].x - view.points[4].x, view.points[3].x - view.points[0].x);
    check(same && front > 110 && back < 110 && front > back + 30);
    // 11: the lines of the edges, which have loops of their own: for
    // every kind (flat and steep, down and up, both orders of the ends,
    // beyond x = 255) both ends are set, there are as many pixels as the
    // longer side has, and pen 0 takes all of them away again
    g.clear();
    let mut wire = View::<2, 1>::new();
    let shape = g3d::Shape { vertices: &[[0; 3]; 2], faces: &[], edges: &[[0, 1, g3d::NO_FACE, g3d::NO_FACE]] };
    let mut lines_ok = true;
    for &(x0, y0, x1, y1) in &LINES {
        wire.points[0] = g3d::Point { x: x0, y: y0 };
        wire.points[1] = g3d::Point { x: x1, y: y1 };
        g.pen = 1;
        wire.draw(&g, &shape);
        let (dx, dy) = ((x1 - x0).unsigned_abs(), (y1 - y0).unsigned_abs());
        let longer = if dx > dy { dx } else { dy };
        lines_ok &= g.point(x0 as u16, y0 as u8) && g.point(x1 as u16, y1 as u8) && pixels() == longer + 1;
        g.pen = 0;
        wire.draw(&g, &shape);
        lines_ok &= pixels() == 0;
    }
    check(lines_ok);
    gfx::text();
    unsafe {
        mem::poke(0x0400, FAILS);
        hw::vic().border.write(if FAILS == 0 { Color::Green } else { Color::Red });
    }
}
