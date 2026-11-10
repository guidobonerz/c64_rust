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

    unsafe {
        write_volatile(0xd020 as *mut u8, if FAILS == 0 { 5 } else { 2 });
        write_volatile(0x0401 as *mut u8, TEST);
    }
}
