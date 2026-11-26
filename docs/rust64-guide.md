# rust64 User Guide

rust64 compiles Rust programs for the Commodore 64. You write real Rust
(`no_std`, traits, generics, closures, iterators, `Option`, `match`), with
the usual tools (rust-analyzer, clippy, `cargo test` for the logic), and
rust64 turns it into a program that the C64 loads and runs. It is a
codegen backend for rustc: rustc does parsing, type checking, borrow
checking and monomorphization, and rust64 translates the resulting MIR
into 6510 assembly, which [rat](../../rat) assembles into a `.prg` and a
`.d64`. The `c64` crate gives you the machine: text, sprites, bitmap
graphics, sound, raster interrupts, joysticks, disk drives.

```rust
#![no_std]
#![no_main]

use c64::hw::{self, Color};

#[c64::main]
fn main() {
    c64::println!("hello from rust64");
    hw::vic().border.write(Color::Green);
}
```

```
$ cargo c64 build --release
    wrote .../target/mos-c64/release/hello.prg
```

This guide explains how to install rust64, which part of Rust it
supports, how the generated program uses the machine, and how to use the
`c64` library. Chapter 9 is a cookbook with complete examples.

**Contents**

1. [Installation](#1-installation)
2. [Your first program](#2-your-first-program)
3. [The cargo c64 command](#3-the-cargo-c64-command)
4. [Rust on the C64](#4-rust-on-the-c64)
5. [Memory and the runtime](#5-memory-and-the-runtime)
6. [Attributes and macros](#6-attributes-and-macros)
7. [Assembly functions](#7-assembly-functions)
8. [Library reference](#8-library-reference)
9. [Cookbook](#9-cookbook)
10. [Inside the backend](#10-inside-the-backend)
11. [Testing](#11-testing)
12. [Troubleshooting](#12-troubleshooting)
13. [Limitations and roadmap](#13-limitations-and-roadmap)

---

## 1. Installation

| Tool | Purpose | Where rust64 looks |
|---|---|---|
| rustup | installs the pinned nightly toolchain | `rust-toolchain.toml` |
| **rat** (source checkout) | assembler, linked into the backend as a library | `../rat` next to this repository |
| **mimic** (optional) | runs programs (`cargo c64 run`) | `RUST64_MIMIC`, else `mimic` on the `PATH` |
| **mimic-headless** (optional) | end-to-end tests | `RUST64_MIMIC_HEADLESS`, else `../mimic/target/release` |
| C64 ROMs (for mimic) | KERNAL, BASIC, character ROM | `EMU_ROM_DIR`, else `~/.mimic/roms` |

rust64 needs a fixed nightly, because a codegen backend uses rustc's
internal crates. `rust-toolchain.toml` pins it together with the
components `rustc-dev`, `rust-src` and `llvm-tools`; rustup installs them
on the first build. The repositories are expected side by side:

```
git/
  c64_rust/      this repository
  rat/           the assembler
  mimic/         the emulator (optional)
```

Build the backend and the cargo subcommand:

```
cd c64_rust
cargo build --release -p rustc_codegen_rust64 -p cargo-c64
```

This puts `cargo-c64` and the backend (`rustc_codegen_rust64.dll`, `.so`
or `.dylib`) side by side in `target/release`. `cargo-c64` finds the
backend next to itself; set `RUST64_BACKEND` to use another one. Put
`target/release` on your `PATH` so that `cargo c64` works everywhere:

```
set PATH=%PATH%;C:\Users\me\git\c64_rust\target\release       (Windows)
export PATH=$PATH:$HOME/git/c64_rust/target/release            (Linux, macOS)
```

### A project

A rust64 program is a normal cargo package with a binary target that
depends on the `c64` crate:

```
mygame/
  Cargo.toml
  src/
    main.rs
    sprites.rat        (optional: assembly)
  music.sid            (optional: a SID tune)
```

```toml
[package]
name = "mygame"
version = "0.1.0"
edition = "2024"

[dependencies]
c64 = { path = "../c64_rust/lib/c64" }

[profile.dev]
opt-level = 1

[profile.release]
opt-level = "s"
```

`opt-level = 1` in the dev profile is recommended: it lets rustc's MIR
inliner remove the small helper calls that iterators and `core` produce.
The release profile with `"s"` gives the smallest programs.

The `c64` crate also builds on the host (without `cargo c64`): then it
uses stand-ins, a simulated 64 KB memory and stdout for printing, so
rust-analyzer, clippy and `cargo test` work as usual.

---

## 2. Your first program

`src/main.rs`:

```rust
//! Hello: prints a greeting and the first Fibonacci numbers, then turns the
//! border green.
#![no_std]
#![no_main]

use c64::hw::{self, Color};

fn fib(n: u8) -> u16 {
    let (mut a, mut b) = (0u16, 1u16);
    for _ in 0..n {
        (a, b) = (b, a + b);
    }
    a
}

#[c64::main]
fn main() {
    c64::println!("hello from rust64");
    for i in 0..10 {
        c64::print!(fib(i), " ");
    }
    c64::println!();
    hw::vic().border.write(Color::Green);
}
```

* `#![no_std]`: the C64 has no operating system for `std`; `core` is
  available completely.
* `#![no_main]` and `#[c64::main]`: the runtime starts the function marked
  with `#[c64::main]`. When it returns, the program returns to BASIC.
* `c64::println!` takes a comma-separated list of values (strings,
  numbers, characters, `bool`, see [c64::fmt](#c64fmt-formatting)). It does
  not use `core::fmt`, which would cost kilobytes.
* There is no `#[panic_handler]` in the program: the `c64` crate brings
  it.

Build and run:

```
cargo c64 build --release          # target/mos-c64/release/hello.prg
cargo c64 run --release            # build and start mimic
```

The program prints

```
HELLO FROM RUST64
0 1 1 2 3 5 8 13 21 34
```

and the border turns green. (Strings are ASCII in the source and are
converted to PETSCII: in the C64's default character set both upper and
lower case letters show as capitals.)

The repository has more examples in `examples/`: `border` (the smallest
program, without the library), `hello`, `joystick`, `ui` (boxes on the
text screen), `music`, `sprites`, `drawing`, `rasterbars` (colour bars
over the whole width of the screen: a stable raster interrupt in
assembly, the colour table from Rust), `scroller` (a horizontal soft
scroll: `$D016` set by two raster interrupts for one text row),
`diagonal` (the whole screen scrolls diagonally over a pattern in a
custom character set, with two screens that are swapped), `mandelbrot`,
`julia` and `pi` (arithmetic), `cube3d` (a turning cube without its
hidden edges, with double buffering), `perspective` (a turning bar with
a vanishing point), `dir` (the same as c64_go's), and `sd2iec`, `uci`
and `wic64` (the extension crates of chapter 8).

---

## 3. The cargo c64 command

```
cargo c64 build [--release] [-o FILE.prg|FILE.d64] [cargo args...]
cargo c64 run   [--release] [-o FILE.prg|FILE.d64] [cargo args...]
cargo c64 asm   [--release] [cargo args...]
```

| Command | Does |
|---|---|
| `build` | builds the program: `target/mos-c64/<profile>/<name>.prg` |
| `run` | builds and starts mimic (`--prg`, or `--disk` for a `.d64`) |
| `asm` | builds and prints the generated assembly listing |
| `-o FILE` | also copies the result to FILE; with `.d64` the disk image |
| `--release` | the release profile |

Other arguments go to `cargo build` (for example `-p`, `--features`,
`-v`).

Behind the scenes `cargo c64` writes the target specification
(`target/rust64/mos-c64.json`) and runs

```
cargo build -Zbuild-std=core -Zjson-target-spec --target target/rust64/mos-c64.json
  with RUSTFLAGS = -Zcodegen-backend=<backend> -Zalways-encode-mir -Zinline-mir
                   -Zinline-mir-threshold=100 -Zinline-mir-hint-threshold=500
```

with the pinned nightly. `core` is built from source for the C64 target.
Library crates (`core`, `c64`, your own) produce no code, only their MIR;
when rustc compiles the program, the backend translates everything that
`main` reaches. Unused library code costs nothing.

Every build writes three files. The `.prg` is copied to
`target/mos-c64/<profile>/`; the assembly listing (`.rat`) and the disk
image (`.d64`, with the program as its only file) are next to cargo's
build files in `target/mos-c64/<profile>/build/<package>/<hash>/out/`.
`cargo c64 asm` and `-o x.d64` find them for you.

When the backend changes, `cargo c64` cleans the program's crate, because
cargo cannot see that change itself.

| Variable | Meaning |
|---|---|
| `RUST64_BACKEND` | path of the backend library |
| `RUST64_MIMIC` | the emulator for `cargo c64 run` |
| `RUST64_MIMIC_HEADLESS` | the headless emulator for the tests |
| `RUST64_TRACE` | print the MIR of every compiled function |

---

## 4. Rust on the C64

### What works

* All of `core` that does not need floats or the heap: `Option`,
  `Result`, `?`, slices and their methods (`iter`, `get`, `split_at`,
  `chunks`, `contains`, ...), `&str` and byte strings, ranges, arrays,
  `core::mem`, `core::ptr`, `core::hint::black_box`.
* `struct`, `enum` (also with data), tuples, `match` with every pattern,
  `if let`, `let else`, `while let`, labelled `break`/`continue`.
* Generics, traits and trait methods (resolved at compile time),
  associated constants, const generics, `impl Trait`.
* Closures (capturing by reference or by value) and every iterator
  adapter: `map`, `filter`, `enumerate`, `zip`, `rev`, `step_by`, `sum`,
  `fold`, `any`, `position`, ...
* `static` and `static mut`, `const`, `const fn` (evaluated by rustc:
  tables cost no code).
* Function pointers (`fn(u8) -> u16`), also non-capturing closures as
  function pointers.
* `Drop`: values are dropped at the end of their scope (a `dos::File`
  closes itself).
* Integer arithmetic of every width with overflow checks in debug builds,
  `wrapping_*`, `checked_*`, `saturating_*`, shifts, rotations,
  `count_ones`, `leading_zeros`, ...
* Panics: bounds checks, `unwrap` on `None`, overflow in debug builds,
  `unreachable!()`, `assert!`.

### Sizes

| Type | Bytes | Note |
|---|---|---|
| `u8`, `i8`, `bool` | 1 | native: prefer them |
| `u16`, `i16`, `usize`, `isize` | 2 | `usize` is 16 bits |
| pointers, references, `fn` | 2 | |
| `&[T]`, `&str` | 4 | pointer and length |
| `u32`, `i32` | 4 | multiplication/division in the runtime (`examples/pi`, `examples/mandelbrot`) |
| `u64`, `i64` | 8 | |
| `u128`, `i128` | 16 | add, subtract, logic, compare; no `*` `/` |
| `char` | 4 | avoid: use `u8` and ASCII |
| `Option<&T>`, `Option<fn()>` | 2 | null is `None` |

Everything has alignment 1. Struct fields may be reordered by rustc as
usual; use `#[repr(C)]` when the layout matters (hardware registers,
assembly).

### What does not work (yet)

| Feature | Instead |
|---|---|
| recursion | loops; the compiler reports the cycle |
| `dyn Trait` (vtables) | generics, enums, `fn` pointers |
| `f32`, `f64` | fixed point (`i16` with 8.8) |
| `alloc` (`Vec`, `String`, `Box`) | arrays, `c64::fmt::Buf` |
| `core::fmt` with arguments (`write!(f, "{}", x)`, `panic!("{}", x)`) | `c64::print!`, `c64::write!`, `panic!("text")` |
| `ptr::copy` (overlapping copies, memmove) | `c64::mem::copy` (copies upwards) |
| `u128` multiplication/division | |
| `asm!` with registers as operands | `c64::asm!`: operands are memory locations (chapter 7) |
| threads, atomics | interrupts with `c64::irq`; `sys::without_interrupts` |

Recursion is not possible because every function has a static frame (see
[5.3](#static-frames)). A recursive program is rejected:

```
error: rust64: recursion is not supported (static frames): mygame::walk -> mygame::walk
```

### Panics

The target uses `panic = "immediate-abort"`: a panic turns the border red
and returns to BASIC. There is no message. In debug builds overflow and
`ub_checks` (pointer alignment, `get_unchecked` bounds, ...) are checked;
release builds keep bounds checks but drop the others.

### Strings and PETSCII

String literals in the source are ASCII (UTF-8). The C64 uses PETSCII and
its own screen codes:

* `c64::print!`/`println!` and `text::print` convert ASCII to PETSCII:
  `a-z` and `A-Z` both show as capitals in the default upper case/graphics
  character set; after `text::lowercase()` they show as written. `\n`
  becomes RETURN.
* `c64::p!("HELLO")` converts at compile time to a `&'static [u8; N]` of
  PETSCII for the upper case set; `c64::pm!` does it for the lower/upper
  case set. Use them for file names, KERNAL calls and
  `text::print_petscii`.
* `text::put_str` writes screen codes directly into screen memory (with a
  colour, no cursor), `text::screen_code` converts one character.
* `c64::s!("SCORE")` converts at compile time to a `&'static [u8; N]` of
  screen codes for the upper case set, `c64::sm!` for the lower/upper case
  set. `text::put_codes` writes them: the program then carries no
  conversion and spends no time on it.
* `b'A'` and `b"..."` stay ASCII bytes.

---

## 5. Memory and the runtime

### Memory map

| Address | Contents |
|---|---|
| `$0002-$008F` | zero page: `#[c64::zp]` statics, the registers of the runtime routines and the argument areas of the library's assembly routines that the program uses, then the frames of the most-called functions (saved at start, restored at exit) |
| `$00F9-$00FE` | the runtime's pointer registers (`zp_p0`-`zp_p2`) |
| `$0400-$07FF` | text screen (default) |
| `$0801` | BASIC line `SYS 2061`, then the runtime and the program |
| after the code | constant data and statics with initial values |
| after the data | BSS (not in the file): runtime buffers, statics that start as zero, static frames |
| up to `$9FFF` | the end: the program, its data and BSS must stay below |
| `$A000-$BFFF` | BASIC ROM (RAM after `hw::bank_out_basic()`) |
| `$C000-$CFFF` | free RAM |
| `$D000-$DFFF` | I/O: VIC-II, SID, colour RAM, CIAs |
| `$E000-$FFFF` | KERNAL ROM |

`#[c64::at]` statics with data are part of the file at their address
(chapter 6), and the linker checks that the program ends below every
placed static in RAM. Using `c64::gfx` places its colour matrix at
`$4000`: then the program and its BSS must end below `$4000`.

What the library takes above the program, when the program uses it:

| Address | Used by | For |
|---|---|---|
| `$4000-$43E7` | `c64::gfx` | the colour matrix of the bitmap (VIC bank 1) |
| `$6000-$7F3F` | `c64::gfx` | the bitmap |
| `$8000-$83E7` | `Bitmap::double_buffer` | the colour matrix of the second bitmap (VIC bank 2) |
| `$A000-$BF3F` | `Bitmap::double_buffer` | the second bitmap, under the BASIC ROM, which is switched off while there are two bitmaps |
| `$9600-$9FFF` | `c64::g3d` | the table of squares, the work area of the corners, the table of the perspective |
| `$F7`/`$F8` | `c64::g3d` | a fourth pointer in the zero page |

Nothing checks the last three against what a program places there
itself: keep them free.

### Start and exit

The program starts like a BASIC program (`RUN`). The start code

1. saves the part of the zero page `$02-$8F` that the program uses and
   clears it,
2. saves the stack pointer,
3. clears the statics that start as zero (they are in the BSS, not in the
   file, so they are zero at every `RUN`) and initializes the zero-page
   statics,
4. calls `main`.

When `main` returns, or on `sys::exit()` or a panic, the exit code puts
the machine back into the state BASIC starts with, so BASIC continues
normally:

* the memory configuration (`$01` = `$37`),
* the CIAs and the SID's volume, with the KERNAL's `IOINIT` (`$FF84`):
  the timer interrupt is on, the VIC bank is 0, the SID is silent,
* the VIC's mode and interrupts: `$D011`-`$D01A` get the KERNAL's start
  values (text mode, 25 rows, 40 columns, the upper-case character set at
  `$1000`, the screen at `$0400`, sprites off, raster interrupt off),
* the IRQ vector `$0314`, if the program names it: the KERNAL's handler
  `$EA31`,
* the zero page.

Nothing of the hardware is saved at the start for this: the values are
the KERNAL's, and the exit is about 20 bytes. The border and background
colours, the sprites' positions and colours and the text colour stay as
the program left them, so the program's last screen stays readable (and
the tests can read the border colour). What was different before `RUN`
is not brought back: lower case switched on by hand, a screen moved
somewhere else, an interrupt handler of a cartridge in `$0314`.

The backend generates this code for each program and leaves out what the
program cannot need:

| Step | Left out when |
|---|---|
| zero page | no zero-page statics, and no function frame in the zero page is used (otherwise only the used bytes are saved) |
| stack pointer | nothing can reach `sys::exit()` or a panic |
| clearing statics | no static starts as zero (a few bytes of them stay in the file instead) |
| memory configuration, CIAs, VIC, SID | every write of the program goes to a known, harmless place: its own variables, or a fixed address outside `$00/$01`, `$0314-$0319` and the I/O area (the colours `$D020-$D02E` are harmless) |
| IRQ vector | the program and the library routines it uses name none of the vectors `$0314-$0319` |

Anything the backend cannot follow keeps the step for the hardware: a
store through a pointer, an indexed store to a numeric address, `sei`, a
call to a numeric address outside the ROMs or through a vector. Calls
into the KERNAL and BASIC ROMs do not count. A program that needs none of
the steps starts directly with `main`: `examples/border` (`lda #5`,
`sta $d020`, `rts`) is a 20-byte file, 12 of them the BASIC line.

### Static frames

There is no stack for data: every function gets a fixed block of memory
(its frame) in the BSS for its parameters, locals and temporaries. A call
passes the first three bytes of the arguments in A, X and Y and copies
the rest into the callee's frame; a result of up to three bytes comes
back in A, X and Y, a larger one in the callee's frame. `&local` is an
ordinary 16-bit address.

This is fast on the 6510, which has only 256 bytes of hardware stack and
no stack-relative addressing, but it has two consequences:

* **No recursion**: a function must not run twice at the same time.
* **Interrupts**: a handler must not run a function that the main program
  is in the middle of. rust64 solves this by compiling every function
  that is reached through a function pointer (interrupt handlers are
  `fn()` values) a second time, with its own frames. Listings call these
  copies `(pointer copy)`. Statics are shared as usual: use
  `sys::without_interrupts` when main and a handler change the same data.

Values live in memory, not in registers: the code generator makes each
operation load, compute and store byte by byte. A data-flow pass over the
assembly of each function then removes most of what that costs (chapter
10), and only the bytes of a frame that are still used get memory.

### Runtime registers

The arithmetic helpers (`rt_mul8`, `rt_mul16`, `rt_mul16s`/`rt_mul16u`, `rt_mul32`, `rt_mul`, `rt_udiv8`/`16`/`32`, `rt_udiv`, `rt_sdiv`, `rt_memcpy`, ...)
use registers of their own: `rt_a`, `rt_b`, `rt_r`, `rt_t` (8 bytes each),
`rt_n`, `rt_cnt`, in the zero page where the program leaves room (see
"Zero page" below), else in the BSS; calls through pointers use `rt_args`
(32 bytes), `rt_ret` (16 bytes) and `rt_jvec`. The interrupt dispatcher saves them
(`rt_ctx_save`), but only the groups that the code reached by handlers
uses: the arithmetic registers (39 bytes), `rt_args`/`rt_ret` (48 bytes),
the argument areas of assembly functions. The six zero-page pointer
bytes are saved if that code uses the scratch pointers or any of these
groups, `rt_jvec` if the program calls through function pointers
anywhere. Handlers that only write hardware registers cost nothing here
(`RUST64_CTX_ALL=1` saves pointers and vector always). Only the helpers a
program uses are part of it.

### Zero page

The zero page `$02-$8F` (BASIC's, free while the program runs) holds the
`#[c64::zp]` statics, then what the library's assembly works with (the
registers of the runtime's multiplication, division and block routines,
`rt_a`, `rt_b`, `rt_r`, ..., and the argument areas of the assembly
routines the program calls: only the ones it uses, 10 to 80 bytes), and in
what is left the frames at the bottom of the frame overlay: the leaf
functions first, and of each frame the small values (up to four bytes)
before the structs and arrays. The assembly names its registers and
arguments in nearly every instruction, so this makes the library routines
shorter and faster (`pi`: 12% smaller, 15% fewer cycles). Where
there is no room (many `#[c64::zp]` statics) they stay in the BSS. Machine code that you call (`sys::jsr`, a SID
tune's player) must not use these addresses for its own data; `$FB-$FE`,
the usual choice of such code, are the runtime's scratch pointers and may
be overwritten by it between calls. `c64::g3d` uses `$F7`/`$F8` as well
(the KERNAL's pointer to the RS-232 input buffer, free while no RS-232
channel is open).

---

## 6. Attributes and macros

### `#[c64::main]` — the entry point

```rust
#[c64::main]
fn main() { ... }
```

The function takes no arguments and returns `()`. (It generates an
`extern "C" fn` exported as `main`; you can also write that yourself.)

### `#[c64::at(ADDR)]` — fixed address

```rust
// loaded at $2000 together with the program
#[c64::at(0x2000)]
static SPRITE_DATA: [u8; 128] = *include_bytes!("sprites.bin");

// only placed: the memory is not initialized and not part of the file
#[c64::at(0xc000)]
static mut BUFFER: MaybeUninit<[u8; 1024]> = MaybeUninit::uninit();
```

* A static **with data** becomes part of the program file at that address
  (`$0801` or above, and above the program: the file is padded up to it).
* A static initialized with **`MaybeUninit::uninit()`** only gets the
  address. If it is in RAM above `$0801`, the build fails when the program
  and its BSS grow into it:
  `rust64: the program overlaps BUFFER at $c000`.
* Two placed statics must not overlap.

`MaybeUninit::uninit()` is free everywhere: a local variable or a field
that is set to it gets its room and no code (nothing is stored, and the
program holds no bytes for it). The library uses it for buffers that an
assembly routine fills, such as the name buffer of `c64::dos`.

Hardware registers are better reached through `c64::hw` (it uses pointers
to the fixed addresses), but `#[c64::at(0xd020)] static mut BORDER:
MaybeUninit<u8>` works too.

### `#[c64::zp]` — zero page

```rust
#[c64::zp]
static mut PTR: u16 = 0x0400;
```

The static goes to the zero page `$02-$8F` (fast, and usable as a
`(zp),y` pointer from assembly). It is initialized at start; it must not
contain references. The zero page has 142 bytes for these statics.

### `include_bytes!` — embed a file

This is plain Rust: `static DATA: [u8; N] = *include_bytes!("file.bin");`
puts the bytes into the program's data, with `#[c64::at]` at an address.
A `&'static [u8]` works too:
`static PIC: &[u8] = include_bytes!("pic.kla");`.

### `c64::sid!` — a SID tune

```rust
c64::sid!(MUSIC, "music.sid");          // path relative to Cargo.toml

MUSIC.start(MUSIC.start_song);          // init + play every frame
MUSIC.stop();
```

The macro reads the PSID/RSID header at compile time, places the tune's
data at its load address and defines `static MUSIC: c64::sound::Tune`
with `init`, `play`, `songs` and `start_song` (0-based). Tunes without a
play address (that install their own interrupt) are rejected; so are
tunes that load below `$0801`.

### `c64::p!` / `c64::pm!` — PETSCII literals

```rust
kernal::setnam(c64::p!("HIGHSCORE"));
text::print_petscii(c64::pm!("Mixed Case"));
```

### `c64::s!` / `c64::sm!` — screen code literals

```rust
text::put_codes(11, 1, c64::s!("rust64 sprite demo"), Color::White);
static MESSAGE: &[u8] = c64::s!("hello from rust64 ... ");
```

The text is converted at compile time. `text::put_str` converts every
character when the program runs, and brings the conversion routines
(83 bytes) into the program.

### `c64::rat_source!` — include assembly

See chapter 7.

### Exported statics and functions

`#[unsafe(no_mangle)]` (or `#[unsafe(export_name = "...")]`) on a static
or a function makes its symbol visible to assembly code, even when no Rust
code uses it:

```rust
#[unsafe(no_mangle)]
static mut player_x: u8 = 0;         // assembly: lda player_x
```

Exported functions are called with the function pointer convention
(arguments in `rt_args`, result in `rt_ret`).

---

## 7. Assembly functions

Routines written in rat assembly are declared as `extern "C"` functions
and included with `c64::rat_source!`:

```rust
c64::rat_source!(DOUBLE, "double.rat");    // relative to this .rs file

unsafe extern "C" {
    fn double(v: u16) -> u16;
}

let x = unsafe { double(21) };            // 42
```

`src/double.rat`:

```
; double(v: u16) -> u16: v * 2
double:
        asl double__args
        rol double__args+1
        lda double__args
        ldx double__args+1
        rts
```

The macro creates a byte-array static in the link section `rat`; the
backend includes the text of all such statics of the program and its
dependencies. Labels starting with `@` are local to the previous label
(rat syntax).

### Calling convention

| What | Where |
|---|---|
| all argument bytes, in order | `<name>__args` (defined by the backend) |
| the first three argument bytes | also in A, X, Y |
| a result of 1-3 bytes | A, X, Y (low byte in A) |
| a larger result | `<name>__res` |

Arguments are laid out one after the other: `fn f(a: u8, p: *const u8,
n: u16)` has `a` at `f__args`, `p` at `f__args+1`, `n` at `f__args+3`.
Structs as arguments or results need `#[repr(C)]`. A `&[u8]` is 4 bytes:
pointer, then length.

The routine may use A, X, Y, the zero-page pointers `zp_p0` (`$FB`),
`zp_p1` (`$FD`), `zp_p2` (`$F9`) and the runtime registers. It must end
with `rts` (or `jmp` to another routine). Useful runtime labels:

| Label | Use |
|---|---|
| `rt_memcpy` | copy `rt_n` bytes from `(zp_p0)` to `(zp_p1)` |
| `rt_memset` | set `rt_n` bytes at `(zp_p1)` to A |
| `rt_exit` | return to BASIC |
| `rt_panic` | red border, return to BASIC |
| `rt_jsrind` | `jmp (rt_jvec)` |

Only the routines the program refers to are in the `.prg`: the text is
cut at its global labels, and a routine, table or equate is kept if the
code or something that is kept names it. A routine can have a second,
short form for programs that have a larger routine anyway:

```
small:
.ifdef large {
        ...                     ; pass the work on
        jmp large
} .else {
        ...                     ; do it here
}
```

What is in the first branch counts only if `large` is kept for another
reason, so `small` alone does not bring `large` into the program
(`c64_fmt_dec16` and `c64_fmt_dec` in `c64`'s `text.rat` work like this).

### Inline assembly

`c64::asm!` puts assembly lines into a function, without a call:

```rust
fn add(a: u16, b: u8) -> u16 {
    let sum: u16;
    unsafe {
        c64::asm!(
            "clc",
            "lda {a}",
            "adc {b}",
            "sta {sum}",
            "lda {a}+1",
            "adc #0",
            "sta {sum}+1",
            a = in(reg) a,
            b = in(reg) b,
            sum = out(reg) sum,
        );
    }
    sum
}
```

It is `core::arch::asm!` (the macro spares the program the feature gate)
with 6510 code in rat syntax. rust64 keeps values in memory, not in
registers, so a placeholder stands for the **memory location** of its
operand: an address, used like a label (`lda {x}`, `sta {x}+1`).

| Operand | `{name}` is |
|---|---|
| `in(reg) value` | where the value is (8 or 16 bits; a constant or a value behind a pointer is copied to a temporary first) |
| `out(reg) place`, `lateout(reg) place` | where the result goes |
| `inout(reg) var` | a copy of `var` that is stored back afterwards |
| `const N` | the number: `lda #{n}` |
| `sym STATIC` | the label of the static |

A, X, Y and the flags are free inside the block; they are undefined at
its start and after it. The same holds for the scratch pointers `zp_p0`
to `zp_p2`: a block that uses one sets it up itself.
Local labels start with `@` and belong to the block (`"@loop: dex"`,
`"bne @loop"`). The runtime's labels and the routines of rat sources can
be used (`jsr rt_memcpy`, `sta zp_p0`). The library uses this for its small
wrappers: `kernal::chrout(c)` is `lda c`, `jsr $ffd2`.

The lines of a block are part of the function, and the optimizer reads
them like its own code: an operand that holds a constant becomes an
immediate (`lda {c}` with `c = 5` is `lda #$05`), a load of a value that
the register already holds goes, and so does an instruction whose result
nothing uses. Code where every instruction and cycle counts (a stable
raster routine) belongs in a rat source, which is taken as it is. A
function is also left as it is when the optimizer cannot follow its
assembly completely: directives (`.byte`), labels other than `@labels`, a
subroutine inside the block, code that changes itself or takes the
address of one of its labels, decimal mode.

Use a rat source instead when the code is long, is called from several
places (inline code is repeated at every use) or needs data tables.

A KERNAL routine can be declared directly when its registers fit the
convention (`c64_chrout = $ffd2` in the library's `kernal.rat`). Look at
`lib/c64/src/rat/*.rat` for more examples.

rat also assembles the stable undocumented opcodes of the 6510 (`lax`,
`sax`, `dcp`, `isc`, `slo`, `sre`, `rla`, `rra`, `anc`, `alr`, `arr`,
`sbx`). Use them in rat sources only: the pass that reads `c64::asm!`
blocks does not know them. A rat source is one byte-array static, and no
object can be larger than 32767 bytes on this target: split a longer
source into two files (`g3d.rat` and `g3d_line.rat` in the library).

---

## 8. Library reference

The `c64` crate has these modules. Everything that touches the hardware
is safe to call; only raw memory access (`mem::bytes`, `sys::jsr`) is
`unsafe`.

### c64::hw — hardware registers

```rust
use c64::hw::{self, Color};

hw::vic().border.write(Color::Green);          // $d020
hw::vic().background[0].write(Color::Black);   // $d021
let line = hw::vic().raster.read();             // $d012
hw::sid().voice[0].freq.write(7493);
hw::cia1().pra.read();
```

| Item | Description |
|---|---|
| `vic()`, `sid()`, `cia1()`, `cia2()` | the chips as `#[repr(C)]` register structs |
| `Vic` | `sprite[8].{x,y}`, `sprite_x_msb`, `control1`, `raster`, `sprite_enable`, `control2`, `memory`, `irq_status`, `irq_enable`, `sprite_priority`, `sprite_multicolor`, `sprite_expand_x/_y`, `sprite_collision`, `sprite_bg_collision`, `border`, `background[4]`, `sprite_mc[2]`, `sprite_color[8]` |
| `Sid` | `voice[3].{freq, pulse, control, attack_decay, sustain_release}`, `cutoff`, `resonance`, `mode_volume`, `pot_x/_y`, `osc3`, `env3` |
| `Cia` | `pra`, `prb`, `ddra`, `ddrb`, `timer_a/_b`, `tod_*`, `sdr`, `icr`, `cra`, `crb` |
| `Volatile<T>` | `read()`, `write(v)`, `modify(f)`; for `u8` also `set_bits`, `clear_bits` |
| `ColorReg` | `read() -> Color`, `write(Color)` (colour registers) |
| `Color` | the 16 colours; `Color::from_u8(n)`, `c as u8` |
| `screen()`, `color_ram()`, `sprite_pointers()` | `[Volatile<u8>; N]` at `$0400`, `$D800`, `$07F8` |
| `cpu_port()` | `$01`, with `ALL_ROMS`, `NO_BASIC`, `IO_ONLY` |
| `raster_line()` | the 9-bit raster line (0-311) |
| `wait_raster(line)` | wait until the beam reaches `line` (0-255) in the next frame |
| `wait_frame()` | wait for the bottom of the screen (line 251) |
| `set_vic_bank(n)` | VIC bank 0-3 (`$0000`, `$4000`, `$8000`, `$C000`) |
| `set_screen(screen, charset)` | `$D018` in units of 1K and 2K |
| `bank_out_basic()` | RAM at `$A000-$BFFF` |

### c64::kernal — KERNAL ROM

Thin wrappers; routines that report errors return `Result<_, Error>`
(`Error::FILE_NOT_FOUND`, `Error::DEVICE_NOT_PRESENT`, ...).

| Function | KERNAL |
|---|---|
| `chrout(c)`, `chrin()`, `getin()` | `$FFD2`, `$FFCF`, `$FFE4` |
| `scnkey()`, `stop()` | `$FF9F`, `$FFE1` |
| `plot(row, col)`, `cursor() -> (row, col)` | `$FFF0` |
| `setlfs(lfn, dev, sa)`, `setnam(name)` | `$FFBA`, `$FFBD` |
| `open()`, `close(lfn)`, `chkin(lfn)`, `chkout(lfn)`, `clrchn()`, `clall()` | `$FFC0`... |
| `readst()` | `$FFB7` |
| `load(verify, addr) -> Result<end>`, `save(start, end)` | `$FFD5`, `$FFD8` |
| `rdtim() -> u32`, `settim(t)`, `jiffies() -> u16` | the 1/60 s clock |

Constants: `RETURN`, `CLEAR`, `HOME`, `UP`, `DOWN`, `LEFT`, `RIGHT`,
`REVERSE_ON`, `REVERSE_OFF`, `LOWERCASE`, `UPPERCASE`, `DELETE`.

### c64::text — the character screen

| Function | Description |
|---|---|
| `print(s)`, `println(s)` | print ASCII at the cursor (PETSCII conversion) |
| `print_petscii(bytes)` | print PETSCII bytes unchanged |
| `clear()`, `locate(col, row)` | clear the screen, move the cursor |
| `set_color(c)`, `colors(border, bg)` | text colour; border and background |
| `lowercase()`, `uppercase()` | switch the character set |
| `put_char(col, row, code, color)` | write a screen code directly |
| `put_str(col, row, s, color)` | write a string directly (no cursor) |
| `put_codes(col, row, codes, color)` | the same for screen codes (a `c64::s!` literal): no conversion |
| `fill(code, color)`, `fill_rect(col, row, w, h, code, color)` | fill the screen or a rectangle |
| `scroll_up(code, color)` | move everything up one line |
| `set_screen_addr(addr)`, `set_charset(addr)` | another screen / character set |
| `petscii(c)`, `screen_code(c)`, `petscii_to_screen(c)` | conversions |
| `Console` | the screen as a `fmt::Write` |
| `WIDTH`, `HEIGHT` | 40, 25 |

### c64::fmt — formatting

```rust
c64::println!("x=", x, " y=", y);
c64::print!(Hex(0xd020u16), " ", Bin(5u8));     // D020 00000101
let mut b = c64::fmt::Buf::<16>::new();
c64::write!(b, "score ", score);
text::put_str(0, 0, core::str::from_utf8(b.as_bytes()).unwrap_or(""), Color::White);
```

| Item | Description |
|---|---|
| `trait Write` | `write_byte`, `write_bytes`, `write_str` |
| `trait Display` | `fmt<W: Write>(&self, w)`, for `str`, `[u8]`, `bool`, `char`, all integers, `&T` |
| `Hex(v)`, `Bin(v)` | all digits, for `u8`/`u16`/`u32` (`Bin`: `u8`/`u16`) |
| `Buf<N>` | a fixed text buffer: `as_bytes`, `len`, `clear` |
| `write_u16`, `write_u32` | decimal without division; a program that prints no number wider than 16 bits only has the small routine for the digits |
| `print!`, `println!`, `write!` | comma-separated values |

Implement `Display` for your own types:

```rust
struct Pos(u8, u8);
impl c64::fmt::Display for Pos {
    fn fmt<W: c64::fmt::Write>(&self, w: &mut W) {
        c64::write!(*w, self.0, ',', self.1);
    }
}
```

### c64::sprite — hardware sprites

```rust
use c64::sprite::{self, Sprite};
sprite::load(13, &BALL);         // image block 13 = $0340
let s = Sprite(0);
s.set_image(13);
s.set_color(Color::Yellow);
s.move_to(160, 120);
s.show();
```

| Item | Description |
|---|---|
| `load(block, data)` | copy an image (up to 64 bytes) to `block * 64` |
| `Sprite(n)` | `set_image`, `show`, `hide`, `move_to(x: u16, y: u8)`, `x()`, `y()`, `set_color`, `set_multicolor`, `expand(x, y)`, `behind`, `hit(mask)` |
| `hide_all()`, `shared_colors(c1, c2)` | |
| `collisions()`, `bg_collisions()` | collision bits (cleared on read) |
| `set_pointer_addr(addr)` | after moving the screen |

The visible screen starts at x = 24, y = 50.

### c64::gfx — bitmap graphics

```rust
let g = gfx::hires(Color::White, Color::Black);    // Bitmap<Hires>
g.line(0, 0, 319, 199);
g.circle(160, 100, 50);
gfx::text();
```

| Item | Description |
|---|---|
| `hires(fg, bg) -> Bitmap<Hires>` | 320x200, clears the bitmap |
| `multicolor(bg, c1, c2, c3) -> Bitmap<Multicolor>` | 160x200, pens 0-3 |
| `show_koala(pic) -> Option<Bitmap<Multicolor>>` | a Koala picture (10003 bytes with load address) |
| `text()` | back to the text screen |
| `Bitmap<M>` | `pen`, `clear`, `plot(x, y)`, `line(x0, y0, x1, y1)`, `fline` (the same, fast), `hline(x0, x1, y)`, `vline(x, y0, y1)`, `rect`, `fill_rect`, `circle` |
| `Bitmap<Hires>` | also `point(x, y)`, `set_colors`, `set_cell_colors(col, row, fg, bg)` |
| `bitmap.double_buffer() -> Double<M>` | double buffering: draws like the bitmap, into the one of two that is not on the screen; `flip()` shows it, `clear()` clears the hidden one, `single_buffer()` (or dropping it) ends it |
| `BITMAP`, `MATRIX` | `$6000`, `$4000` (VIC bank 1) |

Coordinates are `i16`; pixels outside the screen are skipped. The mode is
part of the type, so `point` only exists for hires bitmaps.

The drawing itself is assembly, one set of routines for each mode
(`lib/c64/src/rat/gfx_hires.rat` and `gfx_mc.rat`; the tables both use
are in `gfxtab.rat`). No routine asks for the mode when it runs, and a
program contains just the routines it uses, of the modes it draws in:

* `plot`: one pixel, about 150 cycles.
* `line`: with both ends on the screen, a mask and a pointer move from
  pixel to pixel instead of a new address for each one (about 50
  cycles per pixel). A line that leaves the screen is stepped pixel by
  pixel through the pixel routine; it draws the same pixels.
* `fline` draws the same line as `line`, and flat ones (at least 8
  pixels across for each line down, both ends on the screen) as a
  staircase of horizontal runs: each run is stored like a horizontal
  line, whole bytes at a time, about three times as fast. It adds some
  300 bytes to the program, which is why it is a method of its own.
* `hline` and `vline` draw horizontal and vertical lines without the
  line routine: a program that only needs those stays small. (`line`
  also takes this path when it gets such a line.)
* Horizontal and vertical lines, `fill_rect` and `rect` are areas: they
  are cut to the screen first, then whole bytes are stored (about 25
  cycles for 8 pixels) or, for one column, the routine goes down the
  column (about 35 cycles per pixel).
* `circle`: eight pixels per step.

**Double buffering.** `double_buffer()` adds a second bitmap at `$A000`
with its colour matrix at `$8000` (VIC bank 2; the first is at `$6000`
and `$4000` in bank 1). Everything is drawn into the bitmap that is not
on the screen, and `flip()` shows it with one store to `$DD00`:

```rust
let mut d = gfx::hires(Color::White, Color::Black).double_buffer();
loop {
    d.clear();                  // the hidden bitmap: the picture before the last
    d.line(0, 0, x, 199);
    hw::wait_frame();
    d.flip();
}
```

`$A000` is under the BASIC ROM, which is switched off while there are two
bitmaps (the KERNAL stays; leaving the program brings it back). The
drawing routines need no test for it: they take every address from the
table of cell rows, and `flip` changes that table. A program without
double buffering has none of this code.

**Without clipping.** All of this checks the coordinates against the
screen. A program that never draws outside it can leave the checks out
with the feature `gfx-unclipped` of the `c64` crate:

```toml
[dependencies]
c64 = { path = "../c64_rust/lib/c64", features = ["gfx-unclipped"] }
```

or for one build: `cargo c64 build --release --features c64/gfx-unclipped`.
The feature holds for the whole program. The pixel routine then has no
range check, `line` does not look at its ends and the pixel-by-pixel
line is not in the program: `examples/drawing` shrinks from 2491 to 2103
bytes and draws 2.5% faster (the circles 6%; lines already ran without a
check per pixel). The price: **every pixel must be on the screen.** One
outside is written to other memory and can destroy the program. Empty
rectangles (`w` or `h` not positive) still draw nothing, and `point`
still answers `false` outside the screen. On the host the stand-ins
always clip.

### c64::g3d — bodies in three dimensions

```rust
use c64::g3d::{self, Matrix, View};

let mut g = gfx::hires(Color::White, Color::Black);
let m = Matrix::rotation(20, 40, 0);      // angles: 256 is a full turn
let mut view = View::<8, 6>::new();       // room for 8 corners and 6 faces
view.project(&g3d::CUBE, &m, 160, 100);   // the corners on the screen
view.draw(&g, &g3d::CUBE);                // the edges that are seen
```

| Item | Description |
|---|---|
| `Matrix` | a rotation, 9 signed bytes (127 is 1.0): `IDENTITY`, `rotation(ax, ay, az)`, `turn_x(a)`, `turn_y(a)`, `turn_z(a)`, `apply([x, y, z])` |
| `sin(a)`, `cos(a)` | 127 * sine and cosine; 256 is a full turn |
| `Shape { vertices, faces, edges }` | a body: corners `[x, y, z]`, faces (three corners each), edges (two corners and two faces each) |
| `CUBE`, `PYRAMID` | two bodies |
| `View::<V, F>::new()` | a body as it is seen: room for `V` corners (up to 64) and `F` faces (up to 127) |
| `view.project(&shape, &m, cx, cy)` | turns the corners, puts them on the screen around (`cx`, `cy`), finds the faces that are seen |
| `view.project_perspective(&shape, &m, cx, cy, depth)` | the same with a vanishing point: what is further away is smaller; `depth` 0 (none) to 127 |
| `view.draw(&g, &shape)` | draws the edges that are seen with the pen of the hires bitmap `g` |
| `view.points`, `view.is_seen(face)` | the corners on the screen; whether a face shows its front |
| `NO_FACE` | for an edge without a (second) face |

Coordinates are signed bytes: x to the right, y up, z towards the viewer.
A corner must not be further than 120 from the origin. `project` is a
parallel projection, one pixel for each unit, whatever the depth of a
corner.

**The vanishing point.** `project_perspective` draws what is further away
smaller. `depth` says how much: the viewer is 256 * 128 / `depth` units in
front of the origin (256 units for 127, 512 for 64), and what is at the
origin keeps its size. A corner at depth `z` (towards the viewer) is
drawn 256 / (256 - z * depth / 128) times as far from the centre, up to
twice for the nearest corners; the picture of a body must stay less than
256 pixels wide and high. Hidden edges work as before: a face is judged
by its corners on the screen.

```rust
view.project_perspective(&g3d::CUBE, &m, 160, 100, 127);
view.draw(&g, &g3d::CUBE);
```

`examples/perspective` turns a long bar (36 x 36 x 170) this way: the end
that points to the viewer is large, the far end small. The fire button of
joystick 2 shows the same bar as a parallel projection (`depth` 0).

The size comes from a table (256 bytes, 32768 / n, filled when the
routine is first used: 0.2 seconds, once). A corner then costs its depth
(three more products), one multiplication for the depth and two for x
and y: about 1000 cycles more than the parallel projection, 8000 a
picture for the cube (`cargo c64 run --release --features perspective`
in `examples/cube3d`: 101 000 cycles a picture, 9.8 pictures a second).
A program that only calls `project` has none of this code.

**Hidden edges.** A face is given by three of its corners,
counter-clockwise when one looks at it from outside the body; it is seen
when its front points to the viewer. An edge names the two faces it
belongs to and is drawn when one of them is seen. For a convex body (a
cube, a pyramid, a prism) these are exactly the visible edges. A body
with dents shows the edges of all faces that point to the viewer, also
where another part is in front of them. An edge with `NO_FACE` as its
first face is always drawn, so a shape without faces is a wire model.

```rust
// a wedge: 6 corners, 5 faces, 9 edges
static WEDGE: g3d::Shape = g3d::Shape {
    vertices: &[[-50, -30, 40], [50, -30, 40], [50, -30, -40], [-50, -30, -40], [-50, 30, -40], [50, 30, -40]],
    faces: &[[0, 3, 2], [0, 1, 5], [3, 4, 5], [0, 4, 3], [1, 2, 5]],
    edges: &[
        [0, 1, 0, 1], [1, 2, 0, 4], [2, 3, 0, 2], [3, 0, 0, 3], [4, 5, 1, 2],
        [0, 4, 1, 3], [1, 5, 1, 4], [3, 4, 2, 3], [2, 5, 2, 4],
    ],
};
```

**Moving pictures.** Clearing the bitmap takes five frames. A body that
moves is drawn again with pen 0 from the view it was drawn with, then
the next view is drawn. `examples/cube3d` does that with double
buffering (`gfx::hires(..).double_buffer()`), so that no picture is seen
half drawn: each of the two bitmaps has its own view, the one that was
drawn into it last. Make the matrix new from the angles for every
picture; one that is turned on and on collects the rounding of every
step.

**Speed.** The cube of the example takes about 97 000 cycles a picture,
10.2 pictures a second (the example does not wait for the frame before it
changes the bitmaps, which is one store to a register; with
`hw::wait_frame()` in front of `flip` it is about 7):

| Part | Cycles a picture |
|---|---:|
| the lines: nine edges, taken away and drawn | 80 000 |
| the corners on the screen and the faces | 11 000 |
| the rotation matrix | 5 000 |

All of it is assembly (`lib/c64/src/rat/g3d.rat`):

* **The multiplication** of two signed bytes is two lookups in a table of
  squares (`a * b = sq(|a + b|) - sq(|a - b|)`, with the signs as they
  are: no factor is made positive first). A turn of the matrix is 12
  multiplications (those by 0 are left out), a face 2.
* **The corners** are taken entry by entry of the matrix: four pointers
  are set once for an entry, and its product with the coordinate of every
  corner is then two accesses with the coordinate as the index (64
  cycles a product with its addition, 6 products a corner).
* **Memory.** The table, the work area of the corners and the table of
  the perspective are 2.5 KB of RAM at `$9600`-`$9FFF`, filled when the
  first routine of the module runs,
  and the corners use the zero-page bytes `$F7`/`$F8` (the KERNAL's for
  RS-232) as a fourth pointer. Keep both free in a program that uses
  `c64::g3d`, and do not call the module from an interrupt handler while
  the main program is in it.
* **The lines** of the edges have loops of their own, used by this module
  only. Drawing sets pixels: 38 cycles a pixel for a flat line and 35 to
  37 for a steep one, which has a loop for each direction (the general
  line of `c64::gfx` writes any pattern through a mask and takes 47 and
  50). Taking a picture away (pen 0) clears the
  whole byte of the bitmap, once for every byte and line an edge runs
  through: 24 cycles for a pixel that needs no new byte, 34 for a steep
  line. **It takes the pixels next to an edge in the same byte with it**:
  with several bodies, take all of them away first, then draw them all.
  The loops draw the usual case: both ends on the screen, neither
  horizontal nor vertical. Every other line goes to the line routine of
  `c64::gfx`.
* **The feature `g3d-fast`** of the `c64` crate draws flat lines with a
  piece of code for each of the eight bits of a byte, with the mask in
  it, so that no mask is shifted and tested: 29 cycles a pixel in place
  of 38, for about 200 bytes more. The cube then takes 94 000 cycles
  (10.5 pictures a second).

  ```toml
  c64 = { path = "../c64_rust/lib/c64", features = ["g3d-fast"] }
  ```

### c64::sound — the SID

```rust
use c64::sound::{self, Note, Voice};
sound::volume(15);
let v = Voice(0);
v.adsr(0, 9, 0, 0);
v.note_on(Note::C4, sound::PULSE);
v.note_off();
```

| Item | Description |
|---|---|
| `Voice(n)` | `freq`, `pulse_width`, `adsr(a, d, s, r)`, `play(wave)`, `note_on(note, wave)`, `tone(freq, wave)`, `note_off()` |
| `Note(n)` | 0 = C0 ... 95 = B7; `Note::new(octave, semitone)`, constants `C3`...`C5` (`CS4` = C#4), `freq()` (PAL; a `const fn`: in a constant or static it is computed at compile time) |
| `reset()`, `volume(v)`, `filter(cutoff, resonance, voices, mode)` | |
| waveforms | `TRIANGLE`, `SAWTOOTH`, `PULSE`, `NOISE`, bits `GATE`, `SYNC`, `RING`, `TEST` |
| filter | `LOW_PASS`, `BAND_PASS`, `HIGH_PASS`, `VOICE3_OFF`, `FILTER_VOICE1..3` |
| `Tune` | from `c64::sid!`: `start(song)`, `stop()`, fields `init`, `play`, `songs`, `start_song` |

The SID's registers are write-only; the module keeps copies of the
control and volume registers.

### c64::irq — raster interrupts

```rust
fn top() { hw::vic().border.write(Color::Blue); }
fn bottom() { hw::vic().border.write(Color::Black); }

irq::add(50, top);         // at raster line 50
irq::add(200, bottom);     // ascending lines
irq::stop();
```

| Function | Description |
|---|---|
| `add(line, f: fn())` | install a handler (up to `MAX_HANDLERS` = 8) |
| `on_frame(f)` | once per frame at line 251 |
| `stop()` | remove all, restore the KERNAL interrupt |
| `count()` | installed handlers |
| `critical_section(f)` | run `f` with interrupts off |

Handlers run in order; after the last one of each frame the KERNAL's
keyboard scan and jiffy clock run. The CIA timer interrupt is off while
handlers are installed. If a handler runs past the next handler's line,
that one follows immediately. Handlers can call any function (they get
their own copies, see [5.3](#static-frames)); keep them short.

What an interrupt costs besides the handler depends on the handler: the
dispatcher saves only what the handlers' code can change (see "Runtime
registers"). Two handlers that write registers of the VIC cost about
530 cycles per frame together, most of it the KERNAL's keyboard scan.

### c64::input — joystick and keyboard

| Item | Description |
|---|---|
| `joy(Port::One / Port::Two) -> Joy` | `up()`, `down()`, `left()`, `right()`, `fire()`, `idle()` |
| `key() -> Option<u8>` | next key (PETSCII) from the buffer |
| `wait_key() -> u8` | wait for a key |
| `pressed(row, col)` | a key of the keyboard matrix |

### c64::dos — disk drives

```rust
let end = dos::load("LEVEL1", 8, 0xc000)?;
let mut f = dos::File::open(2, 8, 2, "SCORES,S,W")?;
f.write_str("12345")?;
drop(f);                                        // closes the file
dos::command(8, "S0:OLDFILE")?;
for e in dos::Dir::open(2, 8)? { ... }
```

| Item | Description |
|---|---|
| `load(name, dev, addr)`, `load_abs(name, dev)` | load to `addr` / to the file's address; returns the end |
| `save(name, dev, start, end)` | save `start..end` |
| `command(dev, cmd)`, `command_arg(dev, cmd, arg)`, `scratch(dev, name)`, `rename(dev, from, to)` | DOS commands (`command_arg`: a command and what it applies to, `"S0:"` and a name) |
| `status(dev, buf) -> Option<(len, code)>` | the drive status (`00, OK,00,00`), at most 255 bytes of it |
| `File::open(lfn, dev, sa, name)` | `get()`, `read(buf)`, `eof()`, `write(bytes)`, `write_str(s)`; closed on drop |
| `Dir::open(lfn, dev)` | an `Iterator<Item = DirEntry>`; `header`, `free()` |
| `DirEntry` | `blocks`, `name()` (PETSCII), `kind` (`PRG`, `SEQ`, ...) |

File names are ASCII and converted to PETSCII capitals.

### c64::mem — raw memory

| Function | Description |
|---|---|
| `peek(a)`, `poke(a, v)`, `peek_word(a)`, `poke_word(a, v)` | volatile accesses |
| `copy(dst, src, n)` | fast copy, front to back; up to 255 bytes it is a loop in place, two bytes per round (15.5 cycles a byte; for an odd `n` one byte first), more goes through `rt_memcpy` |
| `fill(dst, n, v)` | fast fill; up to 255 bytes a loop in place (two bytes per round when `n` is even) |
| `unsafe bytes(a, n) -> &'static mut [u8]` | memory as a slice |
| `addr(a) -> *mut u8` | a pointer to an address |

### c64::sys — program control

| Item | Description |
|---|---|
| `exit() -> !` | back to BASIC from anywhere |
| `panic() -> !` | red border, back to BASIC |
| `sei()`, `cli()`, `without_interrupts(f)` | the interrupt flag |
| `unsafe jsr(addr, Regs) -> Regs` | call machine code with A/X/Y, get A/X/Y/P back (`carry()`) |

### c64::ui — boxes on the text screen

Boxes drawn with the PETSCII line characters, written straight into the
screen and colour memory. See `examples/ui`.

```rust
use c64::ui::{self, Border};
ui::frame(2, 2, 20, 8, Border::Round, Color::White);
ui::titled_panel(10, 6, 24, 10, Border::Square, Color::Yellow, " OPTIONS ");
```

| Item | Description |
|---|---|
| `frame(col, row, w, h, border, color)` | the lines of a box; what is inside stays |
| `panel(...)` | a box with the inside cleared (a window on top of other text) |
| `titled_frame(..., title)`, `titled_panel(..., title)` | with a title in the top line, from the third column, cut off at `w - 4` characters |
| `Border::Square`, `Border::Round` | the corners; round ones need the upper case / graphics character set |

`w` and `h` include the lines. A box smaller than 2 x 2 or not completely
on the screen is not drawn.

---

### More hardware: separate crates

Four crates next to `c64` (in `lib/`) support hardware that not every
C64 has. A program names the ones it needs in its `Cargo.toml`
(`c64-uci = { path = "../../lib/c64-uci" }`); what it does not call is not
in the program. Each has an example in `examples/` (except the sampler)
and a test in `tests/e2e/`.

**`c64-sd2iec`** — directories, disk images and settings of an SD2IEC,
sent on the command channel; files are read with `c64::dos` as usual.

```rust
use c64_sd2iec as sd;
if sd::detect(8) {
    sd::cd(8, "games")?;                 // or "/games/arcade"
    sd::mount(8, "disk1.d64")?;          // the image is now the disk
    let end = c64::dos::load("intro", 8, 0xc000)?;
    sd::unmount(8)?;
}
```

| Item | Description |
|---|---|
| `detect(dev)` | whether the drive is an SD2IEC |
| `cd`, `cd_parent`, `cd_root`, `mkdir`, `rmdir` | directories |
| `mount(dev, image)`, `unmount(dev)` | disk images (D64, D71, D81) |
| `check(dev)`, `status(dev, buf)` | the error number / status text of the last command (39 = path not found) |
| `set_device`, `hide_extensions`, `images_as`, `save_settings`, `version`, `command` | settings and anything else |

**`c64-uci`** — the Ultimate command interface of the 1541 Ultimate-II(+)
and Ultimate 64 (`$DF1C`; enable it in the Ultimate's menu): files on its
storage, TCP/UDP sockets, control.

```rust
use c64_uci::{self as uci, dos, net};
if uci::present() {
    dos::open("/Usb0/scores.txt", dos::READ)?;
    let n = dos::read(&mut buf)?;
    dos::close()?;
    let s = net::tcp_connect("example.org", 80)?;
    net::write(s, b"GET / HTTP/1.0\r\n\r\n")?;
    let n = net::read(s, &mut buf)?;
    net::close(s)?;
}
```

| Item | Description |
|---|---|
| `present()` | check first: the other functions wait for the Ultimate's answer |
| `dos::` | `identify`, `open(name, flags)`, `close`, `read`, `write`, `seek`, `file_size`, `file_info`, `delete`, `cd`, `path`, `mkdir`, `open_dir`/`read_dir`, `mount`/`unmount`, `time` |
| `net::` | `identify`, `interface_count`, `ip_address`, `tcp_connect`, `udp_connect`, `read`, `write`, `close` |
| `control::` | `identify`, `hardware`, `reboot` |
| `begin`, `put`, `put_u16`, `put_bytes`, `push`, `read`, `status`, `code`, `accept`, `finish`, `request` | the steps of a command, for commands without a function |
| `Error(n)` | the number of the status text (`82,FILE NOT FOUND`) |

**`c64-wic64`** — the WiC64 WiFi module on the user port: one request,
one response.

```rust
use c64_wic64 as wic;
if wic::present() {
    let n = wic::http_get("http://example.org/hello.txt", &mut buf)?;
    // larger than the buffer: request, then read piece by piece
    let len = wic::request(wic::cmd::HTTP_GET, url.as_bytes())?;
    while wic::remaining() > 0 { let n = wic::read(&mut buf); ... }
}
```

| Item | Description |
|---|---|
| `present()`, `is_connected()`, `version`, `ip`, `mac`, `ssid`, `rssi`, `local_time` | the module and its network |
| `http_get`, `http_post`, `server`, `set_server` | HTTP (`http://` only) |
| `tcp_open("host:port")`, `tcp_available`, `tcp_read`, `tcp_write`, `tcp_close` | one TCP connection |
| `command`, `request`, `read`, `remaining`, `cmd::*` | any command |
| `set_wait`, `set_remote_timeout`, `status_message`, `echo` | time-outs, the text of the last error, a connection test |
| `Error(n)` | 1 internal, 2 client, 3 no WiFi, 4 network, 5 server, 255 no WiC64 |

**`c64-uaudio`** — Ultimate Audio, the sampler of the Ultimate-II+ and
Ultimate 64 (`$DF20`; needs the REU and "Map Ultimate Audio" switched on):
seven voices play samples from REU memory.

```rust
use c64_uaudio::{self as ua, Voice};
if ua::present() {
    let v = Voice(0);
    v.sample(0, 8000);                   // REU address, length
    v.rate(ua::rate(11025));
    v.volume(40);
    v.pan(ua::CENTER);
    v.play(0);                           // or REPEAT, IRQ, BITS_16
}
```

mimic emulates the SD2IEC, the command interface and the WiC64, and the
tests run against it. It does not emulate the sampler: `c64-uaudio` follows
the Ultimate's FPGA source and has not been run against hardware.

---

## 9. Cookbook

All recipes compile with the current rust64. Each is a complete
`src/main.rs` or a part of one (with the `#![no_std]`, `#![no_main]`
header from chapter 2).

### A moving sprite with the joystick

`examples/sprites`: a ball moves with the joystick in port 2; fire changes
its colour; a raster interrupt paints a band behind the title.

```rust
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
    text::put_codes(11, 1, c64::s!("rust64 sprite demo"), Color::White);
    text::put_codes(8, 23, c64::s!("joystick 2 - fire: colour"), Color::Grey);

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
        if j.left() && x > 24 { x -= 2; }
        if j.right() && x < 320 { x += 2; }
        if j.up() && y > 50 { y -= 2; }
        if j.down() && y < 229 { y += 2; }
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
```

### Raster bars

Four handlers on consecutive lines change the border colour:

```rust
use c64::hw::{self, Color};
use c64::{input, irq};

static COLORS: [Color; 4] = [Color::Red, Color::Orange, Color::Yellow, Color::Black];
static mut BAR: u8 = 0;

fn bar() {
    unsafe {
        hw::vic().border.write(COLORS[BAR as usize]);
        BAR = (BAR + 1) & 3;
    }
}

fn raster_bars() {
    for line in [60, 70, 80, 90] {
        irq::add(line, bar);
    }
    input::wait_key();
    irq::stop();
}
```

A handler that only writes hardware registers starts within about two
raster lines of its line. It starts later when its code uses the scratch
pointers (three lines more) or the runtime's arithmetic helpers (about 16
lines more: the dispatcher saves their registers), and in a debug build
nearly every handler does, through rustc's runtime checks. Keep the lines
some distance apart. Where the screen allows it, leave a handler 30 lines or more
before its effect is needed (`examples/scroller` does), or build with
`--release`.

The colour changes at a slightly different place in every frame, somewhere
in the line: an interrupt starts up to seven cycles late, and Rust code
is not timed by the cycle. For bars with clean edges, one colour per
raster line, see `examples/rasterbars`: its `raster.rat` has a stable
raster interrupt (two interrupts in a row, then cycle-counted code that
also allows for the bad lines) that writes a 96-byte colour table to
`$D020`/`$D021`, one byte per line, while the beam is off screen. The Rust
program fills that table once per frame.

### Drawing on the bitmap

`examples/drawing`:

```rust
use c64::gfx;
use c64::hw::{self, Color};
use c64::input;

#[c64::main]
fn main() {
    hw::vic().border.write(Color::Black);
    let g = gfx::hires(Color::LightGreen, Color::Black);
    for i in (0..320i16).step_by(16) {
        g.line(i, 0, 319 - i, 199);
    }
    g.rect(40, 30, 240, 140);
    for r in (10..=60i16).step_by(10) {
        g.circle(160, 100, r);
    }
    for c in 0..40u8 {
        g.set_cell_colors(c, 24, Color::from_u8(c & 15 | 1), Color::Black);
    }
    input::wait_key();
    gfx::text();
}
```

### A picture that moves: double buffering

Drawing into the bitmap that is on the screen shows every picture half
drawn. With two bitmaps the picture is drawn where it is not seen:

```rust
use c64::gfx;
use c64::hw::{self, Color};
use c64::kernal;

#[c64::main]
fn main() {
    let mut d = gfx::hires(Color::White, Color::Black).double_buffer();
    let mut x = 0i16;
    while !kernal::stop() {
        d.clear();                  // the hidden bitmap
        d.line(x, 0, 319 - x, 199);
        d.circle(160, 100, 40);
        hw::wait_frame();           // change between two frames
        d.flip();                   // show it; draw into the other one
        x = (x + 4) % 320;
    }
    drop(d);                        // one bitmap again, BASIC ROM back
    gfx::text();
}
```

`clear` takes five frames. A program that knows what it drew takes that
away instead (the next recipe).

### A turning body

`examples/cube3d`: a cube from `c64::g3d`, a new rotation matrix for every
picture, hidden edges left out, two bitmaps. Each of the two bitmaps has
the view that was drawn into it last; its edges are taken away with pen
0, then the new view is drawn:

```rust
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
```

`examples/perspective` does the same with a body of its own (a `Shape` is
three tables: corners, faces, edges) and `project_perspective`.

### Playing notes

`examples/music` plays a melody on voice 1 and a bass on voice 2, timed
with the jiffy clock. The tune stores the frequencies of its notes, which
are computed when the program is compiled (`Note::freq` is a `const fn`),
and plays them with `tone`; `note_on(note, wave)` looks a note up when
the program runs and brings the table of all 96 notes (192 bytes) into
the program:

```rust
use c64::hw::{self, Color};
use c64::sound::{self, Note, Voice};
use c64::{kernal, text};

/// A note: its number (for the border colour), the SID's frequency value
/// and the length in jiffies. The frequency is looked up when the program
/// is compiled, so the program carries no table of all notes.
#[derive(Clone, Copy)]
struct Tone {
    note: u8,
    freq: u16,
    len: u8,
}

const fn n(note: Note, len: u8) -> Tone {
    Tone { note: note.0, freq: note.freq(), len }
}

static MELODY: [Tone; 26] = [
    n(Note::E4, 15), n(Note::D4, 15), n(Note::C4, 15), n(Note::D4, 15),
    n(Note::E4, 15), n(Note::E4, 15), n(Note::E4, 30),
    n(Note::D4, 15), n(Note::D4, 15), n(Note::D4, 30),
    n(Note::E4, 15), n(Note::G4, 15), n(Note::G4, 30),
    n(Note::E4, 15), n(Note::D4, 15), n(Note::C4, 15), n(Note::D4, 15),
    n(Note::E4, 15), n(Note::E4, 15), n(Note::E4, 15), n(Note::E4, 15),
    n(Note::D4, 15), n(Note::D4, 15), n(Note::E4, 15), n(Note::D4, 15),
    n(Note::C4, 60),
];

static BASS: [u16; 4] = [Note::C3.freq(), Note::G3.freq(), Note::A3.freq(), Note::F3.freq()];

fn wait(jiffies: u8) {
    let start = kernal::jiffies();
    while kernal::jiffies().wrapping_sub(start) < jiffies as u16 {}
}

#[c64::main]
fn main() {
    text::colors(Color::Blue, Color::Black);
    text::clear();
    c64::println!("rust64 music");
    sound::reset();
    sound::volume(15);
    let (lead, bass) = (Voice(0), Voice(1));
    lead.adsr(0, 9, 4, 4);
    lead.pulse_width(0x800);
    bass.adsr(0, 4, 10, 6);
    for (i, t) in MELODY.iter().enumerate() {
        if i % 4 == 0 {
            bass.tone(BASS[(i / 4) % 4], sound::TRIANGLE);
        }
        lead.tone(t.freq, sound::PULSE);
        hw::vic().border.write(Color::from_u8(t.note));
        wait(t.len - 3);
        lead.note_off();
        wait(3);
    }
    bass.note_off();
    wait(60);
    sound::reset();
    c64::println!("done");
}
```

### Playing a SID tune

```rust
use c64::input;

c64::sid!(MUSIC, "music.sid");

#[c64::main]
fn main() {
    c64::println!("playing, press a key");
    MUSIC.start(MUSIC.start_song);
    input::wait_key();
    MUSIC.stop();
}
```

The tune's data is loaded at its own address, so the program must end
below it. `start` calls the tune's init routine and its play routine once
per frame from a raster interrupt.

### Saving and loading a high score

`dos::save` writes a memory range as a program file; `dos::load` reads it
back to an address:

```rust
use c64::{dos, kernal};

static mut HIGH_SCORE: u16 = 0;

fn save_high_score() -> Result<(), kernal::Error> {
    let a = (&raw const HIGH_SCORE) as usize as u16;
    let _ = dos::scratch(8, "HIGHSCORE");       // replace an old file
    dos::save("HIGHSCORE", 8, a, a + 2)
}

fn load_high_score() {
    let a = (&raw mut HIGH_SCORE) as usize as u16;
    if dos::load("HIGHSCORE", 8, a).is_err() {
        unsafe { HIGH_SCORE = 0 };
    }
}
```

### Writing and reading a sequential file

```rust
use c64::dos::File;
use c64::{kernal, text};

fn write_log() -> Result<(), kernal::Error> {
    let mut f = File::open(2, 8, 2, "LOG,S,W")?;
    f.write_str("LEVEL 1 DONE\n")?;
    Ok(())                                 // f is closed here
}

fn read_log() {
    let Ok(mut f) = File::open(2, 8, 2, "LOG,S,R") else { return };
    let mut buf = [0u8; 40];
    let n = f.read(&mut buf);
    text::print_petscii(&buf[..n]);
}
```

Drive errors such as "file not found" are not KERNAL errors: read them
with `dos::status(8, &mut buf)`, which returns the two-digit code (62 =
file not found).

### Listing the directory

`examples/dir`:

```rust
use c64::dos::{self, Dir};
use c64::text;

#[c64::main]
fn main() {
    let mut d = match Dir::open(2, 8) {
        Ok(d) => d,
        Err(e) => {
            c64::println!("cannot open directory, error ", e.0);
            return;
        }
    };
    c64::print!("disk: ");
    text::print_petscii(d.header.name());
    c64::println!();
    for e in &mut d {
        c64::print!(e.blocks, " ");
        text::print_petscii(e.name());
        c64::print!(" ");
        text::print_petscii(&e.kind);
        c64::println!();
    }
    c64::println!(d.free(), " blocks free");
    drop(d);

    let mut buf = [0u8; 40];
    if let Some((n, _)) = dos::status(8, &mut buf) {
        text::print_petscii(&buf[..n]);
        c64::println!();
    }
}
```

Build it as a disk image and run it from there, so the drive has
something to list:

```
cargo c64 run --release -o dir.d64
```

### A custom character set

Copy the ROM character set into RAM, change a character, and point the
VIC at the copy (the program must end below `$3000`):

```rust
use c64::{hw, mem, sys, text};

fn custom_charset() {
    sys::without_interrupts(|| {
        hw::cpu_port().write(0x33);          // character ROM at $D000
        mem::copy(0x3000, 0xd000, 2048);
        hw::cpu_port().write(hw::ALL_ROMS);
    });
    // character 0 ("@"): a smiley
    let smiley = [0x3c, 0x42, 0xa5, 0x81, 0xa5, 0x99, 0x42, 0x3c];
    for (i, &b) in smiley.iter().enumerate() {
        mem::poke(0x3000 + i as u16, b);
    }
    text::set_charset(0x3000);
}
```

### Callbacks with function pointers

```rust
fn apply(f: fn(u8) -> u8, xs: &mut [u8]) {
    for x in xs {
        *x = f(*x);
    }
}

let mut xs = [1u8, 2, 3];
apply(|x| x * 3, &mut xs);                 // [3, 6, 9]
```

Generic functions (`fn apply<F: Fn(u8) -> u8>(f: F, ...)`) are faster:
the call is direct and can be inlined. Function pointers are for tables
of handlers and for interrupts.

### Zero page and fixed addresses

```rust
use core::mem::MaybeUninit;

#[c64::zp]
static mut FRAME: u8 = 0;                  // somewhere in $02-$8F

#[c64::at(0xc000)]
static mut BUFFER: MaybeUninit<[u8; 256]> = MaybeUninit::uninit();

unsafe {
    FRAME += 1;
    let p = (&raw mut BUFFER).cast::<u8>();  // 0xc000
    *p = 42;
}
```

### Formatting into a buffer

```rust
use c64::fmt::{Bin, Buf, Hex};
use c64::hw::Color;
use c64::text;

c64::println!("x=", 10u8, " hex ", Hex(0xd020u16), " bin ", Bin(5u8), " neg ", -3i16);
// X=10 HEX D020 BIN 00000101 NEG -3

let mut b = Buf::<16>::new();
c64::write!(b, "score ", 1200u16);
text::put_str(0, 24, core::str::from_utf8(b.as_bytes()).unwrap_or(""), Color::Yellow);
```

### Calling assembly

See chapter 7 for the `double` example with its own `.rat` file. For a
routine that already exists in ROM or memory, `sys::jsr` passes A, X, Y
and returns the registers and flags afterwards:

```rust
use c64::sys::{self, Regs};

// GETIN ($FFE4): A = the next key, 0 if none
let r = unsafe { sys::jsr(0xffe4, Regs::default()) };
if r.a != 0 {
    c64::println!("key ", r.a);
}
```

`sys::jsr` cannot set the carry flag before the call; routines that read
it (like PLOT) need a wrapper in assembly.

---

## 10. Inside the backend

```
 .rs ─► rustc (pinned nightly, target mos-c64.json: 16-bit pointers, os = c64)
        │  parse, types, borrow check, monomorphization, MIR optimizations
        ▼
  rustc_codegen_rust64   (-Zcodegen-backend)
        │  library crates: only metadata with MIR (-Zalways-encode-mir)
        │  the program: walk the MIR from `main` through all crates
        │  static frames, recursion check, pointer copies, data, zero page
        ▼
  rat assembly ─► rat-asm (linked in) ─► .prg + .d64
```

The target is a JSON specification based on `msp430` (rustc has no 6502
architecture; msp430 is 16-bit little endian, and LLVM is never called),
with `target_os = "c64"`, 16-bit pointers, alignment 1 everywhere,
`panic-strategy = immediate-abort` and the file suffix `.prg`.

The backend compiles the whole program at once, like Miri: starting at
`main` it fetches the MIR of every function it reaches
(`tcx.instance_mir`), in any crate, and translates it. This is why library
crates need `-Zalways-encode-mir`, and why unused code costs nothing.

Details worth knowing:

* Constants and `ub_checks` are folded: blocks behind a constant
  condition are not compiled.
* Diverging functions of `core` (`panic_fmt`, `slice_index_fail`, ...)
  become `jmp rt_panic` without being compiled; they would pull in
  `core::fmt`.
* Statics are evaluated by rustc (`const` evaluation) and emitted as
  bytes; pointers inside them become `.word` relocations.
* The crate scan finds `#[link_section = "rat"]` statics (assembly),
  `extern "C"` declarations (argument areas) and exported symbols in the
  program and every dependency except the standard library.

### What the code looks like

`examples/border` writes 5 to `$D020`:

```rust
#[unsafe(no_mangle)]
pub extern "C" fn main() {
    unsafe { core::ptr::write_volatile(0xd020 as *mut u8, 5) };
}
```

```
; fn main
F0:
        lda #$05
        sta $d020
        rts
```

Labels: `F<n>` is function n, `F<n>_b<k>` its basic block k, `F<n>f` its
frame, `F<n>v<i>` the storage of its local i, `F<n>t` its temporaries,
`L<n>_<i>` its result (0) and arguments (1...), `A<n>` constant data,
`S<n>` statics, `F<n>_ind` the pointer entry of a function,
`F<n>_i<k>`/`F<n>_e<k>` the start and the end of an `asm!` block.

The code generator (`codegen.rs`) keeps every value in memory and
translates one statement at a time. It already avoids part of the cost
of that:

* **Known addresses.** A pointer that always holds the same address
  (hardware registers, statics, `&local`) is used as a constant: accesses
  are direct (`sta $d020`), and the pointer itself is never stored.
* **Views instead of copies.** A temporary that is only a copy, a
  truncation, a zero-extension, a byte-preserving cast or a tuple/struct
  built from other values gets no storage: its users read (or dereference)
  the source bytes directly, also across branches, as long as the source
  cannot change before a use. Zero-extended high bytes become `#0`, which
  also shortens the 16-bit arithmetic that follows. A struct or array of
  more than 16 bytes that is only passed on (`let d = e; x.f = d`) is a
  view of the one place it was copied from.
* **Built where it goes.** A temporary of four bytes or more that is
  assigned once and then only moved into (a field of) another variable in
  the same block gets the bytes of that variable: `Name { buf: [0; 40],
  len: 0 }` fills `buf` where it stays, `Ok(d)` has nothing to copy for
  `d`.
* **Results where the call left them.** A result of more than three bytes
  is in the frame of the called function. A variable that only holds such
  a result and is read before the next call is that memory: no copy into
  the caller's frame (`match dir.next() { Some(e) => ..., None => ... }`
  reads the entry there).
* **Compare and branch.** A comparison that feeds a branch or a bounds
  check is not turned into a `bool`: `lda i / cmp #25 / bcc ok`. Constant
  high bytes are skipped, `x < 0` tests the sign bit. The same holds for
  `!(a < b)` (`is_err()`, `!done`): the branch goes the other way.
* **Checks that were made.** A comparison that the code can only reach
  through the true branch of the same comparison of the same values is not
  made again: in `if n < buf.len() { buf[n] = c }` the index has no check
  of its own.
* **Matches on enums.** Where a `match` covers all variants, the last one
  is not tested: it is what is left. An enum with two variants that has no
  tag of its own (`Option<&T>`, `Result<Dir, Error>` with the error in a
  value that the data cannot have) is decided by one comparison of that
  byte.
* **Tables.** Arrays of up to 256 bytes at a known address are indexed
  with X: `lda table,x`.
* **Small arithmetic.** Shifts of a byte by a constant are done in A,
  wider ones in place; unsigned `* / %` by powers of two become shifts and
  masks; `x += 1` is `inc` (with `bne`/`inc` for 16 bits); zero bytes of a
  constant are not added; `and #$ff` and similar disappear. `min` and
  `max` are a comparison and a copy; unsigned division of one, two and
  four bytes has routines of its own, and so has multiplication. A shift
  of a wider value by 8 bits or more moves bytes first.
* **Fixed point.** `(a as i32 * b as i32) >> k` with 16-bit `a` and `b` is
  one call of a 16 x 16 to 32 bit multiplication (`rt_mul16s`, or
  `rt_mul16u` for unsigned values): the operands are not widened first.
* **Quotient and remainder.** `d % g` and `d / g` with the same operands
  next to each other (the digits of a number) are one division.
* **Blocks of memory.** A copy or fill of a known size up to 255 bytes is
  a loop in place (5 to 11 bytes) instead of a call to `rt_memcpy` or
  `rt_memset`.
* **Block layout.** The blocks are emitted in reverse postorder, the
  usual successor directly after its block: the body of a loop stays
  together with the exit after it, both sides of an `if` and the cases of
  a `match` come before the code they join in. Most jumps fall through
  and most branches are short.

Then the assembly of each function goes through a **data-flow pass**
(`opt.rs`), which looks at the function as a whole:

* **Values.** For A, X, Y and every byte of the frame that is only used
  as a plain operand it follows which value the location holds (a
  constant, a copy of another location) and in which range the value is,
  through branches and loops. A read of a constant becomes an immediate,
  a read of a copy reads the original, a load of what another register
  holds becomes a transfer (`sta t` / `ldx t` is `tax`), and a load or
  store that changes nothing goes.
* **Ranges and branches.** A branch narrows the ranges on its two sides
  (`cmp #25` / `bcc`: below 25 where it is taken), so the bounds check of
  a loop counter or of a masked index (`i & 7` into eight elements) is
  known to pass and disappears with its comparison; a `clc` in front of an
  addition goes when the carry is known to be clear; code that cannot be
  reached is removed.
* **Liveness.** Stores to frame bytes that nobody reads afterwards, loads
  and computations whose result is not used go. At the end of a function
  only its result is needed; registers carry nothing into or out of a
  generated function, and an assembly function takes exactly the argument
  bytes of its signature in A, X and Y.
* **Copies.** Two frame bytes that are connected by a copy and never hold
  different values that are both still needed become one byte (as a
  register allocator merges copies), and the copy disappears. That removes
  most of the moves between the temporaries of iterators and ranges.
* **Index registers.** A pointer that was computed as address + byte or
  pointer + byte and is then only dereferenced becomes an indexed access,
  and its computation goes: `0x0400 + i as u16` written through is
  `ldx i` / `sta $0400,x`, an element of a slice is `ldy i` /
  `lda (slice),y`. Where the pointer and Y are both known (the `&mut self`
  of a function that was put into its caller, to a variable of the
  caller), the access is direct: `lda (ptr),y` becomes `lda d+2`.
* **Loop counters.** A frame byte that a loop only loads, stores and
  steps lives in X or Y for the time of the loop, when the loop does not
  need that register for something else and the byte is not used after it:
  `lda i` is `tya`, the step is `iny` at the end of the body, and the copy
  of the counter that a `for` loop works with is not needed any more.
* **Steps at the end.** A `for` loop over a slice steps its pointer (and
  `enumerate` its counter) first and works with a copy of the old value.
  The step moves to the end of the loop, where nothing needs the copy any
  more: the copy and the pointer become one, and the step is done in
  place (`inc p` / `bne` / `inc p+1`).
* **Out of the loop.** A computation that gives the same in every round
  (it reads only what the loop does not change: its own variables that
  the loop does not write, immutable statics, constants) moves in front
  of the loop, for example the address of the row that an inner loop
  works on. A bounds check of that kind moves too, when the loop is known
  to run at least once (a loop over an array or over a slice of known
  length).
* **A short test.** A loop over an array, or over a slice whose length is
  a constant, walks a pointer up to an end and compares the two, 16 bits
  wide, in every round. If the end is less than 256 bytes after the start,
  the low bytes alone tell when the pointer is there, and the test is
  `lda p` / `cmp end` / `bne`. If the loop also counts in X or Y
  (`enumerate`), the counter tells it without a load: `cpx #n` / `bne`.
* **An index for the pointer.** If the elements of such a loop are bytes
  and the loop needs Y for nothing else, the pointer stays at the start of
  the array and Y is the index: `lda (p),y` / ... / `iny` / `cpy #n` /
  `bne`, in place of a 16-bit step and a 16-bit test in every round. A
  second pointer that the loop steps along with the first (the
  destination of a copy loop written with `for`) stays where it is too;
  if it is needed after the loop, the number of rounds is added to it
  there.
* **Test at the end.** The test of a small loop moves behind its body, so
  each round ends with one conditional branch instead of a branch and a
  jump; the loop is entered with a jump to the test (or directly, when the
  first test is known to pass and the body needs nothing from it).

Finally, for the program as a whole:

* **Arguments.** The first three bytes of the arguments are in A, X and
  Y, and so is a result of up to three bytes; the callee stores them only
  if it needs them in memory. An argument byte in memory that the callee
  never reads is not passed.
* **Arguments that are not read.** For each function the pass notes which
  of A, X and Y it reads at its entry; the callers do not load the others.
  A reference to a value without bytes (`&mut Console`, the `self` of a
  type that only has methods) that a function only hands on is not an
  argument at all: the other arguments take its registers. A function
  that looks at the address itself still gets it.
* **One way back.** A loop with several jumps back to its head
  (`continue`, an `if` without `else` at the end of the body) gets one
  block for them, and the step of the loop goes there: `for &c in text`
  with a condition in the body steps its pointer in place like a loop
  without one.
* **Variables that share their bytes.** Where a value of four bytes or
  more is moved from one variable to another (`let d = match open() {
  Ok(d) => d, .. }`, `Ok(entry)` at the end of a function, `drop(d)`),
  the two get the same memory if they are never in use at the same time
  with different contents, and the move copies nothing. A value that is
  built in a variable and then returned is built in the result. A copy
  that is changed while the original is still read, two values that
  change places, a value that is needed while its successor is made:
  those keep their own bytes. `RUST64_NO_JOIN=1` turns this off.
* **Results of functions that are pasted in.** A function with one caller
  that returns its value in memory writes it straight to the caller's
  variable when its code stands in the caller, if no reference to that
  variable exists at the call.
* **Counters that fit a byte.** A `usize` or `u16` variable that starts at
  a small constant or a byte and is only stepped where it is known to be
  below a constant (`if n < 40 { buf[n] = c; n += 1 }`, a `while n < 100`)
  never reaches 256: where it is read its upper byte is the constant 0,
  and the code that kept that byte goes away. The comparison is one `cmp`,
  the step one `inc`, the index goes straight into X or Y. A counter with
  a limit of 300, or one that is stepped past 255, keeps both bytes.
  `RUST64_NO_NARROW=1` turns this off.
* **Addresses the assembler adds.** The end of an array in the frame or
  in the data (`for p in &parts`) is its address plus a constant: the
  code loads `#<(X+12)` instead of adding at run time, and a loop over at
  most 255 bytes compares only the low byte of its pointer with it.
  A pointer keeps its known address on the way through a slice
  (`&mut name[..]`, `iter_mut()`), through `NonNull` and through
  `ptr.add(16)`; the length of such an iterator, end minus start, is then
  a constant too. `RUST64_NO_ADDR_SUMS=1` turns this off.
* **Lengths that are a byte.** `&name[..len as usize]` with a `u8` length
  ends at `start + len`, and its iterator asks for `end - start` again:
  that is `len`, without the subtraction. What is made of such lengths
  stays a byte: the smaller of two (`zip` of two iterators), a copy, a
  counter that is stepped below one of them. A loop like
  `for (to, &from) in out.iter_mut().zip(e.name())` then is

  ```text
  loop:   tax
          lda name,x
          sta out,x
          iny
  test:   tya
          cmp len
          bcc loop
  ```

  where it added the counter to both addresses in 16 bits in every round.
  The slice may lie behind a reference (`&self.text[..self.len as
  usize]`): what counts is that start and end are made of the same
  pointer, one that does not change in the function. The access is then
  `lda (ptr),y` with the counter in Y. `RUST64_NO_NARROW_MORE=1` turns
  the byte lengths off, `RUST64_NO_PTR_SYMS=1` the part behind
  references.
* **Arguments that are the same in every call.** The first three bytes
  of the arguments travel in A, X and Y, the rest in memory. A byte there
  that is the same constant at every call of a function (the upper byte
  of a length when all the texts are short, a flag that is always off)
  is not passed: the function is compiled with the constant in place of
  the byte, and the callers lose the store, often with the load in front
  of it. `c64::println!("score: ", n)` then costs a call with three
  registers and nothing else. A function that is also called through a
  pointer has a copy of its own for that, which takes all its arguments.
  `RUST64_NO_CONST_ARGS=1` turns this off.
* **Multiplication.** Two 16-bit values widened for a 32-bit product
  (`x as i32 * y as i32`) go to a routine for exactly that. The same holds
  for one widened value and a constant that fits 16 bits
  (`f[b] as u32 * 10000`); a constant with up to three bits set is shifts
  and additions in place instead. A product with zero returns at once.
  `RUST64_NO_MUL16K=1` turns the case with the constant off.
* **A `match` as a table.** A `match` on a byte whose arms only give a
  constant byte each,

  ```rust
  match c { b'/' => 2, b'#' => 3, b'.' => 4, b'o' => 5, _ => 0 }
  ```

  is a table with one byte for every value from the smallest case to the
  largest, and `sec` / `sbc #first` / `cmp #count` / `bcs other` / `tax` /
  `lda table,x` in place of a comparison per case. It is taken from four
  cases on where the table is not larger than the comparisons it
  replaces (about 11 bytes per case): the cases of `diagonal`'s tile, 77
  values apart, are a table; four cases spread over all 256 values stay
  comparisons. `RUST64_NO_LOOKUP=1` turns this off.
* **An index plus a constant.** `matrix[960 + c]` with `c` in Y came
  out as `tya` / `adc #$c0` / `tax` / `sta matrix+768,x`. Where the
  addition cannot carry (the optimizer knows the range of `c`), the
  constant goes into the address, `sta matrix+960,y`, and the second
  index register and the temporary that held the value meanwhile are
  free. `RUST64_NO_INDEX_ADD=1` turns this off.
* **Where branches meet.** After `let r = if ok { a } else { b };` or a
  `match` with a value, each way leaves the value in A and in the
  variable. Where the ways meet, the optimizer still knows that both
  hold the same, so the load of the variable that follows goes, and
  with it often the stores. (Not at the head of a loop: there the step
  of a counter would have to bring it to A, an addition in place of an
  `inc`.) `RUST64_NO_JOIN_EQ=1` turns this off.
* **Arrays compared as a whole.** `kind == *b"DIR"` or
  `&s[i..i + 6] == b"SD2IEC"` (up to 128 bytes) is a loop in place:

  ```text
          ldx #0
          ldy #2
  next:   lda kind,y
          cmp text,y
          bne done
          dey
          bpl next
          inx             ; all equal: the result is 1
  done:
  ```

  Longer ones call `rt_memcmp`. `RUST64_NO_EQ_LOOP=1` turns the loop off.
* **Inlining.** rustc inlines small functions before the code generator
  starts; some only show to be small, or to have one caller, in the
  finished program. So at the end the assembly of a function replaces the
  calls of it if the function is called from one place only, or if its
  code is not longer than what its calls save (a call is three bytes, and
  the function itself goes away). The caller is then optimized again with
  the code in it: arguments that were constants stay constants, a result
  stays in its register. Functions whose address is taken (interrupt
  handlers) stay functions. In the listing such a function no longer
  appears under its name; its variables keep their names (`F<n>v<m>`).
  `RUST64_NO_INLINE=1` in the environment turns this off.
* **Frames.** Only the bytes of a frame that the final code still names
  get memory, and functions that are never active at the same time share
  it: a frame lies above the frames of everything the function calls.
  Whatever fits into the free zero page goes there, the leaves first:
  shorter and faster instructions, and pointers there are dereferenced
  directly with `(ptr),y`. After the data-flow pass the frames of small
  programs fit completely. Function pointer targets (interrupt handlers)
  get their own region.

  A frame has two parts, which are placed apart: the values of up to four
  bytes (`F<n>f` in the listing: counters, pointers, lengths) and the
  larger ones (`F<n>g`: structs, arrays, buffers). The small parts of all
  functions come first. So a buffer of 40 bytes in a function at the
  bottom does not push the counters and pointers of its callers out of
  the zero page; the large parts follow and get what is left of it.
* **Statics in the BSS.** Statics that are all zero are not in the file:
  they are cleared at the start.
* **Jumps.** Jumps to jumps and to the next line are removed; `bcc skip /
  jmp target` becomes `bcs target` when the target is certainly in range;
  `jsr f` / `rts` becomes `jmp f`. A test of a result for zero (`match
  open() { 0 => .., e => .. }`, `if let Err(e) = ..`) is `bne` to one side
  and falls into the other; a value that is stored only to be tested is
  tested in A (`ora #$00`) and not stored.
* **Pruning.** Only the runtime and library assembly routines that the
  program refers to are included: the smallest program is 20 bytes.

A loop over a row of the colour matrix, from `examples/drawing`
(`set_cell_colors(c, 24, Color::from_u8(c & 15 | 1), Color::Black)` for
`c` in `0..40`), comes out as

```
        ldy #$00
        jmp F1_b3
_L19:
        and #$0f
        ora #$01
        sta F1v9
        tya
        adc #$c0                ; 24 * 40 + c, the low byte (the carry is clear)
        tax
        lda F1v9
        asl
        asl
        asl
        asl
        sta S12+0+768,x         ; the matrix, + $300
        iny
F1_b3:
        tya                     ; c
        cmp #$28
        bcc _L19
```

What is still missing is in chapter 13. Setting `RUST64_NO_OPT=1` for a
build leaves the data-flow pass out (to compare, or to look for a bug in
it).

### Source layout

| Path | Contents |
|---|---|
| `crates/rustc_codegen_rust64/src/lib.rs` | the `CodegenBackend`: drives the build, writes `.prg`, `.rat`, `.d64` |
| `crates/rustc_codegen_rust64/src/codegen.rs` | MIR to 6510: whole program, frames, statements, calls, intrinsics, data |
| `crates/rustc_codegen_rust64/src/asm.rs` | operands (absolute, indirect) and byte-wise code emission; the passes over the text: jumps, zero-page pointers, pruning |
| `crates/rustc_codegen_rust64/src/opt.rs` | the data-flow pass over the assembly of a function |
| `crates/rustc_codegen_rust64/src/runtime.rat` | start/exit, panic, memcpy/memset/memcmp, mul/div, bit counts |
| `crates/rustc_codegen_rust64/src/runtime.rs` | the call into rat |
| `crates/cargo-c64` | the `cargo c64` command and the end-to-end test harness |
| `target-spec/mos-c64.json` | the target |
| `lib/c64` | the C64 library (Rust and `src/rat/*.rat`) |
| `lib/c64-macros` | `#[c64::main]`, `#[c64::at]`, `#[c64::zp]`, `c64::sid!` |
| `examples/` | example programs |
| `tests/e2e/` | end-to-end test programs |
| `PLAN.md`, `docs/spike.md` | design and findings |

---

## 11. Testing

### End-to-end tests

Every package in `tests/e2e/` is a C64 program that checks itself and
turns the border green (all checks passed) or red (a check failed or it
panicked). The harness builds each one in debug and release with
`cargo c64`, runs it in mimic-headless for 600 frames and looks at the
border colour:

```
cargo test -p cargo-c64 -- --nocapture
```

```
ok   core (debug)
ok   core (release)
ok   gfx (debug)
...
```

| Test | Covers |
|---|---|
| `core` | the language: arithmetic of all widths, comparisons, loops, arrays, slices, structs, enums, `Option`, statics, strings, closures, iterators |
| `lib` | printing, cursor, formatting, memory, `sys::jsr`, assembly functions, hardware registers, input |
| `lib2` | function pointers, `at`/`zp` statics, exported statics, raster interrupts, SID tunes, sprites, sound, PETSCII literals |
| `gfx` | hires and multicolour drawing, cell colours, bitmap mode |
| `gfxu` | the same with the feature `gfx-unclipped`, everything on the screen |
| `g3d` | `c64::g3d`: sine, rotation matrix, corners and faces against the numbers of the Rust version, edges on the bitmap |
| `g3df` | the same with the feature `g3d-fast` |
| `sd2iec` | `c64-sd2iec` and `c64::dos` against mimic's SD2IEC: directories, loading, the status, commands, the directory listing, a sequential file written and read back |

A test program writes the number of the first failing check to the screen,
so the snapshot (`rust64-e2e-<name>-<profile>.png` in the temp directory)
shows what went wrong. The tests need mimic-headless
(`RUST64_MIMIC_HEADLESS` or `../mimic/target/release`) and the ROMs;
without the emulator they are skipped.

A minimal test program:

```rust
#![no_std]
#![no_main]
use c64::hw::{self, Color};
use core::hint::black_box as bb;

static mut FAILS: u8 = 0;

fn check(ok: bool) {
    if !ok {
        unsafe { FAILS += 1 };
    }
}

#[c64::main]
fn main() {
    check(bb(6u8) * 7 == 42);
    check(c64::text::screen_code(b'A') == 1);
    let ok = unsafe { FAILS == 0 };
    hw::vic().border.write(if ok { Color::Green } else { Color::Red });
}
```

`black_box` keeps rustc from computing the checks at compile time.

### Host tests

The `c64` crate's logic (formatting, screen codes, drawing, memory) has
unit tests that run on the PC with the stand-ins:

```
cd lib/c64
cargo test
```

### Debugging

* `cargo c64 asm` prints the generated assembly.
* mimic's debug server (`mimic-headless --debug`) has breakpoints,
  watchpoints, `peek` and tracing; load the labels with rat
  (`rat build x.rat --sym x.sym`) to see function names.
* `RUST64_TRACE=1` prints the MIR of every compiled function.
* Backend errors name the call path from `main` to the function.
* If generated code looks wrong, build again with one of the switches
  below: they turn single steps of the backend off, and the one that
  changes the result names the step. All are environment variables for
  `cargo c64 build`; delete `target/rust64/backend-*.stamp` (or change a
  source file) so that the build runs again.

| Switch | Turns off |
|---|---|
| `RUST64_NO_OPT=1` | the data-flow pass and everything after it (inlining on the assembly, constant arguments, unread stores) |
| `RUST64_NO_INLINE=1` | inlining on the assembly |
| `RUST64_NO_JOIN=1` | variables that share their bytes |
| `RUST64_NO_NARROW=1`, `RUST64_NO_NARROW_MORE=1` | counters that fit a byte; the lengths that are a byte |
| `RUST64_NO_ADDR_SUMS=1`, `RUST64_NO_PTR_SYMS=1` | known addresses through slices and `NonNull`; the same behind references |
| `RUST64_NO_FOLD=1` | addresses the assembler adds |
| `RUST64_NO_CONST_ARGS=1` | arguments that are the same in every call |
| `RUST64_NO_JOIN_EQ=1` | values that stay known where branches meet |
| `RUST64_NO_LOOKUP=1` | a `match` as a table |
| `RUST64_NO_EQ_LOOP=1` | arrays compared in a loop in place |
| `RUST64_NO_MUL16K=1` | the 16-bit routine for a widened value times a constant |
| `RUST64_NO_INDEX_ADD=1` | an index plus a constant in the address |
| `RUST64_NO_ORA=1` | `ora #$00` in place of a load that is only needed for the flags |
| `RUST64_KEEP_STORES=1` | the removal of stores nothing reads at the end |
| `RUST64_NO_ZP_RT=1`, `RUST64_NO_ZP_ARGS=1` | runtime registers and assembly argument areas in the zero page |
| `RUST64_CTX_ALL=1` | (turns on) saving the scratch pointers and `rt_jvec` in every interrupt |
| `RUST64_FRAME_SMALL=<n>` | sets the size up to which a value counts as small for the two parts of a frame (default 4) |

  `RUST64_OPT_TRACE=1` and `RUST64_JOIN_TRACE=1` print what the data-flow
  pass and the sharing of variables do.

---

## 12. Troubleshooting

| Message / symptom | Cause and fix |
|---|---|
| `no entry point` | mark the start function with `#[c64::main]` and use `#![no_main]` |
| `recursion is not supported (static frames): a -> b -> a` | rewrite as a loop |
| `dynamic dispatch is not supported yet` | `dyn Trait`: use generics or an enum |
| `unsizing ... is not supported yet (trait objects)` | a `&dyn` coercion; as above |
| `floating point is not supported yet` | use integers / fixed point |
| `program and its data do not fit below $a000` | the program is too big; build with `--release`, use smaller tables |
| `the program overlaps X at $4000` | a placed static (here `gfx`'s colour matrix) is inside the program: make the program smaller (release, `opt-level = "s"`, in debug `debug-assertions = false`) or place your data higher |
| `static X at $... has data, but the program file cannot load data there` | placed data below `$0801`; use `MaybeUninit::uninit()` |
| `the zero page is full` | fewer or smaller `#[c64::zp]` statics |
| `... takes N bytes of arguments; calls through pointers allow 32` | pass a reference instead of a large value |
| `rat failed on the generated assembly` | an undefined label in your rat code, or a branch too far; the listing is kept |
| red border right away | a panic: index out of bounds, `unwrap` on `None`, overflow in debug |
| the program hangs in a busy loop | do not compare the raster line for equality, use `hw::wait_raster`; debug code is slow |
| a handler makes the program crawl | the handler uses runtime arithmetic (multiplication, division): the dispatcher then saves all runtime registers, and the unoptimized code is slow; keep handlers simple (one that only writes hardware registers has nothing saved for it) |
| `error: could not create LLVM MCSubtargetInfo` | rustc ran without the backend: use `cargo c64`, not plain `cargo build` |
| `backend ... not found` | build `rustc_codegen_rust64` (same profile as `cargo-c64`) or set `RUST64_BACKEND` |

---

## 13. Limitations and roadmap

rust64 follows the phases in `PLAN.md`:

| Phase | Status |
|---|---|
| 0. Spike: target, empty backend, `build-std` | done |
| 1. Core backend: integers, control flow, calls, static frames | done |
| 2. Language breadth: enums, structs, slices, closures, iterators, panics | done |
| 3. Library I: `hw`, `mem`, `sys`, `kernal`, `text`, `fmt`, `input`, assembly, `at`/`zp`, PETSCII | done |
| 4. Library II: `sprite`, `gfx` (with double buffering), `g3d`, `sound`, `irq`, `dos`, function pointers, D64 | done |
| 5. Optimization: frame overlay, zero-page allocation, block layout, data-flow pass (values, ranges, liveness, copies, index registers, loop counters, steps and tests at the end of loops, invariant code out of loops), arguments in registers, inlining of small functions, pruning | done |
| 6. Tooling: `cargo c64 new/test`, Rust source spans for rat errors, memory map, debugger labels, `c64::asm!` | |
| 7. Extensions: recursion on a software stack, `alloc`, `dyn Trait`, `f32`, cartridges | |

Code quality: the passes of chapter 10 make the release builds of the
examples 17% smaller in total than the code generator alone gives
(`RUST64_NO_OPT=1`; `diagonal`: 1823 to 1265 bytes, `sd2iec`: 3175 to
2225, `wic64`: 2389 to 1698). Compared with oscar64 `-Os`, an optimizing
C compiler for the 6502, the fifteen examples that have a C port are
5% smaller in total (eight smaller, seven larger; from 86% smaller to 51%
larger, where the library brings more than the program needs), and
every program that is timed needs fewer cycles (0.5% to 45%); against
`-O2` and `-O3` one of them needs 6 to 9% more. The README has the
numbers.
There is one level of optimization: the backend does the same for every
`--release` build, mostly for size, and does not read `opt-level`
(rustc's own passes before it do).
Still open: an index for an iterator's pointer where the loop uses Y for
something else or the elements are larger than a byte, a counter in a
register also where the loop needs both index registers (a block copy in
its body), loops whose way back is a conditional branch or that hold an
`asm!` block.
Release builds (`opt-level = "s"`) give the best code. Debug builds
include rustc's runtime checks and are much bigger and slower.

Other open points:

* No `dyn Trait`, floats, heap, recursion, `core::fmt` with arguments.
* Recursion through function pointers is not detected.
* A pointer copy that the main program calls (through a function pointer)
  while an interrupt runs the same copy shares its frame.
* `MulWithOverflow` does not report overflow.
* `#[track_caller]` locations are dummies (panics have no message anyway).
