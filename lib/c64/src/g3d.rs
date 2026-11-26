//! Bodies in three dimensions on the hires bitmap: corners, edges and
//! faces, turned by a rotation matrix and drawn without the edges that are
//! hidden.
//!
//! ```ignore
//! let mut g = gfx::hires(Color::White, Color::Black);
//! let m = Matrix::rotation(20, 40, 0);          // angles: 256 is a full turn
//! let mut view = View::<8, 6>::new();           // room for 8 corners, 6 faces
//! view.project(&g3d::CUBE, &m, 160, 100);       // the corners on the screen
//! view.draw(&g, &g3d::CUBE);                    // the edges that are seen
//! ```
//!
//! # The numbers
//!
//! Everything is a signed byte. A corner has the coordinates x (to the
//! right), y (up) and z (towards the viewer), each -127 to 127, and must not
//! be further than 120 from the origin. In a [`Matrix`] and for [`sin`] and
//! [`cos`], 127 is 1.0. [`View::project`] is a parallel projection: a
//! corner lands `x` to the right of the centre and `y` above it, one pixel
//! for each unit, whatever its depth. [`View::project_perspective`] has a
//! vanishing point: what is further away is smaller.
//!
//! # Hidden edges
//!
//! A [`Shape`] names, for every edge, the two faces it belongs to. A face is
//! seen when its front points to the viewer, and an edge is drawn when one
//! of its faces is seen. That removes exactly the hidden edges of a convex
//! body (a cube, a pyramid, a prism). A body with dents shows the edges of
//! all faces that point to the viewer, also where another part is in front
//! of them. An edge without faces ([`NO_FACE`]) is always drawn: a shape
//! with no faces is a wire model.
//!
//! # Moving pictures
//!
//! Clearing the bitmap takes five frames. To move a body, draw it again
//! with pen 0 from the [`View`] it was drawn with, then draw the next
//! view (`examples/cube3d`). Taking away clears whole bytes of the bitmap
//! along the edges (see [`View::draw`]).
//!
//! # Memory
//!
//! The multiplications use a table of squares, the corners a work area and
//! the perspective a table: 2.5 KB of RAM at `$9600`-`$9FFF`, filled when
//! the first routine of this module runs, and the zero-page bytes `$F7`/`$F8` (the KERNAL's for
//! RS-232). Keep both free, and do not call this module from an interrupt
//! handler while the main program is in it.
//!
//! # Faster lines
//!
//! The feature `g3d-fast` of this crate draws flat lines (more wide than
//! high) with a piece of code for each bit of a byte: 29 cycles a pixel in
//! place of 38, for 180 bytes more in the program.
//!
//! ```toml
//! c64 = { path = "...", features = ["g3d-fast"] }
//! ```
//!
//! On the C64 all of it is assembly (`rat/g3d.rat`), with line loops of
//! its own for the edges; lines that leave the screen, and horizontal and
//! vertical ones, are those of [`Bitmap::line`](crate::gfx::Bitmap::line).

use crate::gfx::{Bitmap, Hires, Mode};

/// The face number of an edge that has no (second) face.
pub const NO_FACE: u8 = 0xff;

/// 127 * the sine of the angles 0 to 64 (a quarter turn).
#[unsafe(export_name = "c64_g3d_quarter")]
static QUARTER: [i8; 65] = [
    0, 3, 6, 9, 12, 16, 19, 22, 25, 28, 31, 34, 37, 40, 43, 46, 49, 51, 54, 57, 60, 63, 65, 68, 71,
    73, 76, 78, 81, 83, 85, 88, 90, 92, 94, 96, 98, 100, 102, 104, 106, 107, 109, 111, 112, 113,
    115, 116, 117, 118, 120, 121, 122, 122, 123, 124, 125, 125, 126, 126, 126, 127, 127, 127, 127,
];

#[cfg(target_os = "c64")]
crate::rat_source!(G3D_RAT, "rat/g3d.rat");
#[cfg(target_os = "c64")]
crate::rat_source!(G3D_LINE_RAT, "rat/g3d_line.rat");

// The feature `g3d-fast`: the definition that selects the longer and
// faster code for flat lines (see rat/g3d.rat).
#[cfg(all(target_os = "c64", feature = "g3d-fast"))]
#[doc(hidden)]
#[allow(dead_code)]
#[unsafe(link_section = "rat")]
static G3D_FAST_RAT: [u8; 17] = *b"C64_G3D_FAST = 1\n";

#[cfg(target_os = "c64")]
#[allow(dead_code)]
unsafe extern "C" {
    fn c64_g3d_sin(angle: u8) -> i8;
    fn c64_g3d_cos(angle: u8) -> i8;
    fn c64_g3d_turn(m: *mut i8, axis: u8, angle: u8);
    fn c64_g3d_rotation(m: *mut i8, ax: u8, ay: u8, az: u8);
    fn c64_g3d_apply(m: *const i8, v: *const i8, out: *mut i8);
    fn c64_g3d_view(view: *mut u8, shape: *const u8, m: *const i8, cx: i16, cy: i16);
    fn c64_g3d_viewp(view: *mut u8, shape: *const u8, m: *const i8, cx: i16, cy: i16, depth: u8);
    fn c64_g3d_draw(view: *const u8, shape: *const u8, pattern: u8);
    // The parts of the two above (they pass their arguments on to these).
    fn c64_g3d_project(m: *const i8, corners: *const i8, n: u8, points: *mut Point, cx: i16, cy: i16);
    fn c64_g3d_faces(points: *const Point, faces: *const [u8; 3], n: u8, seen: *mut u8);
    fn c64_g3d_edges(points: *const Point, edges: *const [u8; 4], n: u8, seen: *const u8, pattern: u8);
}

/// 127 * the sine of an angle; 256 is a full turn (64 a right angle).
pub fn sin(angle: u8) -> i8 {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_g3d_sin(angle)
    }
    #[cfg(not(target_os = "c64"))]
    {
        let place = angle & 0x7f;
        let v = QUARTER[if place > 64 { 128 - place } else { place } as usize];
        if angle >= 0x80 { -v } else { v }
    }
}

/// 127 * the cosine of an angle; 256 is a full turn.
pub fn cos(angle: u8) -> i8 {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_g3d_cos(angle)
    }
    #[cfg(not(target_os = "c64"))]
    sin(angle.wrapping_add(64))
}

/// (sum + 64) / 128, rounded down: a sum of products back to a byte.
#[cfg(not(target_os = "c64"))]
fn round(sum: i16) -> i8 {
    ((sum + 64) >> 7) as i8
}

/// A rotation: three rows of three numbers, 127 is 1.0. Row 0 gives the x
/// of a turned corner, row 1 its y and row 2 its z.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(transparent)]
pub struct Matrix(pub [i8; 9]);

impl Matrix {
    /// Leaves every corner where it is.
    pub const IDENTITY: Matrix = Matrix([127, 0, 0, 0, 127, 0, 0, 0, 127]);

    /// The rotation by `ax` about the x axis, then `ay` about the y axis,
    /// then `az` about the z axis (256 is a full turn). Make a new matrix
    /// from the angles for every picture: one that is turned on and on
    /// collects the rounding of every step.
    pub fn rotation(ax: u8, ay: u8, az: u8) -> Matrix {
        #[cfg(target_os = "c64")]
        unsafe {
            let mut m = core::mem::MaybeUninit::<Matrix>::uninit();
            c64_g3d_rotation(m.as_mut_ptr().cast(), ax, ay, az);
            m.assume_init()
        }
        #[cfg(not(target_os = "c64"))]
        {
            let mut m = Matrix::IDENTITY;
            m.turn_x(ax);
            m.turn_y(ay);
            m.turn_z(az);
            m
        }
    }

    /// Adds a turn about the x axis to what the matrix does.
    pub fn turn_x(&mut self, angle: u8) {
        self.turn(0, angle);
    }

    /// Adds a turn about the y axis.
    pub fn turn_y(&mut self, angle: u8) {
        self.turn(1, angle);
    }

    /// Adds a turn about the z axis.
    pub fn turn_z(&mut self, angle: u8) {
        self.turn(2, angle);
    }

    fn turn(&mut self, axis: u8, angle: u8) {
        #[cfg(target_os = "c64")]
        unsafe {
            c64_g3d_turn(self.0.as_mut_ptr(), axis, angle)
        }
        #[cfg(not(target_os = "c64"))]
        {
            let (i, j) = [(3, 6), (6, 0), (0, 3)][axis as usize % 3];
            let (s, c) = (sin(angle) as i16, cos(angle) as i16);
            for k in 0..3 {
                let (a, b) = (self.0[i + k] as i16, self.0[j + k] as i16);
                self.0[i + k] = round(c * a - s * b);
                self.0[j + k] = round(s * a + c * b);
            }
        }
    }

    /// The corner `v` (x, y, z) turned.
    pub fn apply(&self, v: [i8; 3]) -> [i8; 3] {
        #[cfg(target_os = "c64")]
        unsafe {
            let mut out = core::mem::MaybeUninit::<[i8; 3]>::uninit();
            c64_g3d_apply(self.0.as_ptr(), v.as_ptr(), out.as_mut_ptr().cast());
            out.assume_init()
        }
        #[cfg(not(target_os = "c64"))]
        {
            let mut out = [0i8; 3];
            for (r, o) in out.iter_mut().enumerate() {
                let mut sum = 0i16;
                for k in 0..3 {
                    sum += self.0[r * 3 + k] as i16 * v[k] as i16;
                }
                *o = round(sum);
            }
            out
        }
    }
}

/// A corner on the screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct Point {
    pub x: i16,
    pub y: i16,
}

/// A body: its corners, faces and edges. Up to 64 corners, 127 faces and
/// 255 edges. (rat/g3d.rat knows the layout.)
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Shape<'a> {
    /// The corners: x (right), y (up), z (towards the viewer).
    pub vertices: &'a [[i8; 3]],
    /// For each face three of its corners, counter-clockwise when one
    /// looks at the face from outside the body.
    pub faces: &'a [[u8; 3]],
    /// For each edge its two corners and the two faces it belongs to
    /// ([`NO_FACE`] where there is none).
    pub edges: &'a [[u8; 4]],
}

/// A cube with edges of 110, around the origin. Its faces: 0 front (+z),
/// 1 back, 2 right (+x), 3 left, 4 top (+y), 5 bottom.
pub static CUBE: Shape<'static> = Shape {
    vertices: &[
        [-55, -55, -55],
        [55, -55, -55],
        [-55, 55, -55],
        [55, 55, -55],
        [-55, -55, 55],
        [55, -55, 55],
        [-55, 55, 55],
        [55, 55, 55],
    ],
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

/// A pyramid on a square base of 120, 110 high. Its faces: 0 the base,
/// 1 front, 2 right, 3 back, 4 left.
pub static PYRAMID: Shape<'static> = Shape {
    vertices: &[[-60, -45, 60], [60, -45, 60], [60, -45, -60], [-60, -45, -60], [0, 65, 0]],
    faces: &[[0, 3, 2], [0, 1, 4], [1, 2, 4], [2, 3, 4], [3, 0, 4]],
    edges: &[
        [0, 1, 0, 1],
        [1, 2, 0, 2],
        [2, 3, 0, 3],
        [3, 0, 0, 4],
        [0, 4, 4, 1],
        [1, 4, 1, 2],
        [2, 4, 2, 3],
        [3, 4, 3, 4],
    ],
};

/// A body as it is seen: the places of up to `V` corners on the screen,
/// and which of up to `F` faces show their front (`V` up to 64, `F` up to
/// 127). (rat/g3d.rat knows the layout.)
#[repr(C)]
pub struct View<const V: usize, const F: usize> {
    corners: u8,
    faces: u8,
    /// The corners on the screen, after [`View::project`].
    pub points: [Point; V],
    seen: [u8; F],
}

impl<const V: usize, const F: usize> View<V, F> {
    /// A view of nothing: drawing it draws no edge that has a face.
    pub const fn new() -> Self {
        assert!(V <= 64 && F <= 127);
        View { corners: V as u8, faces: F as u8, points: [Point { x: 0, y: 0 }; V], seen: [0; F] }
    }

    /// Turns the corners of `shape` with `m`, puts them on the screen
    /// around (`cx`, `cy`) and finds the faces that are seen. Corners and
    /// faces beyond `V` and `F` are left out.
    pub fn project(&mut self, shape: &Shape, m: &Matrix, cx: i16, cy: i16) {
        #[cfg(target_os = "c64")]
        unsafe {
            c64_g3d_view((self as *mut Self).cast(), (shape as *const Shape).cast(), m.0.as_ptr(), cx, cy)
        }
        #[cfg(not(target_os = "c64"))]
        {
            let nv = shape.vertices.len().min(self.corners as usize);
            for i in 0..nv {
                let v = m.apply(shape.vertices[i]);
                self.points[i] = Point { x: cx + v[0] as i16, y: cy - v[1] as i16 };
            }
            self.faces_seen(shape);
        }
    }

    /// [`View::project`] with a vanishing point: what is further away is
    /// smaller. `depth` (0 to 127) says how much: the viewer is
    /// 256 * 128 / `depth` units in front of the origin (256 for 127, 512
    /// for 64), and what is at the origin keeps its size. With 0 this is
    /// [`View::project`].
    ///
    /// A corner at depth `z` (towards the viewer) is drawn
    /// 256 / (256 - z * depth / 128) times as far from the centre: up to
    /// twice for the nearest corners. The picture of a body must stay less
    /// than 256 pixels wide and high.
    pub fn project_perspective(&mut self, shape: &Shape, m: &Matrix, cx: i16, cy: i16, depth: u8) {
        let depth = if depth > 127 { 127 } else { depth };
        #[cfg(target_os = "c64")]
        unsafe {
            c64_g3d_viewp((self as *mut Self).cast(), (shape as *const Shape).cast(), m.0.as_ptr(), cx, cy, depth)
        }
        #[cfg(not(target_os = "c64"))]
        {
            let nv = shape.vertices.len().min(self.corners as usize);
            for i in 0..nv {
                let v = m.apply(shape.vertices[i]);
                // the depth, and the size of what is there (128 is 1.0)
                let z = round(v[2] as i16 * depth as i16) as i16;
                let size = (32768u32 / (256 - z) as u32).min(255) as i16;
                let scaled = |n: i8| {
                    let far = (n.unsigned_abs() as i16 * size + 64) >> 7;
                    if n < 0 { -far } else { far }
                };
                self.points[i] = Point { x: cx + scaled(v[0]), y: cy - scaled(v[1]) };
            }
            self.faces_seen(shape);
        }
    }

    /// The faces of `shape` that show their front, from the points.
    #[cfg(not(target_os = "c64"))]
    fn faces_seen(&mut self, shape: &Shape) {
        let nf = shape.faces.len().min(self.faces as usize);
        for i in 0..nf {
            let [a, b, c] = shape.faces[i];
            let (a, b, c) = (self.points[a as usize], self.points[b as usize], self.points[c as usize]);
            let left = (b.x - a.x) as i32 * (c.y - a.y) as i32;
            let right = (b.y - a.y) as i32 * (c.x - a.x) as i32;
            self.seen[i] = (left < right) as u8;
        }
    }

    /// Whether face `face` showed its front in the last [`View::project`].
    pub fn is_seen(&self, face: u8) -> bool {
        (face as usize) < F && self.seen[face as usize] != 0
    }

    /// Draws the edges of `shape` that are seen, with the pen of `g`. The
    /// view must come from a [`View::project`] of the same shape.
    ///
    /// With pen 0 this takes the picture away again, and fast: it clears
    /// the bytes of the bitmap that the edges run through, so pixels of
    /// other things within the same eight are gone too. Take all bodies
    /// away first, then draw them all.
    pub fn draw(&self, g: &Bitmap<Hires>, shape: &Shape) {
        let pattern = Hires::pattern(g.pen);
        #[cfg(target_os = "c64")]
        unsafe {
            c64_g3d_draw((self as *const Self).cast(), (shape as *const Shape).cast(), pattern)
        }
        #[cfg(not(target_os = "c64"))]
        {
            let _ = pattern;
            for &[a, b, f0, f1] in shape.edges {
                if f0 == NO_FACE || self.is_seen(f0) || (f1 != NO_FACE && self.is_seen(f1)) {
                    let (p, q) = (self.points[a as usize], self.points[b as usize]);
                    g.line(p.x, p.y, q.x, q.y);
                }
            }
        }
    }
}

impl<const V: usize, const F: usize> Default for View<V, F> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sines() {
        assert_eq!((sin(0), sin(64), sin(128), sin(192)), (0, 127, 0, -127));
        assert_eq!((cos(0), cos(64), cos(128)), (127, 0, -127));
        assert_eq!((sin(32), sin(96), sin(160), sin(224)), (90, 90, -90, -90));
    }

    #[test]
    fn quarter_turns() {
        // a quarter turn about z takes +x to +y
        let m = Matrix::rotation(0, 0, 64);
        let v = m.apply([100, 0, 0]);
        assert!(v[0].abs() <= 1 && (v[1] - 98).abs() <= 2 && v[2] == 0, "{v:?}");
        // about x: +y to +z; about y: +z to +x
        let v = Matrix::rotation(64, 0, 0).apply([0, 100, 0]);
        assert!(v[1].abs() <= 1 && (v[2] - 98).abs() <= 2, "{v:?}");
        let v = Matrix::rotation(0, 64, 0).apply([0, 0, 100]);
        assert!(v[2].abs() <= 1 && (v[0] - 98).abs() <= 2, "{v:?}");
    }

    /// The numbers that `tests/e2e/g3d` checks the assembly against.
    #[test]
    fn reference_values() {
        let m = Matrix::rotation(20, 40, 230);
        assert_eq!(m.0, REFERENCE_MATRIX);
        let mut view = View::<8, 6>::new();
        view.project(&CUBE, &m, 160, 100);
        let p: [(i16, i16); 8] = core::array::from_fn(|i| (view.points[i].x, view.points[i].y));
        assert_eq!(p, REFERENCE_POINTS);
        assert_eq!(view.seen, REFERENCE_SEEN);
    }

    const REFERENCE_MATRIX: [i8; 9] = [56, 106, 38, -42, 59, -102, -105, 33, 62];
    const REFERENCE_POINTS: [(i16, i16); 8] =
        [(74, 63), (122, 100), (165, 13), (213, 49), (107, 151), (155, 187), (198, 100), (246, 137)];
    const REFERENCE_SEEN: [u8; 6] = [1, 0, 0, 1, 1, 0];

    /// The numbers that `tests/e2e/g3d` checks the perspective against.
    #[test]
    fn perspective_values() {
        let m = Matrix::rotation(20, 40, 230);
        let mut view = View::<8, 6>::new();
        view.project_perspective(&CUBE, &m, 160, 100, 127);
        let p: [(i16, i16); 8] = core::array::from_fn(|i| (view.points[i].x, view.points[i].y));
        assert_eq!(p, PERSPECTIVE_POINTS);
        assert_eq!(view.seen, REFERENCE_SEEN);
        // with depth 0 it is the parallel projection
        let mut flat = View::<8, 6>::new();
        flat.project(&CUBE, &m, 160, 100);
        view.project_perspective(&CUBE, &m, 160, 100, 0);
        assert_eq!(view.points, flat.points);
        // seen from the front, the front face is larger than the back one
        view.project_perspective(&CUBE, &Matrix::IDENTITY, 160, 100, 127);
        let (front, back) = (view.points[7].x - view.points[4].x, view.points[3].x - view.points[0].x);
        assert!(front > 110 && back < 110 && front > back + 30, "{front} {back}");
    }

    const PERSPECTIVE_POINTS: [(i16, i16); 8] =
        [(73, 62), (131, 100), (166, 1), (203, 59), (92, 166), (156, 177), (217, 100), (245, 136)];

    #[test]
    fn hidden_faces() {
        // seen from the front: only the front face
        let mut view = View::<8, 6>::new();
        view.project(&CUBE, &Matrix::IDENTITY, 160, 100);
        assert_eq!(view.seen, [1, 0, 0, 0, 0, 0]);
        // every face of both bodies is seen from its side and not from
        // the other one: three faces of a cube at most, and opposite
        // faces never together
        for (ax, ay) in [(20u8, 40u8), (100, 30), (200, 150), (77, 201), (5, 250)] {
            view.project(&CUBE, &Matrix::rotation(ax, ay, 0), 160, 100);
            let n: u8 = view.seen.iter().sum();
            assert!((1..=3).contains(&n), "{ax} {ay}: {:?}", view.seen);
            for pair in view.seen.chunks(2) {
                assert!(pair[0] + pair[1] <= 1);
            }
        }
        let mut p = View::<5, 5>::new();
        p.project(&PYRAMID, &Matrix::IDENTITY, 160, 100);
        assert_eq!(p.seen, [0, 1, 0, 0, 0]);
        // from below: the base
        p.project(&PYRAMID, &Matrix::rotation(192, 0, 0), 160, 100);
        assert_eq!(p.seen[0], 1);
    }

    #[test]
    fn draws_the_front() {
        let g = crate::gfx::hires(crate::hw::Color::White, crate::hw::Color::Black);
        let mut view = View::<8, 6>::new();
        view.project(&CUBE, &Matrix::IDENTITY, 160, 100);
        view.draw(&g, &CUBE);
        // the front square, and nothing of the hidden edges inside it
        let d = (55i16 * 127 + 64) >> 7;
        assert!(g.point((160 - d) as u16, (100 - d) as u8) && g.point((160 + d) as u16, (100 + d) as u8));
        assert!(g.point(160, (100 - d) as u8) && !g.point(160, 100));
    }
}
