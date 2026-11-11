//! Formatting without `core::fmt`: no vtables, no recursion, and numbers
//! are converted without division (repeated subtraction of powers of ten).
//!
//! Output goes to a [`Write`]: the screen ([`crate::text::Console`]), a
//! [`Buf`] or your own type. Use it through [`crate::print!`],
//! [`crate::println!`] and [`crate::write!`].

/// A byte sink. Bytes are ASCII; the sink decides how to show them (the
/// console converts them to PETSCII).
pub trait Write {
    fn write_byte(&mut self, b: u8);

    fn write_bytes(&mut self, s: &[u8]) {
        for &b in s {
            self.write_byte(b);
        }
    }

    fn write_str(&mut self, s: &str) {
        self.write_bytes(s.as_bytes());
    }
}

/// A value that can be written to a [`Write`].
pub trait Display {
    fn fmt<W: Write>(&self, w: &mut W);
}

impl<T: Display + ?Sized> Display for &T {
    fn fmt<W: Write>(&self, w: &mut W) {
        (**self).fmt(w)
    }
}

impl Display for str {
    fn fmt<W: Write>(&self, w: &mut W) {
        w.write_str(self)
    }
}

impl Display for [u8] {
    fn fmt<W: Write>(&self, w: &mut W) {
        w.write_bytes(self)
    }
}

impl Display for bool {
    fn fmt<W: Write>(&self, w: &mut W) {
        w.write_str(if *self { "true" } else { "false" })
    }
}

/// An ASCII character (other characters are written as `?`).
impl Display for char {
    fn fmt<W: Write>(&self, w: &mut W) {
        let c = *self as u32;
        w.write_byte(if c < 0x80 { c as u8 } else { b'?' })
    }
}

#[cfg(not(target_os = "c64"))]
const POW10_16: [u16; 4] = [10000, 1000, 100, 10];
#[cfg(not(target_os = "c64"))]
const POW10_32: [u32; 9] = [
    1_000_000_000,
    100_000_000,
    10_000_000,
    1_000_000,
    100_000,
    10_000,
    1000,
    100,
    10,
];

// On the C64 the digits come from one assembly routine (rat/text.rat) for
// all sizes and all sinks.
#[cfg(target_os = "c64")]
unsafe extern "C" {
    fn c64_fmt_dec(n: u32) -> u8;
}

#[cfg(target_os = "c64")]
fn write_dec<W: Write>(w: &mut W, n: u32) {
    unsafe {
        // The digits are in the runtime registers rt_b..: copy them, the
        // sink may use those registers.
        let len = c64_fmt_dec(n);
        let at: u16;
        crate::asm!("lda #<rt_b", "sta {p}", "lda #>rt_b", "sta {p}+1", p = out(reg) at);
        let digits: [u8; 10] = *(at as *const [u8; 10]);
        w.write_bytes(core::slice::from_raw_parts(digits.as_ptr(), len as usize));
    }
}

/// Writes `n` in decimal.
#[cfg(target_os = "c64")]
pub fn write_u16<W: Write>(w: &mut W, n: u16) {
    write_dec(w, n as u32)
}

/// Writes `n` in decimal.
#[cfg(target_os = "c64")]
pub fn write_u32<W: Write>(w: &mut W, n: u32) {
    write_dec(w, n)
}

/// Writes `n` in decimal.
#[cfg(not(target_os = "c64"))]
pub fn write_u16<W: Write>(w: &mut W, mut n: u16) {
    let mut started = false;
    for &p in &POW10_16 {
        let mut d = 0u8;
        while n >= p {
            n -= p;
            d += 1;
        }
        if started || d != 0 {
            w.write_byte(b'0' + d);
            started = true;
        }
    }
    w.write_byte(b'0' + n as u8);
}

/// Writes `n` in decimal.
#[cfg(not(target_os = "c64"))]
pub fn write_u32<W: Write>(w: &mut W, mut n: u32) {
    if n <= 0xffff {
        return write_u16(w, n as u16);
    }
    let mut started = false;
    for &p in &POW10_32 {
        let mut d = 0u8;
        while n >= p {
            n -= p;
            d += 1;
        }
        if started || d != 0 {
            w.write_byte(b'0' + d);
            started = true;
        }
    }
    w.write_byte(b'0' + n as u8);
}

macro_rules! unsigned {
    ($($t:ty => $f:ident),*) => {$(
        impl Display for $t {
            fn fmt<W: Write>(&self, w: &mut W) {
                $f(w, *self as _)
            }
        }
    )*};
}
unsigned!(u8 => write_u16, u16 => write_u16, usize => write_u32, u32 => write_u32);

macro_rules! signed {
    ($($t:ty => $u:ty),*) => {$(
        impl Display for $t {
            fn fmt<W: Write>(&self, w: &mut W) {
                if *self < 0 {
                    w.write_byte(b'-');
                }
                self.unsigned_abs().fmt(w)
            }
        }
    )*};
}
signed!(i8 => u8, i16 => u16, i32 => u32, isize => usize);

const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// Writes the value in hexadecimal with all digits (`Hex(0xd020u16)` →
/// `D020`, `Hex(5u8)` → `05`).
#[derive(Clone, Copy)]
pub struct Hex<T>(pub T);

impl Display for Hex<u8> {
    fn fmt<W: Write>(&self, w: &mut W) {
        w.write_byte(HEX[(self.0 >> 4) as usize]);
        w.write_byte(HEX[(self.0 & 15) as usize]);
    }
}

impl Display for Hex<u16> {
    fn fmt<W: Write>(&self, w: &mut W) {
        Hex((self.0 >> 8) as u8).fmt(w);
        Hex(self.0 as u8).fmt(w);
    }
}

impl Display for Hex<u32> {
    fn fmt<W: Write>(&self, w: &mut W) {
        Hex((self.0 >> 16) as u16).fmt(w);
        Hex(self.0 as u16).fmt(w);
    }
}

/// Writes the value in binary with all digits (`Bin(5u8)` → `00000101`).
#[derive(Clone, Copy)]
pub struct Bin<T>(pub T);

impl Display for Bin<u8> {
    fn fmt<W: Write>(&self, w: &mut W) {
        let mut v = self.0;
        for _ in 0..8 {
            w.write_byte(if v & 0x80 != 0 { b'1' } else { b'0' });
            v <<= 1;
        }
    }
}

impl Display for Bin<u16> {
    fn fmt<W: Write>(&self, w: &mut W) {
        Bin((self.0 >> 8) as u8).fmt(w);
        Bin(self.0 as u8).fmt(w);
    }
}

/// A fixed-size text buffer; bytes beyond the capacity are dropped.
pub struct Buf<const N: usize> {
    data: [u8; N],
    len: usize,
}

impl<const N: usize> Buf<N> {
    pub const fn new() -> Self {
        Buf { data: [0; N], len: 0 }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.data[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }
}

impl<const N: usize> Default for Buf<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Write for Buf<N> {
    fn write_byte(&mut self, b: u8) {
        if self.len < N {
            self.data[self.len] = b;
            self.len += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s<T: Display>(v: T) -> std::string::String {
        let mut b = Buf::<40>::new();
        v.fmt(&mut b);
        std::string::String::from_utf8(b.as_bytes().to_vec()).unwrap()
    }

    #[test]
    fn numbers() {
        assert_eq!(s(0u8), "0");
        assert_eq!(s(255u8), "255");
        assert_eq!(s(10u16), "10");
        assert_eq!(s(10000u16), "10000");
        assert_eq!(s(65535u16), "65535");
        assert_eq!(s(65536u32), "65536");
        assert_eq!(s(4_294_967_295u32), "4294967295");
        assert_eq!(s(-128i8), "-128");
        assert_eq!(s(-32768i16), "-32768");
        assert_eq!(s(i32::MIN), "-2147483648");
        assert_eq!(s(Hex(0xd020u16)), "D020");
        assert_eq!(s(Hex(5u8)), "05");
        assert_eq!(s(Bin(5u8)), "00000101");
        assert_eq!(s("hi"), "hi");
        assert_eq!(s(true), "true");
    }

    #[test]
    fn write_macro() {
        let mut b = Buf::<40>::new();
        crate::write!(b, "x=", 3u8, ' ', -4i16);
        assert_eq!(b.as_bytes(), b"x=3 -4");
    }
}
