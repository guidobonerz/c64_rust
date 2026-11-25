//! End-to-end test of the core language. Each check counts up; the number
//! of the first failing check is written to the top left of the screen.
//! Border: green = all passed, red = a check failed (or a panic).
#![no_std]
#![no_main]

use core::hint::black_box as bb;
use core::ptr::write_volatile;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

static mut TEST: u8 = 0;
static mut FAILS: u8 = 0;

fn check(ok: bool) {
    unsafe {
        TEST += 1;
        write_volatile(0x0402 as *mut u8, TEST);
        if !ok {
            if FAILS == 0 {
                write_volatile(0x0400 as *mut u8, TEST);
            }
            FAILS += 1;
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Point {
    x: i16,
    y: i16,
}

fn add(p: Point, q: Point) -> Point {
    Point { x: p.x + q.x, y: p.y + q.y }
}

enum Shape {
    Square(u8),
    Rect(u8, u8),
    Nothing,
}

fn area(s: &Shape) -> u16 {
    match *s {
        Shape::Square(a) => a as u16 * a as u16,
        Shape::Rect(w, h) => w as u16 * h as u16,
        Shape::Nothing => 0,
    }
}

fn largest<T: PartialOrd + Copy>(xs: &[T]) -> T {
    let mut m = xs[0];
    for &x in xs {
        if x > m {
            m = x;
        }
    }
    m
}

fn fib(n: u8) -> u16 {
    let (mut a, mut b) = (0u16, 1u16);
    for _ in 0..n {
        let t = a.wrapping_add(b);
        a = b;
        b = t;
    }
    a
}

static TABLE: [u8; 4] = [10, 20, 30, 40];
static GREETING: &str = "HELLO";

fn first_even(xs: &[u8]) -> Option<u8> {
    for &x in xs {
        if x % 2 == 0 {
            return Some(x);
        }
    }
    None
}

#[inline(never)]
fn sum_to(n: u16) -> u32 {
    let mut s = 7u32;
    for i in 0..n {
        s += i as u32;
    }
    s
}

/// The decimal digits of `v`, returns how many.
#[inline(never)]
fn spell(out: &mut [u8; 6], mut v: u32) -> u8 {
    let mut tmp = [0u8; 6];
    let mut n = 0;
    loop {
        tmp[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
        if v == 0 {
            break;
        }
    }
    for i in 0..n {
        out[i] = tmp[n - 1 - i];
    }
    n as u8
}

/// A value that is returned in memory, with a variant without a tag of
/// its own (`Option<Rec>`: the tag is `ok`).
#[derive(Clone, Copy)]
struct Rec {
    id: u16,
    name: [u8; 16],
    len: u8,
    ok: bool,
}

struct Holder {
    lfn: u8,
    rec: Rec,
    done: bool,
}

#[inline(never)]
fn make(n: u8) -> Option<Rec> {
    if n == 0 {
        return None;
    }
    let mut r = Rec { id: n as u16 * 257, name: [b'.'; 16], len: 0, ok: true };
    for c in b"abcdefghijklmnopqrstuvwxyz" {
        // (the index is checked by the condition)
        if (r.len as usize) < r.name.len() {
            let c = if *c >= b'n' { *c - 0x20 } else { *c };
            r.name[r.len as usize] = c;
            r.len += 1;
        }
    }
    Some(r)
}

#[inline(never)]
fn hold(lfn: u8, n: u8) -> Result<Holder, u8> {
    let rec = match make(n) {
        Some(r) => r,
        None => return Err(lfn + 1),
    };
    let mut h = Holder { lfn, rec, done: false };
    if !(h.rec.len < 16) {
        h.done = true;
    }
    Ok(h)
}

/// A value without bytes: a reference to it is not passed where the
/// function only hands it on, and is where the function looks at it.
struct Sink;

impl Sink {
    #[inline(never)]
    fn put(&mut self, v: u8) -> u8 {
        v + 1
    }

    #[inline(never)]
    fn at(&self) -> usize {
        self as *const Sink as usize
    }
}

#[inline(never)]
fn through(s: &mut Sink, a: u16, b: u8) -> u16 {
    let first = s.put(b) as u16;
    a + first + s.put(b + 1) as u16
}

/// A loop with two ways back to its head (the step of the pointer goes
/// to the end of both).
#[inline(never)]
fn upper(out: &mut [u8; 8], text: &[u8]) -> usize {
    let mut n = 0;
    for &c in text {
        if n < out.len() {
            let c = if c.is_ascii_lowercase() { c - 0x20 } else { c };
            out[n] = c;
            n += 1;
        }
    }
    n
}

/// A `usize` counter that stays below 256 (its upper byte is the
/// constant 0) ...
#[inline(never)]
fn fill(out: &mut [u8; 40], text: &[u8]) -> usize {
    let mut n: usize = 0;
    for &c in text {
        if n < 40 {
            out[n] = c;
            n += 1;
        }
    }
    n
}

/// ... and counters that do not: the limit is too large, or the step
/// leads past 255.
#[inline(never)]
fn count_to_300(step: u8) -> usize {
    let mut n: usize = 0;
    let mut sum: usize = 0;
    while n < 300 {
        sum += step as usize;
        n += 1;
    }
    sum
}

#[inline(never)]
fn by_hundreds() -> usize {
    let mut n: usize = bb(0u8) as usize;
    while n < 250 {
        n += 100;
    }
    n
}

/// An iterator over an array in the frame: its end is an address the
/// assembler adds up.
#[inline(never)]
fn add_up(a: u16, b: u16) -> u16 {
    let parts = [a, b, 300, 4000, 50000];
    let mut sum = 0u16;
    for p in &parts {
        sum = sum.wrapping_add(bb(*p));
    }
    sum
}

/// Values of several bytes that are moved from one variable to the next:
/// the variables share their bytes where that changes nothing ...
#[derive(Clone, Copy, PartialEq)]
struct Quad {
    a: u16,
    b: u16,
    c: [u8; 4],
}

#[inline(never)]
fn quad(n: u16) -> Quad {
    Quad { a: n, b: n + 1, c: [n as u8; 4] }
}

#[inline(never)]
fn touch(q: &mut Quad) -> u16 {
    q.a += 10;
    q.a + q.b
}

/// (a result that is moved on and then changed through a reference)
#[inline(never)]
fn moved_on(n: u16) -> u16 {
    let first = quad(n);
    let mut second = first;
    let sum = touch(&mut second);
    let third = second;
    sum + third.a + third.c[3] as u16
}

/// ... and keep their own where it would: the copy is changed and the
/// original is read again,
#[inline(never)]
fn both_used(n: u16) -> u16 {
    let first = quad(n);
    let mut second = first;
    second.a += 100;
    let sum = touch(&mut second);
    first.a + second.a + sum
}

/// two values change places,
#[inline(never)]
fn swapped(n: u16) -> u16 {
    let mut x = quad(n);
    let mut y = quad(n + 100);
    for _ in 0..bb(3u8) {
        let t = x;
        x = y;
        y = t;
    }
    x.a * 2 + y.a
}

/// the old value is still needed when the new one is made,
#[inline(never)]
fn chain(n: u16) -> u16 {
    let mut prev = quad(n);
    let mut sum = 0;
    for i in 0..bb(4u16) {
        let cur = quad(prev.a + i);
        if cur.a & 1 == 0 {
            sum += prev.b;
            prev = cur;
        } else {
            sum += cur.b + prev.a;
        }
    }
    sum + prev.a
}

/// and a value is built in a variable and returned on one of two ways.
#[inline(never)]
fn built(n: u16) -> Result<Quad, u8> {
    let mut q = Quad { a: 0, b: 0, c: [0; 4] };
    for i in 0..4 {
        if bb(n) == 7 {
            return Err(i as u8);
        }
        q.c[i] = n as u8 + i as u8;
        q.a += n;
    }
    q.b = q.a + 1;
    Ok(q)
}

/// A function that is called from one place and returns a value in
/// memory, to a variable a reference is made to afterwards.
#[inline(never)]
fn first_quad(n: u16) -> Result<Quad, u8> {
    let mut q = built(n)?;
    let t = touch(&mut q);
    q.a += t;
    Ok(q)
}

#[inline(never)]
fn set_far(a: &mut [u16; 400]) {
    a[399] = 0x1234;
    a[130] = 7;
}

#[unsafe(no_mangle)]
pub extern "C" fn main() {
    // 1-4: 8/16-bit add and subtract
    check(bb(200u8).wrapping_add(bb(100)) == 44);
    check(bb(1000u16) + bb(2000) == 3000);
    check(bb(300u16).wrapping_sub(bb(301u16)) == 0xffff);
    check(bb(0xffffu32) + bb(1) == 0x10000);
    // 5-8: comparisons
    check(bb(5u16) < bb(300u16));
    check(bb(-5i16) < bb(3i16));
    check(bb(-1i8) < bb(0i8));
    check(!(bb(40000u16) < bb(3u16)));
    // 9-11: multiply
    check(bb(123u16) * bb(45) == 5535);
    check(bb(12u8) * bb(11) == 132);
    check(bb(70000u32) * bb(3) == 210000);
    // 12-15: divide
    check(bb(1000u16) / bb(7) == 142);
    check(bb(1000u16) % bb(7) == 6);
    check(bb(-100i16) / bb(7) == -14);
    check(bb(-100i16) % bb(7) == -2);
    // 16-18: shifts
    check(bb(1u16) << bb(9) == 512);
    check(bb(0x8000u16) >> bb(15) == 1);
    check(bb(-16i16) >> bb(2) == -4);
    // 19-20: loops
    let mut sum = 0u16;
    for i in 1..=bb(100u16) {
        sum += i;
    }
    check(sum == 5050);
    check(fib(bb(20)) == 6765);
    // 21-23: arrays and slices
    let a = [1u16, 2, 3, 4, 5];
    check(a[bb(3)] == 4);
    check(a.iter().sum::<u16>() == 15);
    check(largest(&a[..]) == 5);
    // 24-25: structs
    let p = add(Point { x: bb(3), y: bb(-4) }, Point { x: 10, y: 20 });
    check(p == Point { x: 13, y: 16 });
    check(largest(&[bb(-3i16), 7, -9]) == 7);
    // 26-28: enums and match
    check(area(&Shape::Rect(bb(6), 7)) == 42);
    check(area(&Shape::Square(bb(9))) == 81);
    check(area(bb(&Shape::Nothing)) == 0);
    // 29-31: Option (niche and tagged layout)
    check(first_even(&[1, 3, bb(8), 5]) == Some(8));
    check(first_even(&[bb(1), 3]).is_none());
    let r: Option<&u16> = a.get(bb(7));
    check(r.is_none() && a.get(bb(1)) == Some(&2));
    // 32-34: statics and strings
    check(TABLE[bb(2)] == 30);
    check(GREETING.len() == 5 && GREETING.as_bytes()[bb(1)] == b'E');
    check(b"rust64"[bb(4)] == b'6');
    // 35: closures
    let k = bb(10u8);
    let f = |x: u8| x + k;
    check(f(bb(5)) == 15);
    // 36: iterator adapters
    check((0..bb(10u8)).filter(|x| x % 3 == 0).map(|x| x as u16 * 2).sum::<u16>() == 36);
    // 37-41: multiply by a constant (shifts and additions, or the runtime)
    check(bb(7u8) * 10 == 70 && bb(3u8).wrapping_mul(255) == 253 && bb(9u8) * 0 == 0);
    check(bb(25u16) * 40 == 1000 && bb(199u16) * 320 == 63680);
    check(bb(123u16).wrapping_mul(0xfffb) == 0xfd99 && 100 * bb(655u16) == 65500);
    check(bb(-7i16) * 100 == -700 && bb(-3i8) * 5 == -15);
    let mut m = bb(321u16);
    m *= 10;
    check(m == 3210);
    // 42-45: multiply, variable operands
    check(bb(255u8).wrapping_mul(bb(255)) == 1 && bb(0u8) * bb(9) == 0 && bb(16u8) * bb(15) == 240);
    check(bb(0xffffu16).wrapping_mul(bb(0xffff)) == 1 && bb(256u16) * bb(255) == 65280);
    check(bb(-300i16) * bb(100) == -30000 && bb(0u16) * bb(1234) == 0);
    check(
        bb(0x0001_0000u32) * bb(0xffff) == 0xffff_0000
            && bb(0xffff_ffffu32).wrapping_mul(bb(0xffff_ffff)) == 1
            && bb(123_456_789u64) * bb(1_000_000_007) == 123_456_789_864_197_523
            && bb(5u32) * bb(0) == 0,
    );
    // 46-49: add and subtract a small value in place
    let mut w = bb(0x12f0u16);
    w += 0x20;
    check(w == 0x1310);
    w -= 0x11;
    check(w == 0x12ff);
    let mut d = bb(0x00ff_fff0u32);
    d += 0x20;
    d += 1;
    check(d == 0x0100_0011);
    let mut q = bb(0xffff_ffff_ffff_ffffu64);
    q = q.wrapping_add(1);
    let mut z = bb(0x1200u16);
    z -= 1;
    check(q == 0 && z == 0x11ff && 5 + bb(1000u16) == 1005);
    // 50: indexing with a scaled index (elements of 3 and 6 bytes)
    let rgb = [[1u8, 2, 3], [4, 5, 6], [7, 8, 9]];
    let wide = [[1u16, 2, 3], [4, 5, 6], [7, 8, 9]];
    check(rgb[bb(2usize)][1] == 8 && wide[bb(1usize)][2] == 6 && wide[bb(2u8) as usize][0] == 7);
    // 51-56: loops over arrays of known length (the test of the iterator's
    // pointer against its end): bytes with a counter, without one, words,
    // 255 bytes, more than 255 bytes, and leaving early in nested loops
    let k = bb(3u8);
    let mut bytes = [0u8; 40];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = (i as u8) * k;
    }
    check(bytes[0] == 0 && bytes[1] == 3 && bytes[39] == 117);
    let mut sum = 0u16;
    for b in &bytes {
        sum += *b as u16;
    }
    check(sum == 2340);
    let mut words = [0u16; 100];
    for (i, w) in words.iter_mut().enumerate() {
        *w = 1000 + i as u16;
    }
    check(words[0] == 1000 && words[99] == 1099 && words[bb(50usize)] == 1050);
    let mut long = [1u8; 255];
    let mut n = 0u16;
    for b in long.iter_mut() {
        *b = k;
        n += 1;
    }
    check(n == 255 && long[0] == 3 && long[254] == 3);
    let mut big = [0u8; 300];
    let mut n = 0u16;
    for b in big.iter_mut() {
        *b = k;
        n += 1;
    }
    check(n == 300 && big[0] == 3 && big[bb(299usize)] == 3);
    let mut grid = [[0u8; 12]; 5];
    for (r, row) in grid.iter_mut().enumerate() {
        for (i, cell) in row.iter_mut().enumerate() {
            if i == 10 {
                continue;
            }
            if r == 4 && i == 6 {
                break;
            }
            *cell = (r as u8) * 16 + i as u8;
        }
    }
    check(
        grid[0][0] == 0
            && grid[3][11] == 0x3b
            && grid[2][10] == 0
            && grid[4][5] == 0x45
            && grid[4][6] == 0
            && grid[bb(4usize)][11] == 0,
    );
    // 57-59: an index in place of the iterator's pointer: with a second
    // pointer that walks along and is needed after the loop, leaving the
    // loop early, and a plain sum
    let src = [k, 5, 7, 9, 11, 13, 15, 17];
    let mut dst = [0u8; 20];
    let mut to = dst.as_mut_ptr();
    for _ in 0..2 {
        for &b in &src {
            unsafe {
                to.write_volatile(b);
                to = to.add(1);
            }
        }
    }
    check(dst[0] == 3 && dst[7] == 17 && dst[8] == 3 && dst[15] == 17 && dst[bb(16usize)] == 0);
    let (mut before, mut at) = (0u8, 0u8);
    for (i, &b) in src.iter().enumerate() {
        if b == 11 {
            at = i as u8;
            break;
        }
        before += b;
    }
    check(at == 4 && before == 24);
    let mut total = 0u16;
    for &b in &dst {
        total += b as u16;
    }
    check(total == 160);
    // 60-61: 32-bit multiplication: 16 x 16 bits, a byte, signs, wide
    check(
        bb(1234i32) * bb(-567) == -699678
            && bb(-300i32) * bb(-200) == 60000
            && bb(200u32) * bb(40000) == 8_000_000
            && bb(40000u32) * bb(3) == 120000
            && bb(0u32) * bb(40000) == 0,
    );
    check(
        bb(0x12345u32).wrapping_mul(bb(0x6789)) == 0x12345u32.wrapping_mul(0x6789)
            && bb(-70000i32) * bb(3) == -210000
            && bb(i32::MIN).wrapping_mul(bb(3)) == i32::MIN.wrapping_mul(3)
            && bb(0x00ff_ffffu32).wrapping_mul(bb(0x00ff_ffff)) == 0x00ff_ffffu32.wrapping_mul(0x00ff_ffff),
    );
    // 62-63: 32-bit division: small and large divisors, leading zeros
    check(
        bb(7_000_000u32) / bb(699) == 10014
            && bb(7_000_000u32) % bb(699) == 214
            && bb(0xffff_ffffu32) / bb(0x1_0001) == 0xffff
            && bb(0xffff_ffffu32) % bb(0x1_0001) == 0
            && bb(0u32) / bb(7) == 0
            && bb(255u32) % bb(256) == 255
            && bb(0x00ff_ff00u32) / bb(0xffff) == 0x100,
    );
    check(
        bb(5u32) / bb(0x9000_0000) == 0
            && bb(0xf000_0000u32) / bb(0x9000_0000) == 1
            && bb(0xf000_0000u32) % bb(0x9000_0000) == 0x6000_0000
            && bb(0xffff_ffffu32) / bb(0x7fff_ffff) == 2
            && bb(0xffff_ffffu32) % bb(0x7fff_ffff) == 1,
    );
    // 64: the quotient and the remainder of one division
    let (mut num, den) = (bb(123456u32), bb(1000u32));
    let rest = num % den;
    num /= den;
    let (sn, sd) = (bb(-17i16), bb(5i16));
    let (bn, bd) = (bb(200u8), bb(7u8));
    check(rest == 456 && num == 123 && sn / sd == -3 && sn % sd == -2 && bn % bd == 4 && bn / bd == 28);
    // 65: shifts by a constant of a byte and more
    check(
        bb(0x1234_5678u32) >> 11 == 0x1234_5678 >> 11
            && bb(-123456i32) >> 11 == -123456 >> 11
            && bb(-2i32) >> 24 == -1
            && bb(-5i32) >> 31 == -1
            && bb(0x1234_5678u32) << 13 == 0x1234_5678u32 << 13
            && bb(1u32) << 31 == 0x8000_0000
            && bb(0x1234_5678_9abc_def0u64) >> 20 == 0x1234_5678_9abc_def0u64 >> 20
            && bb(0x1234_5678_9abc_def0u64) << 40 == 0x1234_5678_9abc_def0u64 << 40
            && bb(0x8000u16) >> 9 == 0x40
            && bb(-32768i16) >> 9 == -64
            && bb(0x0123u16) << 8 == 0x2300,
    );
    // 66: a constant index far into a large array behind a reference
    let mut large = [0u16; 400];
    set_far(&mut large);
    check(large[399] == 0x1234 && large[130] == 7 && large[bb(129usize)] == 0 && large[bb(398usize)] == 0);
    // 67: the result of a function with one caller (its code is put into
    // the caller) is still there after calls of other functions
    let total = sum_to(bb(200));
    let mut digits = [0u8; 6];
    let a = spell(&mut digits, bb(4711));
    let first = digits[0];
    let b = spell(&mut digits, total);
    check(a == 4 && first == b'4' && b == 5 && digits == *b"19907 ");

    // 68-71: structs as results. One that is read where the call left it,
    // one that is still needed after the next call, a `Result` around a
    // struct that is built from the result of another call, and the
    // variant without the data.
    check(matches!(make(bb(3)), Some(r) if r.id == 771 && r.len == 16 && &r.name == b"abcdefghijklmNOP"));
    let first = make(bb(1));
    let second = make(bb(2));
    check(matches!((first, second), (Some(a), Some(b)) if a.id == 257 && b.id == 514 && a.ok && b.ok));
    check(make(bb(0)).is_none() && !make(bb(9)).is_none());
    let held = hold(bb(4), bb(5));
    let none = hold(bb(4), bb(0));
    check(matches!(&held, Ok(h) if h.lfn == 4 && h.rec.id == 1285 && h.done && h.rec.name[15] == b'P'));
    check(matches!(none, Err(5)) && held.is_ok());

    // 73-74: a reference to a value without bytes, and a loop with a
    // `continue` in it
    let mut sink = Sink;
    let here = &sink as *const Sink as usize;
    check(through(&mut sink, bb(1000), bb(5)) == 1013 && sink.at() == here && sink.put(bb(7)) == 8);
    let mut out = [b'.'; 8];
    check(upper(&mut out, bb(b"ab1")) == 3 && &out == b"AB1....." && upper(&mut out, bb(b"hello, world")) == 8 && &out == b"HELLO, W");

    // 75-76: counters of two bytes whose upper byte is known to stay 0,
    // and ones where it does not; the end of an array in the frame
    let mut line = [0u8; 40];
    check(fill(&mut line, bb(b"abc")) == 3 && line[2] == b'c' && line[3] == 0
        && fill(&mut line, bb(&[b'x'; 50])) == 40 && line[39] == b'x');
    check(count_to_300(bb(2)) == 600 && by_hundreds() == 300 && add_up(bb(1), bb(20)) == 54321);

    // 77-79: variables that share their bytes, and ones that must not
    check(moved_on(bb(5)) == 5 + 10 + 6 + 15 + 5 && both_used(bb(5)) == 5 + 115 + 121);
    check(swapped(bb(1)) == 203 && chain(bb(2)) == 28);
    check(built(bb(7)) == Err(0) && built(bb(3)).is_ok_and(|q| q.a == 12 && q.b == 13 && q.c == [3, 4, 5, 6]));
    // 80: a result that the pasted function writes where it goes
    let mut got = first_quad(bb(2));
    let ok = match &mut got {
        Ok(q) => touch(q) == 55 + 9 && q.c == [2, 3, 4, 5],
        Err(_) => false,
    };
    check(ok && first_quad(bb(7)) == Err(0));

    unsafe {
        write_volatile(0xd020 as *mut u8, if FAILS == 0 { 5 } else { 2 });
        write_volatile(0x0401 as *mut u8, TEST);
    }
}
