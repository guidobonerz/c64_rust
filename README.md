# rust64 — Rust for the Commodore 64

rust64 compiles Rust programs for the Commodore 64. You write real Rust
(`no_std`, traits, generics, closures, iterators, `Option`, `match`) with the
usual tools (rust-analyzer, clippy, `cargo test` for the logic), and get a
`.prg` and a `.d64` that the C64 loads and runs.

```rust
#![no_std]
#![no_main]

use c64::hw::{self, Color};

#[c64::main]
fn main() {
    c64::println!("hello from rust64");
    hw::vic().border.write(Color::Green);   // lda #5 / sta $d020
}
```

```
$ cargo c64 build --release
    wrote .../target/mos-c64/release/hello.prg        (536 bytes)
$ cargo c64 run                                       build and start the emulator
$ cargo c64 asm                                       print the generated assembly
```

**Contents:** [What it is](#what-it-is) · [How it works](#how-it-works) ·
[Getting started](#getting-started) · [The language](#the-language-on-the-c64) ·
[Libraries](#libraries) · [Examples](#examples) ·
[Test results](#test-results) · [Repository layout](#repository-layout) ·
[Status](#status-and-roadmap)

The full manual is [docs/rust64-guide.md](docs/rust64-guide.md) (also as
[PDF](docs/rust64-guide.pdf)); the design and its history are in
[PLAN.md](PLAN.md) and [docs/spike.md](docs/spike.md).

---

## What it is

Three things in one repository:

1. **A code generator for rustc** (`crates/rustc_codegen_rust64`). It is not a
   new compiler for a Rust-like language and not a fork of LLVM: it is a
   codegen backend that plugs into the stock nightly rustc
   (`-Zcodegen-backend`). rustc parses, type-checks, borrow-checks,
   monomorphizes and optimizes the MIR; rust64 translates that MIR into 6510
   assembly. So the language is Rust itself, including `core`.
2. **A cargo subcommand** (`crates/cargo-c64`): `cargo c64 build | run | asm`
   sets up the target, the flags and the pinned toolchain.
3. **The libraries** (`lib/`): the `c64` crate and four extension crates, as
   safe Rust APIs with the time-critical parts in assembly. See
   [Libraries](#libraries).

It belongs to a small family of tools that are expected side by side:
[rat](../rat) is the assembler (linked into the backend as a library),
[mimic](../mimic) is the emulator that `cargo c64 run` starts and the tests
run in. rust64 follows the same model as its sibling for Go, go64.

## How it works

```
 .rs ─► rustc (pinned nightly, target mos-c64.json: 16-bit pointers, os = c64)
        │  parse, types, borrow check, monomorphization, MIR optimizations
        ▼
  rustc_codegen_rust64   (-Zcodegen-backend)
        │  library crates: only metadata with MIR (-Zalways-encode-mir)
        │  the program: walk the MIR from `main` through all crates
        │  static frames, recursion check, zero-page allocation, peephole
        ▼
  rat assembly ─► rat-asm (linked in) ─► .prg + .d64 ─► mimic / a real C64
```

* **Whole-program compilation.** Library crates (`core`, `c64`, your own)
  produce no code, only MIR. When the program itself is compiled, the backend
  starts at `main` and translates every function it reaches, in any crate.
  Unused library code costs nothing; the smallest program is 20 bytes.
* **Static frames.** The 6510 has a 256-byte stack and no usable stack
  addressing, so every function gets its parameters and locals at fixed
  addresses. Functions that are never active at the same time share memory,
  and what fits goes into the zero page. The price: no recursion (the compiler
  reports the cycle).
* **The target** is a JSON specification based on `msp430` (16-bit little
  endian; rustc has no 6502 architecture and LLVM is never called) with
  `target_os = "c64"`, 16-bit `usize`, alignment 1 and abort-on-panic (red
  border, back to BASIC).
* **Assembly where it pays.** `c64::rat_source!` includes rat assembly files
  and `c64::asm!` inlines 6510 code; the library uses both for memory fills,
  drawing, interrupt entry and KERNAL calls.
* **The same code on the PC.** On the host the `c64` crate is a normal `std`
  crate with stand-ins (a simulated 64 KB memory, printing to stdout), so
  `cargo test`, rust-analyzer and clippy work as usual.

Chapter 10 of the guide shows what the generated code looks like and which
optimizations the backend does.

## Getting started

| Tool | Purpose | Where rust64 looks |
|---|---|---|
| rustup | installs the pinned nightly (`nightly-2026-11-10` with `rustc-dev`, `rust-src`, `llvm-tools`) | `rust-toolchain.toml` |
| rat (source checkout) | the assembler, linked into the backend | `../rat` |
| mimic (optional) | runs programs (`cargo c64 run`) | `RUST64_MIMIC`, else `mimic` on the `PATH` |
| mimic-headless (optional) | end-to-end tests, benchmarks | `RUST64_MIMIC_HEADLESS`, else `../mimic/target/release` |
| C64 ROMs (for mimic) | KERNAL, BASIC, character ROM | `EMU_ROM_DIR`, else `~/.mimic/roms` |

```
git/
  c64_rust/      this repository
  rat/           the assembler
  mimic/         the emulator (optional)
```

Build the backend and the cargo subcommand, and put them on the `PATH`:

```
cd c64_rust
cargo build --release -p rustc_codegen_rust64 -p cargo-c64
set PATH=%PATH%;...\c64_rust\target\release           (Windows)
export PATH=$PATH:.../c64_rust/target/release          (Linux, macOS)
```

A program is a normal cargo package that depends on the `c64` crate:

```toml
[package]
name = "mygame"
version = "0.1.0"
edition = "2024"

[dependencies]
c64 = { path = "../c64_rust/lib/c64" }

[profile.dev]
opt-level = 1          # lets rustc inline the small helpers of iterators

[profile.release]
opt-level = "s"        # the smallest and fastest programs
```

```
cargo c64 build [--release] [-o FILE.prg|FILE.d64] [cargo args...]
cargo c64 run   [--release]                           build and start mimic
cargo c64 asm   [--release]                           build and print the assembly
```

Use `cargo c64`, not plain `cargo build`: without the backend rustc would
look for an LLVM target that does not exist.

## The language on the C64

**Works:** all of `core` that needs neither floats nor the heap (`Option`,
`Result`, `?`, slices, `&str`, ranges, arrays, `core::mem`, `core::ptr`);
structs, enums with data, tuples, `match` with every pattern; generics,
traits, const generics, `impl Trait`; closures and every iterator adapter;
`static`, `const`, `const fn` (tables cost no code); function pointers;
`Drop` (a `dos::File` closes itself); integers of every width up to `u128`
with `wrapping_*`, `checked_*`, `saturating_*`; panics for bounds checks,
`unwrap` and (in debug builds) overflow.

**Does not work (yet):**

| Feature | Use instead |
|---|---|
| recursion | loops |
| `dyn Trait` | generics, enums, `fn` pointers |
| `f32`, `f64` | fixed point (`i16` as 8.8) |
| `alloc` (`Vec`, `String`, `Box`) | arrays, `c64::fmt::Buf` |
| `core::fmt` with arguments | `c64::print!`, `c64::println!`, `c64::write!` |
| `asm!` with register operands | `c64::asm!` (operands are memory locations) |

`u8`, `i8` and `bool` are native and should be preferred; `u16`, `usize` and
pointers are two bytes; `u32` multiplication and division go through the
runtime. String literals are ASCII in the source and converted to PETSCII
when printed; `c64::p!("HELLO")` (PETSCII) and `c64::s!("HELLO")` (screen
codes) convert at compile time.

## Libraries

Everything a rust64 program can use, besides `core`. What a program does not
call is not in the `.prg`, so depending on a crate costs nothing by itself.
The guide (chapter 8) lists every function.

### The `c64` crate

Every program depends on it: `c64 = { path = ".../lib/c64" }`.

| Module | For | Contents |
|---|---|---|
| `c64::hw` | hardware registers | VIC-II, SID and CIA as typed structs, `Color`, raster waits, VIC bank and screen setup |
| `c64::text` | the character screen | print, cursor, colours, `put_char`/`put_str`/`put_codes`, fills, scrolling, character sets |
| `c64::fmt` | formatting | `print!`/`println!`/`write!` without `core::fmt`: decimal, `Hex`, `Bin`, a fixed `Buf` |
| `c64::ui` | boxes on the text screen | `frame`, `panel`, `titled_frame`, `titled_panel`; square or round corners |
| `c64::sprite` | hardware sprites | images, position (9-bit X), colours, multicolour, expansion, collisions |
| `c64::g3d` | bodies in three dimensions | corners, faces and edges on the hires bitmap: rotation matrix, hidden edges, parallel projection or a vanishing point; a cube and a pyramid |
| `c64::gfx` | bitmap graphics | hires and multicolour: `plot`, `line`, `fline`, `hline`, `vline`, `rect`, `fill_rect`, `circle`, cell colours, Koala pictures, double buffering |
| `c64::sound` | the SID | voices, ADSR, notes, filter; playing PSID tunes (`c64::sid!`) |
| `c64::irq` | raster interrupts | Rust functions as handlers, up to eight per frame |
| `c64::input` | joystick and keyboard | joysticks, keyboard buffer, key matrix |
| `c64::dos` | disk drives | load, save, commands, sequential files, the directory as an iterator |
| `c64::kernal` | KERNAL ROM | the KERNAL calls |
| `c64::mem` | raw memory | `peek`/`poke`, `copy`, `fill`, memory as a slice |
| `c64::sys` | program control | `exit`, `panic`, `sei`/`cli`, `jsr` |

| Macro or attribute | For |
|---|---|
| `#[c64::main]` | the program's entry point |
| `c64::print!`, `c64::println!`, `c64::write!` | printing and formatting |
| `c64::p!("...")`, `c64::pm!("...")` | PETSCII string literals, converted at compile time |
| `c64::s!("...")`, `c64::sm!("...")` | screen code literals for `text::put_codes`, converted at compile time |
| `#[c64::at(0x2000)]` | a static at a fixed address |
| `#[c64::zp]` | a static in the zero page |
| `c64::sid!` | a PSID tune in the program |
| `c64::rat_source!(NAME, "file.rat")` | a rat assembly file in the program |
| `c64::asm!(...)` | inline 6510 assembly |

| Cargo feature | Effect |
|---|---|
| `gfx-unclipped` | `c64::gfx` draws without checking the coordinates against the screen: smaller and faster, but every pixel must be on the screen |
| `g3d-fast` | `c64::g3d` draws flat lines with a piece of code for each bit of a byte: faster, about 200 bytes more |

### Extension crates

For hardware that not every C64 has. A program names the ones it needs next
to `c64`:

```toml
[dependencies]
c64     = { path = "../c64_rust/lib/c64" }
c64-uci = { path = "../c64_rust/lib/c64-uci" }
```

| Crate | Hardware | Contents |
|---|---|---|
| `c64-sd2iec` | SD2IEC | detection, directories, disk images (D64, D71, D81), settings |
| `c64-uci` | 1541 Ultimate-II(+), Ultimate 64 command interface | files on its storage, TCP/UDP sockets, control |
| `c64-wic64` | WiC64 WiFi module | HTTP GET/POST, one TCP connection |
| `c64-uaudio` | Ultimate Audio sampler | seven voices playing samples from REU memory |

`lib/c64-macros` holds the proc macros behind `#[c64::main]`, `at`, `zp` and
`sid!`; programs use them through `c64` and do not depend on it directly.

## Examples

`examples/` (release builds, bytes of the `.prg`):

| Example | Shows | Main modules | Bytes |
|---|---|---|---:|
| `border` | the smallest program, without the library | | 20 |
| `hello` | printing, a loop, the border colour | `hw`, `fmt` | 362 |
| `joystick` | reading the joystick | `input` | 761 |
| `ui` | boxes on the text screen | `ui`, `text` | 1050 |
| `music` | notes and a SID tune | `sound` | 844 |
| `sprites` | a moving sprite | `sprite` | 1043 |
| `rasterbars` | a stable raster interrupt in assembly, colours from Rust | `rat_source!`, `mem` | 1204 |
| `scroller` | a horizontal soft scroll with two raster interrupts | `irq`, `mem` | 1247 |
| `drawing` | lines, a rectangle and circles on the hires bitmap | `gfx` | 1963 |
| `cube3d` | a turning cube without its hidden edges, with double buffering | `g3d`, `gfx` | 3694 |
| `perspective` | a turning bar with a vanishing point; fire shows it without | `g3d`, `gfx`, `input` | 4020 |
| `diagonal` | the whole screen scrolling diagonally, custom character set | `hw`, `mem` | 1265 |
| `mandelbrot` | the Mandelbrot set in fixed point: PETSCII, or the multicolour bitmap with `--features multicolor` | `text`, `gfx` | 819 |
| `julia` | a Julia set, in the same two ways | `text`, `gfx` | 827 |
| `pi` | 100 digits of pi with 32-bit arithmetic | `kernal` | 1380 |
| `dir` | the disk directory | `dos` | 1213 |
| `wic64` | the WiFi module | `c64-wic64` | 1698 |
| `uci` | the Ultimate command interface | `c64-uci` | 1954 |
| `sd2iec` | directories and disk images | `c64-sd2iec` | 2225 |

```
cd examples/drawing
cargo c64 run --release
```

---

## Test results

Two kinds of results: the end-to-end tests, which say whether the compiled
programs are correct, and the comparison with oscar64, which says how small
and fast they are.

### End-to-end tests

```
cargo test -p cargo-c64 -- --nocapture       end-to-end, needs mimic-headless
cd lib/c64 && cargo test                     the library's logic on the host
```

Every package in `tests/e2e/` is a C64 program that checks itself and turns
the border green or red. The harness builds each one in debug and release,
runs it in mimic-headless for 600 frames and looks at the border colour.
All ten pass in both builds (20 runs):

| Test | Covers | debug | release |
|---|---|:-:|:-:|
| `core` | the core language | ok | ok |
| `lib` | the `c64` library | ok | ok |
| `lib2` | function pointers, `at`/`zp` statics, raster interrupts, SID tunes, sprites, sound, PETSCII literals | ok | ok |
| `gfx` | `c64::gfx`, hires and multicolour | ok | ok |
| `gfxu` | `c64::gfx` with `gfx-unclipped` | ok | ok |
| `g3d` | `c64::g3d`: matrix, corners, faces, edges | ok | ok |
| `g3df` | `c64::g3d` with `g3d-fast` | ok | ok |
| `ui` | `c64::ui` | ok | ok |
| `sd2iec` | `c64-sd2iec` and `c64::dos` against mimic's SD2IEC (directory, sequential files) | ok | ok |
| `uci` | `c64-uci` against mimic's command interface (no network) | ok | ok |
| `wic64` | `c64-wic64` against mimic's WiC64 (no network) | ok | ok |
| `uaudio` | `c64-uaudio`, as far as possible without the hardware | ok | ok |

mimic does not emulate the Ultimate Audio sampler: `c64-uaudio` follows the
Ultimate's FPGA source and has not been run against hardware.

### Size and speed: comparison with oscar64

[oscar64](https://github.com/drmortalwombat/oscar64) is an optimizing C/C++
compiler for the 6502 with a mature C64 library, and the usual yardstick for
compiled code on this machine. All examples except `wic64` and `uci` were
ported to C for
oscar64 1.32.266 (`bench/<name>/oscar64/`), and both versions were checked
against each other on the emulated screen.

#### Summary

rust64 `--release` against oscar64 `-Os`; less is better in every column.
rust64 has one level of optimization (the backend does the same for every
`--release` build); oscar64's `-O2` and `-O3` are in the tables further down.

| Program | Work is done by | `.prg` bytes rust64 | oscar64 | rust64 is | Cycles rust64 | oscar64 | rust64 needs |
|---|---|---:|---:|---|---:|---:|---|
| drawing, clipped | library assembly | **1963** | 3147 | 38% smaller | **764 634** | 968 876 | 21% fewer |
| drawing, unclipped | library assembly | **1677** | 2046 | 18% smaller | **744 614** | 884 436 | 16% fewer |
| scroller | interrupts, one block copy | **1247** | 1355 | 8% smaller | **210** | 385 | 45% fewer |
| rasterbars | small fills and copies | **1204** | 1096 | 10% larger | **1837** | 1877 | 2% fewer |
| diagonal | compiled code, block copies | **1265** | 1126 | 12% larger | **2356** | 2485 | 5% fewer |
| mandelbrot, PETSCII | compiled code, 32-bit products | **819** | 938 | 13% smaller | **17 889 254** | 18 817 639 | 5% fewer |
| mandelbrot, multicolour | the same, and the libraries' pixels | **1106** | 1269 | 13% smaller | **587 782 706** | 627 249 821 | 6% fewer |
| julia, PETSCII | compiled code, 32-bit products | **827** | 949 | 13% smaller | **23 722 172** | 24 981 058 | 5% fewer |
| julia, multicolour | the same, and the libraries' pixels | **1114** | 1277 | 13% smaller | **769 635 852** | 813 110 831 | 5% fewer |
| pi | compiled code, 32-bit multiplication and division | **1380** | 1250 | 10% larger | **14 480 741** | 14 553 959 | 0.5% fewer |

Cycles: the whole program for drawing, the whole computation for mandelbrot,
julia and pi, the average frame for the others.

**The result in one sentence:** where the work is done by the library's
assembly, rust64 is ahead of oscar64 in size and speed; where it is done by
compiled code, rasterbars and diagonal are 5–12% larger than oscar64's and
the scroller is 8% smaller, and rust64 needs fewer cycles per frame than
`-Os` in all three (45%, 5% and 2% for scroller, diagonal and rasterbars);
in arithmetic rust64 needs 0.5–6% fewer cycles, with the smaller programs
for the two sets.

#### What was compared

* **drawing:** 20 lines across the hires screen, a rectangle, six circles and
  one row of cell colours. The C port uses the same memory layout (colour
  matrix at `$4000`, bitmap at `$6000`) and oscar64's own library: `bm_fill`,
  `memset`, `vic_setmode`, `bm_line`/`bmu_line`. oscar64 has no circle
  outline, so the circle is the same midpoint algorithm as in `c64::gfx`, in
  C, on `bm_set`. Both programs draw the same picture (6045 and 6047 pixels
  set; the two line routines round differently).
* **Clipped and unclipped** (drawing only). *Clipped* drawing leaves out what
  is outside the screen; *unclipped* drawing does not check and overwrites
  other memory when a pixel is outside. rust64 clips by default and drops the
  checks for the whole program with the feature `gfx-unclipped`. oscar64 lets
  you choose per call: `bm_line` with a clip rectangle and a bounds check per
  circle pixel, or `bmu_line` and no checks. Nothing in this picture leaves
  the screen, so all four builds draw it correctly.
* **rasterbars:** the stable raster interrupt is the same 6510 code on both
  sides (`raster.rat` in Rust, inline assembly in C), so only the main
  program differs: once per frame it clears a 96-byte colour table and
  copies three 11-byte bars into it (`mem::fill`/`mem::copy` against
  `memset`/`memcpy`).
* **scroller:** one text row scrolls with XSCROLL, switched on and off by two
  raster interrupts. rust64 uses `c64::irq` with two Rust functions as
  handlers; the C port uses oscar64's `rasterirq` library, where a handler
  is a few bytes of generated code that store one value. The message is
  screen codes in the program on both sides, converted at compile time
  (`c64::s!("...")`, `s"..."`).
* **diagonal:** the whole screen scrolls diagonally over a tile pattern, with
  a custom character set and two screens. All of it is compiled code plus
  block copies.

**Size** is the `.prg` file of the normal program. **Speed** comes from a
second build of each program that measures with CIA 2's timers (timer A
counts cycles, timer B its underflows) and prints the counts. The programs
run in mimic's emulated PAL C64 with the display and the KERNAL interrupt on,
so the numbers are real time on the machine, including the cycles the VIC
takes. One frame is 19 656 cycles, one second 985 248.

* drawing: every phase is bracketed by the timers.
* The other three run 256 frames and measure each frame's work in the main
  loop (from the end of the wait for the frame to the end of the loop body);
  the tables give the average and the worst frame.
* diagonal also: the cycles of the start-up (characters, tile rows, colours,
  first screen).
* scroller also: what the interrupts cost per frame, measured as the extra
  time a fixed loop of 986 150 cycles takes with them (screen off, so that
  the VIC takes no cycles; this includes the KERNAL's keyboard scan, which
  runs 50 times a second in the rust64 version and 60 times in the C
  version).

#### All numbers

**drawing** — size, and CPU cycles per phase. Each oscar64 build is compared
with the rust64 build of the same kind. *setup* clears the bitmap, fills the
colour matrix and switches the mode; *cells* is the loop over
`set_cell_colors`.

| Build | `.prg` bytes | rust64 is | setup | lines | rect | circles | cells | **total** | seconds | rust64 needs |
|---|---:|---|---:|---:|---:|---:|---:|---:|---:|---|
| **rust64** `--release`, clipped | **1963** | | 108 063 | 366 555 | 14 162 | 274 712 | 1 142 | **764 634** | 0.78 | |
| oscar64 `-O1`, clipped | 4153 | 53% smaller | 103 501 | 388 147 | 31 470 | 461 001 | 1 188 | 985 307 | 1.00 | 22.4% fewer cycles |
| oscar64 `-Os`, clipped | 3147 | 38% smaller | 108 050 | 371 991 | 28 068 | 459 661 | 1 106 | 968 876 | 0.98 | 21.1% fewer |
| oscar64 `-O2`, clipped | 3202 | 39% smaller | 103 117 | 378 440 | 29 725 | 458 098 | 1 063 | 970 443 | 0.98 | 21.2% fewer |
| oscar64 `-O3`, clipped | 3658 | 46% smaller | 103 301 | 371 179 | 27 457 | 415 822 | 1 132 | 918 891 | 0.93 | 16.8% fewer |
| **rust64** `--release`, unclipped | **1677** | | 108 063 | 363 369 | 14 273 | 257 724 | 1 185 | **744 614** | 0.76 | |
| oscar64 `-O1`, unclipped | 2724 | 38% smaller | 103 454 | 370 254 | 27 687 | 390 819 | 1 102 | 893 316 | 0.91 | 16.6% fewer |
| oscar64 `-Os`, unclipped | 2046 | 18% smaller | 108 035 | 363 284 | 27 012 | 384 917 | 1 188 | 884 436 | 0.90 | 15.8% fewer |
| oscar64 `-O2`, unclipped | 2004 | 16% smaller | 103 306 | 362 185 | 27 085 | 384 992 | 1 192 | 878 760 | 0.89 | 15.3% fewer |
| oscar64 `-O3`, unclipped | 2297 | 27% smaller | 103 006 | 362 083 | 26 993 | 342 487 | 1 044 | 835 613 | 0.85 | 10.9% fewer |

(The rust64 dev profile, with overflow and UB checks, gives 2729 bytes.)

**rasterbars, scroller, diagonal** — size, and CPU cycles per frame.

| Program | Build | `.prg` bytes | average frame | worst frame | other |
|---|---|---:|---:|---:|---|
| rasterbars | **rust64** `--release` | **1204** | **1837** | | |
| | oscar64 `-Os` | 1096 | 1877 | | |
| | oscar64 `-O2` | 1136 | 1737 | | |
| | oscar64 `-O3` | 1151 | 1684 | | |
| scroller | **rust64** `--release` | **1247** | **210** | **765** | interrupts per frame: **531** |
| | oscar64 `-Os` | 1355 | 385 | 1695 | 826 |
| | oscar64 `-O2` | 1498 | 369 | 1621 | 824 |
| | oscar64 `-O3` | 1498 | 385 | 1694 | 817 |
| diagonal | **rust64** `--release` | **1265** | **2356** | **5352** | start-up: **65 556** |
| | oscar64 `-Os` | 1126 | 2485 | 5634 | 69 490 |
| | oscar64 `-O2` | 1161 | 2484 | 5630 | 67 915 |
| | oscar64 `-O3` | 1161 | 2398 | 5464 | 68 075 |

#### Reading the numbers

Where the library's assembly does the work (drawing, and the scroller's block copy and interrupts):

* **Clipped, rust64 is ahead.** Against oscar64's `-Os` and `-O2` it is
  21% faster and 38–39% smaller; against `-O3` it is 17% faster and 46%
  smaller.
* **Unclipped, rust64 is ahead too.** Against `-Os` and `-O2` it is 15–16%
  faster and 16–18% smaller; against `-O3` it is 11% faster and 27%
  smaller.
* **What dropping the checks buys** is different on the two sides. oscar64
  saves 1100–1400 bytes and about 9% of the time. rust64 saves 286 bytes
  and 2.6%: its line loop never checked each pixel (only the two ends), so
  the gain is in the circles (6%) and in the code for lines that leave the
  screen, which is no longer in the program.
* **Lines: level.** rust64 needs 1% fewer cycles than `-Os` and `-O3` when
  both clip, and as many as oscar64 (0.0–0.4% more) when neither does.
  oscar64's `bmu_line` assembles a loop specialized for each line
  (direction, slope, pattern) at run time and then runs that. rust64's
  `line` checks the two ends against the screen and then runs one of two
  fixed Bresenham loops in assembly, written for the mode (hires and
  multicolour have their own routines): 47 cycles a pixel for a flat line
  and 50 for a steep one in hires. It modifies no code, so that interrupt
  handlers may draw too.
* **Rectangle: rust64 is twice as fast**, because horizontal edges are stored
  a byte at a time, while the C version draws four general lines.
* **Circles: rust64 needs 34–40% fewer cycles than C when both clip, and
  25–33% fewer when neither does** — hand-written assembly against compiled C
  calling `bm_set` for every pixel.
* **Setup** is two memory fills on both sides; rust64 is level with `-Os`
  and up to 5% slower than the other levels.
* **scroller: rust64's main loop is faster** (43–45% fewer cycles on average,
  53–55% fewer in the worst frame, the one that moves the 39 bytes of the
  row: a copy loop in place in rust64, two bytes per round, `memmove` in C).
* **scroller interrupts: rust64 needs 35% fewer cycles.** 531 per frame
  against about 820; much of both is the KERNAL's keyboard scan, which the
  rust64 version runs once per frame. A Rust function as a handler goes
  through a dispatcher. It saves the scratch pointers, the runtime's
  registers and the vector of indirect calls only if the handlers or the
  program use them; these handlers write two registers of the VIC and
  need none of that.

Where compiled code does the work:

* **Size: oscar64 wins rasterbars and diagonal.** rust64's are 5–10%
  larger for rasterbars and 9–12% larger for diagonal; the scroller is 8%
  smaller than `-Os` and 17% smaller than `-O2` and `-O3`.
* **diagonal: 2–5% less time per frame, and a start-up that is 3–6%
  shorter.** The per-frame work is mostly block copies of 40 bytes: a
  loop in place on both sides, and `mem::copy` moves two bytes per round.
  The start-up is compiled code: in Rust nested loops over arrays with
  `iter_mut().enumerate()`, and for every cell a `match` that turns a
  character of the tile into a character number. That `match` is a table
  lookup in rust64 (it was a chain of eleven comparisons, and the
  start-up took 1.2 times as long as the C port's). The loop still walks
  a 16-bit pointer where the C port counts with `char` and indexes.
* **rasterbars: rust64 needs 2% fewer cycles per frame than `-Os` and
  6–9% more than `-O2` and `-O3`** for one fill and three small copies.
  Both sides expand them in place; rust64 goes through a pointer
  (`sta (ptr),y`, two bytes per round: 9.5 cycles per byte for the fill,
  15.5 for a copy), oscar64 knows the address
  of the table (`sta table,x`). Part of rust64's size is not code:
  `raster.rat` aligns its timed code to a page (112 bytes of padding in
  front of it) and keeps the colour table in the program file, while the C
  version has the table outside the file.
* **The *cells* loop of drawing** is the only phase of that program that is
  purely compiler output on both sides, and there both need 26–30 cycles
  per iteration (rust64 26). Both keep the counter in an index register,
  store with it (`sta matrix+$3c0,y`) and test at the end of the loop.
  rust64 had the row's offset added to the counter in the other index
  register, and the colour in a temporary meanwhile: 40 cycles.

What rust64 does about compiled code is in the guide (chapter 10: a
data-flow pass over the assembly of each function). The open optimizations
(an index for an iterator where Y is in use too, a counter in a register
where a block copy needs both) are what these programs still need.

#### Arithmetic: the Mandelbrot set, a Julia set, pi

Three programs whose time goes into arithmetic that the 6502 does not have:
`mandelbrot` and `julia` compute with 16-bit fixed-point numbers and 32-bit
products (three multiplications per round), once for the 40 x 25 cells of
the text screen and once for the 160 x 200 pixels of the multicolour
bitmap; `pi` is the spigot algorithm for 100 digits, with a 32-bit
multiplication and a division with remainder in its inner loop. The
sources are the examples themselves (`--features timed`) and
`bench/math/oscar64/*.c`. Every row of a program has the same checksum (the
sum of all rounds, or of the groups of digits), and pi prints the right
digits on both sides.

| Program | Build | `.prg` bytes | cycles | rust64 needs |
|---|---|---:|---:|---|
| mandelbrot, PETSCII | **rust64** `--release` | **819** | **17 889 254** | |
| | oscar64 `-Os` | 938 | 18 817 639 | 4.9% fewer |
| | oscar64 `-O2` | 940 | 18 448 282 | 3.0% fewer |
| | oscar64 `-O3` | 940 | 18 499 742 | 3.3% fewer |
| mandelbrot, multicolour | **rust64** `--release` | **1106** | **587 782 706** | |
| | oscar64 `-Os` | 1269 | 627 249 821 | 6.3% fewer |
| | oscar64 `-O2` | 1273 | 612 946 565 | 4.1% fewer |
| | oscar64 `-O3` | 1201 | 614 949 025 | 4.4% fewer |
| julia, PETSCII | **rust64** `--release` | **827** | **23 722 172** | |
| | oscar64 `-Os` | 949 | 24 981 058 | 5.0% fewer |
| | oscar64 `-O2` | 951 | 24 475 617 | 3.1% fewer |
| | oscar64 `-O3` | 951 | 24 722 277 | 4.0% fewer |
| julia, multicolour | **rust64** `--release` | **1114** | **769 635 852** | |
| | oscar64 `-Os` | 1277 | 813 110 831 | 5.3% fewer |
| | oscar64 `-O2` | 1281 | 797 501 735 | 3.5% fewer |
| | oscar64 `-O3` | 1209 | 803 192 014 | 4.2% fewer |
| pi | **rust64** `--release` | **1380** | **14 480 741** | |
| | oscar64 `-Os` | 1250 | 14 553 959 | 0.5% fewer |
| | oscar64 `-O2` | 1239 | 14 453 155 | 0.2% more |
| | oscar64 `-O3` | 1239 | 14 476 057 | level |

* **rust64 needs 3–6% fewer cycles for both sets, and pi is level
  (0.5% fewer than `-Os`, 0.2% more than `-O2`). In size rust64's sets
  are 8–13% smaller and pi is 10–11% larger.** Nearly all of the time is spent in
  the runtime's multiplication and division on both sides, so this
  compares those routines and the code around the calls more than the
  compilers' loops.
* **These programs are why rust64 has the routines it has now.** With the
  general routines for any width, which were all it had for 32 bits, pi
  took 92 million cycles and the PETSCII Mandelbrot set 86 million: six
  and five times oscar64's. Now two 16-bit values that are multiplied for
  a 32-bit product go to a routine for exactly that; 32-bit
  multiplication and unsigned division have routines of their own (a
  division skips the leading zero bytes and works with a 16-bit remainder
  for a small divisor); `d % g` and `d / g` next to each other are one
  division; and a shift by 8 bits or more moves bytes first. A 16-bit
  value times a constant of 16 bits (`f[b] as u32 * 10000` in pi) goes
  to the 16-bit routine too, and a product with zero (the first round
  of every point of the Mandelbrot set) is done at once.
* **The registers of these routines are in the zero page**, like
  oscar64's, where the program leaves room for them (all of these do).
  With them in the BSS, as they were at first, rust64 needed 7–20% more
  cycles than oscar64 in every row: the routines name them in nearly
  every instruction.
* **What is left** is per call: rust64 copies the operands to the
  runtime's registers and the result back; the fixed-point shift after
  each product is three more passes over the bytes.

#### All examples: size

The other examples have C ports too (`bench/examples/oscar64/`), each
checked against the Rust program on the emulated screen: the pictures are
the same pixel for pixel, except for the sprites' colour band, which
starts a few pixels apart. They wait for the joystick or play a tune, so
there is nothing to time: this is the size of every example, in the order
of the table above (`pwsh bench/examples.ps1`).

| Example | rust64 `--release` | oscar64 `-Os` | `-O2` | `-O3` | rust64 against `-Os` |
|---|---:|---:|---:|---:|---|
| border | **20** | 142 | 142 | 142 | 86% smaller |
| hello | **362** | 553 | 538 | 557 | 35% smaller |
| joystick | **761** | 732 | 698 | 709 | 4% larger |
| ui | **1050** | 954 | 948 | 1488 | 10% larger |
| music | **844** | 559 | 617 | 617 | 51% larger |
| sprites | **1043** | 1132 | 1123 | 1088 | 8% smaller |
| rasterbars | **1204** | 1096 | 1136 | 1151 | 10% larger |
| scroller | **1247** | 1355 | 1498 | 1498 | 8% smaller |
| drawing | **1963** | 3147 | 3202 | 3658 | 38% smaller |
| diagonal | **1265** | 1126 | 1161 | 1161 | 12% larger |
| mandelbrot | **819** | 938 | 940 | 940 | 13% smaller |
| julia | **827** | 949 | 951 | 951 | 13% smaller |
| pi | **1380** | 1250 | 1239 | 1239 | 10% larger |
| dir | **1213** | 1320 | 1251 | 2673 | 8% smaller |
| sd2iec | **2225** | 1886 | 1867 | 3782 | 18% larger |
| **total** | **16 223** | 17 139 | 17 311 | 21 654 | 5% smaller |

* **rust64 is smaller in eight of the fifteen and larger in seven; over
  all of them it is 5% smaller than `-Os`, without `sd2iec` 8% smaller.**
* **The small programs go to rust64 because of the start code:** oscar64's
  smallest program is 142 bytes. Its text goes through `puts` and `utoa`
  here; with `printf` hello is 4211 bytes.
* **music is 51% larger.** Both programs have the frequencies of their
  notes as constants (`Note::C4.freq()` is computed when the program is
  compiled, like `NOTE_C(4)` in C), so neither carries a table of all
  notes. What is left is `c64::sound`: routines for any voice, with
  copies of the control registers, where the C program stores to the
  registers of the two voices directly.
* **joystick, ui and sprites** write text to the screen, as screen codes
  that are in the file on both sides (`c64::s!("...")`, `s"..."`). sprites
  is 8% smaller. joystick also prints through the KERNAL, which takes the
  conversion to PETSCII at run time, and the titles of ui's boxes are
  converted by the box routine.
* **sd2iec is where compiled Rust is weakest; dir is 8% smaller.** The
  same directory is read line by line on both sides. The library's part
  is assembly (`c64::dos`: one routine parses a line of the listing
  into an entry, with one change of the channel per line); what the
  program does with the entries goes through `Result`, `Option` and an
  iterator in Rust, through `int` and a pointer to one entry in C. The
  first ports were 2323 and 4721 bytes; what was done since is in the
  guide (views of structs, results that stay where the call left them,
  matches on enums, checks that were made, arguments that are not read,
  frames in two parts, counters that fit a byte, variables that share
  their bytes, arrays compared in a loop in place, pointers whose address
  is known through slices, arguments that are the same in every call,
  values that stay known where branches meet). What is left: string
  slices that are copied out of memory before they are used, and results
  of functions that are needed after the next call.

#### Caveats

* One emulator, one version of each tool. The cycle counts are deterministic
  in mimic and repeat exactly; on real hardware the phase of the timer
  interrupt moves them by a fraction of a percent.
* The installed oscar64 build is not fully reproducible: across invocations
  with the same source and flags, the size of an optimized `.prg` varied by
  up to 56 bytes (for example 3202–3258 for drawing `-O2` clipped) while the
  cycle counts stayed the same. The tables show the values of the last
  complete run.
* The same oscar64 build crashes at `-O2` and above when the clip rectangle
  is `const`; the drawing port therefore uses a non-`const` one.
* File size is not the whole memory footprint: at run time oscar64 adds a
  224-byte buffer for the generated line code and a software stack, rust64
  its static frames. Neither is included.
* None of the four is a benchmark without library assembly (a sieve, a sort,
  a Bresenham in plain Rust and C), which would measure the code generators
  properly; these measure what a user of each toolchain gets for such a
  program.

#### Reproducing it

```
$env:OSCAR64 = "C:\path\to\oscar64.exe"       # or have oscar64 on the PATH
pwsh bench/drawing/run.ps1                    # drawing
pwsh bench/effects.ps1                        # rasterbars, scroller, diagonal
pwsh bench/math.ps1                           # mandelbrot, julia, pi (-Quick: without the bitmaps)
pwsh bench/examples.ps1                       # the sizes of all examples
```

The scripts build everything, run the timed programs in mimic-headless and
print the tables. They need oscar64, the built backend with `cargo-c64`,
mimic-headless with the C64 ROMs, and node (which reads the printed numbers
back out of the emulator's screenshot, `bench/drawing/screen.js`).

| File | |
|---|---|
| `bench/<name>/oscar64/<name>.c` | the C port; `-dTIMED` for the timed build, for drawing `-dUNCLIPPED` for the unclipped one |
| `bench/<name>/rust64-timed/` | the Rust example with the same timing code |
| `bench/examples/oscar64/*.c`, `bench/examples.ps1` | the C ports of the examples that are not timed (border, hello, joystick, ui, music, sprites, dir, sd2iec), and the sizes of all |
| `bench/math/oscar64/*.c` | the C ports of mandelbrot, julia and pi (`-dMULTICOLOR`, `-dTIMED`); the Rust side is the examples with `--features multicolor` and `timed` |
| `bench/drawing/run.ps1`, `bench/drawing/screen.js`, `bench/effects.ps1`, `bench/math.ps1` | build, run, read the results |

---

## Repository layout

| Path | Contents |
|---|---|
| `crates/rustc_codegen_rust64` | the backend: `codegen.rs` (MIR to 6510, frames, calls, data), `asm.rs` (operands, byte-wise emission, passes over the text), `opt.rs` (the data-flow pass over each function), `runtime.rat` (start/exit, panic, memcpy, mul/div) |
| `crates/cargo-c64` | the `cargo c64` command and the end-to-end test harness |
| `target-spec/mos-c64.json` | the target |
| `lib/c64`, `lib/c64-macros` | the library (Rust and `src/rat/*.rat`) and its proc macros |
| `lib/c64-*` | the extension crates |
| `examples/` | example programs |
| `tests/e2e/` | self-checking C64 programs |
| `bench/` | the comparisons with oscar64: `drawing`, `rasterbars`, `scroller`, `diagonal`, `math` |
| `docs/` | the user guide (Markdown and PDF), the phase-0 findings |
| `PLAN.md` | the design, the phases and the risks |

## Status and roadmap

| Phase | Status |
|---|---|
| 0. Spike: target, empty backend, `build-std` | done |
| 1. Core backend: integers, control flow, calls, static frames | done |
| 2. Language breadth: enums, structs, slices, closures, iterators, panics | done |
| 3. Library I: `hw`, `mem`, `sys`, `kernal`, `text`, `fmt`, `input`, assembly, `at`/`zp`, PETSCII | done |
| 4. Library II: `sprite`, `gfx` (with double buffering), `g3d`, `sound`, `irq`, `dos`, function pointers, D64 | done |
| 5. Optimization: frame overlay, zero-page allocation, block layout, data-flow pass (values, ranges, liveness, copies, index registers, loops, arguments in registers), inlining of small functions, pruning | done |
| 6. Tooling: `cargo c64 new/test`, Rust spans for assembler errors, memory map, debugger labels | open |
| 7. Extensions: recursion on a software stack, `alloc`, `dyn Trait`, `f32`, cartridges | open |

Known limits: values live in memory and are handled byte by byte, so tight
compiled loops are slower than hand-written assembly (see the
[test results](#test-results)); debug builds carry rustc's runtime checks and
are much bigger and slower than release builds; the toolchain is a pinned
nightly, because a codegen backend uses rustc's internal crates.
