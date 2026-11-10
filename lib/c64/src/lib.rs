//! The Commodore 64 library for rust64.
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//! use c64::hw::{self, Color};
//!
//! #[c64::main]
//! fn main() {
//!     c64::println!("HELLO FROM RUST64 ", 64u8);
//!     hw::vic().border.write(Color::Green);
//! }
//! ```
//!
//! On the C64 (`target_os = "c64"`) the crate is `no_std` and talks to the
//! hardware. On the host it is a normal `std` crate with stand-ins: memory
//! is a simulated 64 KB array and printing goes to stdout, so `cargo test`,
//! rust-analyzer and clippy work as usual.
//!
//! The crate also provides the `#[panic_handler]` on the C64, so programs
//! must not define their own.

#![cfg_attr(target_os = "c64", no_std)]

pub mod dos;
pub mod fmt;
pub mod gfx;
pub mod hw;
pub mod input;
pub mod irq;
pub mod kernal;
pub mod mem;
pub mod sound;
pub mod sprite;
pub mod sys;
pub mod text;

pub use c64_macros::{at, main, sid, zp};

/// A PETSCII string literal for the upper case/graphics character set (both
/// cases become capitals): `c64::p!("HELLO")` is a `&'static [u8; 5]`,
/// converted at compile time. Use it with [`text::print_petscii`],
/// [`kernal::setnam`] and file names.
#[macro_export]
macro_rules! p {
    ($s:literal) => {{
        const P: [u8; $s.len()] = $crate::text::petscii_str::<{ $s.len() }>($s, false);
        &P
    }};
}

/// A PETSCII string literal for the lower/upper case character set (see
/// [`p!`]).
#[macro_export]
macro_rules! pm {
    ($s:literal) => {{
        const P: [u8; $s.len()] = $crate::text::petscii_str::<{ $s.len() }>($s, true);
        &P
    }};
}

/// Includes a rat assembly file in the program:
/// `c64::rat_source!(MY_CODE, "my_code.rat");` (the path is relative to
/// the current file). The routines in it are called through `extern "C"`
/// declarations with the rust64 calling convention: the argument bytes are
/// in `<name>__args` and the first three also in A, X and Y; a result of
/// up to three bytes is returned in A, X, Y, a larger one in `<name>__res`.
#[macro_export]
macro_rules! rat_source {
    ($name:ident, $path:literal) => {
        #[doc(hidden)]
        #[allow(dead_code)]
        #[unsafe(link_section = "rat")]
        static $name: [u8; include_bytes!($path).len()] = *include_bytes!($path);
    };
}

/// Prints the arguments at the cursor (each implements [`fmt::Display`]):
/// `c64::print!("X=", x, " Y=", y);`. ASCII is converted to PETSCII.
#[macro_export]
macro_rules! print {
    ($($arg:expr),* $(,)?) => {{
        let mut _w = $crate::text::Console;
        $( $crate::fmt::Display::fmt(&$arg, &mut _w); )*
    }};
}

/// [`print!`] followed by a line break.
#[macro_export]
macro_rules! println {
    ($($arg:expr),* $(,)?) => {{
        $crate::print!($($arg),*);
        $crate::kernal::chrout($crate::kernal::RETURN);
    }};
}

/// Writes the arguments to a [`fmt::Write`]: `c64::write!(buf, "N=", n);`.
#[macro_export]
macro_rules! write {
    ($w:expr, $($arg:expr),* $(,)?) => {{
        let _w = &mut $w;
        $( $crate::fmt::Display::fmt(&$arg, _w); )*
    }};
}

// The rust64 target aborts on panic (red border, back to BASIC) before a
// handler runs; rustc still needs one.
#[cfg(target_os = "c64")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    sys::panic()
}
