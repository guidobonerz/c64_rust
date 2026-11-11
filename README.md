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
    wrote .../target/mos-c64/release/hello.prg        (583 bytes)
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
when printed; `c64::p!("HELLO")` converts at compile time.

## Libraries

Everything a rust64 program can use, besides `core`. What a program does not
call is not in the `.prg`, so depending on a crate costs nothing by itself.
The guide (chapter 8) lists every function.

### The `c64` crate

Every program depends on it: `c64 = { path = ".../lib/c64" }`.

| Module | For | Contents |
|---|---|---|
| `c64::hw` | hardware registers | VIC-II, SID and CIA as typed structs, `Color`, raster waits, VIC bank and screen setup |
| `c64::text` | the character screen | print, cursor, colours, `put_char`/`put_str`, fills, scrolling, character sets |
| `c64::fmt` | formatting | `print!`/`println!`/`write!` without `core::fmt`: decimal, `Hex`, `Bin`, a fixed `Buf` |
| `c64::ui` | boxes on the text screen | `frame`, `panel`, `titled_frame`, `titled_panel`; square or round corners |
| `c64::sprite` | hardware sprites | images, position (9-bit X), colours, multicolour, expansion, collisions |
| `c64::gfx` | bitmap graphics | hires and multicolour: `plot`, `line`, `fline`, `hline`, `vline`, `rect`, `fill_rect`, `circle`, cell colours, Koala pictures |
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
| `#[c64::at(0x2000)]` | a static at a fixed address |
| `#[c64::zp]` | a static in the zero page |
| `c64::sid!` | a PSID tune in the program |
| `c64::rat_source!(NAME, "file.rat")` | a rat assembly file in the program |
| `c64::asm!(...)` | inline 6510 assembly |

| Cargo feature | Effect |
|---|---|
| `gfx-unclipped` | `c64::gfx` draws without checking the coordinates against the screen: smaller and faster, but every pixel must be on the screen |

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
| `hello` | printing, a loop, the border colour | `hw`, `fmt` | 583 |
| `joystick` | reading the joystick | `input` | 1067 |
| `ui` | boxes on the text screen | `ui`, `text` | 1439 |
| `music` | notes and a SID tune | `sound` | 1476 |
| `sprites` | a moving sprite | `sprite` | 1535 |
| `rasterbars` | a stable raster interrupt in assembly, colours from Rust | `rat_source!`, `mem` | 1733 |
| `scroller` | a horizontal soft scroll with two raster interrupts | `irq`, `mem` | 1885 |
| `drawing` | lines, a rectangle and circles on the hires bitmap | `gfx` | 2663 |
| `diagonal` | the whole screen scrolling diagonally, custom character set | `hw`, `mem` | 3155 |
| `dir` | the disk directory | `dos` | 4109 |
| `wic64` | the WiFi module | `c64-wic64` | 3392 |
| `uci` | the Ultimate command interface | `c64-uci` | 4466 |
| `sd2iec` | directories and disk images | `c64-sd2iec` | 7626 |

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
| `ui` | `c64::ui` | ok | ok |
| `sd2iec` | `c64-sd2iec` against mimic's SD2IEC | ok | ok |
| `uci` | `c64-uci` against mimic's command interface (no network) | ok | ok |
| `wic64` | `c64-wic64` against mimic's WiC64 (no network) | ok | ok |
| `uaudio` | `c64-uaudio`, as far as possible without the hardware | ok | ok |

mimic does not emulate the Ultimate Audio sampler: `c64-uaudio` follows the
Ultimate's FPGA source and has not been run against hardware.

### Size and speed: comparison with oscar64

[oscar64](https://github.com/drmortalwombat/oscar64) is an optimizing C/C++
compiler for the 6502 with a mature C64 library, and the usual yardstick for
compiled code on this machine. Four of the examples were ported to C for
oscar64 1.32.266 (`bench/<name>/oscar64/`), and both versions were checked
against each other on the emulated screen.

#### Summary

rust64 `--release` against oscar64 `-Os`; less is better in every column.

| Program | Work is done by | `.prg` bytes rust64 | oscar64 | rust64 is | Cycles rust64 | oscar64 | rust64 needs |
|---|---|---:|---:|---|---:|---:|---|
| drawing, clipped | library assembly | **2663** | 3147 | 15% smaller | **914 677** | 968 876 | 5.6% fewer |
| drawing, unclipped | library assembly | **2275** | 2046 | 11% larger | **891 912** | 884 436 | 0.8% more |
| scroller | interrupts, one block copy | **1885** | 1355 | 39% larger | **317** | 385 | 18% fewer |
| rasterbars | small fills and copies | **1733** | 1096 | 58% larger | **2810** | 1877 | 50% more |
| diagonal | compiled code, block copies | **3155** | 1126 | 2.8 times as large | **3152** | 2485 | 27% more |

Cycles: the whole program for drawing, the average frame for the others.

**The result in one sentence:** where the work is done by the library's
assembly, rust64 is level with oscar64 or ahead; where it is done by compiled
code, oscar64 is ahead, in size by a wide margin.

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
  converted to screen codes at run time in Rust and at compile time in C
  (`s"..."`).
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
| **rust64** `--release`, clipped | **2663** | | 110 481 | 451 360 | 15 123 | 331 797 | 5 916 | **914 677** | 0.93 | |
| oscar64 `-O1`, clipped | 4153 | 36% smaller | 103 501 | 388 147 | 31 470 | 461 001 | 1 188 | 985 307 | 1.00 | 7.2% fewer cycles |
| oscar64 `-Os`, clipped | 3147 | 15% smaller | 108 050 | 371 991 | 28 068 | 459 661 | 1 106 | 968 876 | 0.98 | 5.6% fewer |
| oscar64 `-O2`, clipped | 3202 | 17% smaller | 103 117 | 378 440 | 29 725 | 458 098 | 1 063 | 970 443 | 0.98 | 5.7% fewer |
| oscar64 `-O3`, clipped | 3658 | 27% smaller | 103 301 | 371 179 | 27 457 | 415 822 | 1 132 | 918 891 | 0.93 | 0.5% fewer |
| **rust64** `--release`, unclipped | **2275** | | 110 481 | 449 609 | 14 775 | 310 834 | 6 213 | **891 912** | 0.91 | |
| oscar64 `-O1`, unclipped | 2724 | 16% smaller | 103 454 | 370 254 | 27 687 | 390 819 | 1 102 | 893 316 | 0.91 | 0.2% fewer |
| oscar64 `-Os`, unclipped | 2046 | 11% larger | 108 035 | 363 284 | 27 012 | 384 917 | 1 188 | 884 436 | 0.90 | 0.8% more |
| oscar64 `-O2`, unclipped | 2004 | 14% larger | 103 306 | 362 185 | 27 085 | 384 992 | 1 192 | 878 760 | 0.89 | 1.5% more |
| oscar64 `-O3`, unclipped | 2297 | 1% smaller | 103 006 | 362 083 | 26 993 | 342 487 | 1 044 | 835 613 | 0.85 | 6.7% more |

(The rust64 dev profile, with overflow and UB checks, gives 5244 bytes.)

**rasterbars, scroller, diagonal** — size, and CPU cycles per frame.

| Program | Build | `.prg` bytes | average frame | worst frame | other |
|---|---|---:|---:|---:|---|
| rasterbars | **rust64** `--release` | **1733** | **2810** | | |
| | oscar64 `-Os` | 1096 | 1877 | | |
| | oscar64 `-O2` | 1136 | 1737 | | |
| | oscar64 `-O3` | 1151 | 1684 | | |
| scroller | **rust64** `--release` | **1885** | **317** | **1174** | interrupts per frame: **983** |
| | oscar64 `-Os` | 1355 | 385 | 1695 | 826 |
| | oscar64 `-O2` | 1498 | 369 | 1621 | 824 |
| | oscar64 `-O3` | 1498 | 385 | 1694 | 817 |
| diagonal | **rust64** `--release` | **3155** | **3152** | **7088** | start-up: **294 349** |
| | oscar64 `-Os` | 1126 | 2485 | 5634 | 69 490 |
| | oscar64 `-O2` | 1161 | 2484 | 5630 | 67 915 |
| | oscar64 `-O3` | 1161 | 2398 | 5464 | 68 075 |

#### Reading the numbers

Where the library's assembly does the work (drawing, and the scroller's block copy and interrupts):

* **Clipped, rust64 is ahead.** Against oscar64's `-Os` and `-O2` it is
  about 6% faster and 15–17% smaller; against `-O3` it is as fast and 27%
  smaller.
* **Unclipped, oscar64 is slightly ahead.** Against `-Os` and `-O2` rust64
  is 1–2% slower and 11–14% larger; against `-O3` it is 7% slower and the
  same size.
* **What dropping the checks buys** is different on the two sides. oscar64
  saves 1100–1400 bytes and about 9% of the time. rust64 saves 388 bytes
  and 2.5%: its line loop never checked each pixel (only the two ends), so
  the gain is in the circles (6%) and in the code for lines that leave the
  screen, which is no longer in the program.
* **Lines: oscar64 needs 14–20% fewer cycles.** Its `bmu_line` assembles a
  loop specialized for each line (direction, slope, pattern) at run time and
  then runs that. rust64's `line` checks the two ends against the screen and
  then runs one of two fixed Bresenham loops in assembly; it modifies no
  code, so that interrupt handlers may draw too.
* **Rectangle: rust64 is twice as fast**, because horizontal edges are stored
  a byte at a time, while the C version draws four general lines.
* **Circles: rust64 needs 20–28% fewer cycles than C when both clip, and
  9–20% fewer when neither does** — hand-written assembly against compiled C
  calling `bm_set` for every pixel.
* **Setup** is two memory fills on both sides; rust64 is 2–7% slower there.
* **scroller: rust64's main loop is faster** (14–18% fewer cycles on average,
  28–31% fewer in the worst frame, the one that moves the 39 bytes of the
  row: `mem::copy` in rust64, `memmove` in C).
* **scroller interrupts: about level.** rust64 needs 983 cycles per frame,
  oscar64 about 820, and most of both is the KERNAL's keyboard scan. A Rust
  function as a handler goes through a dispatcher that saves registers;
  oscar64's handler is a handful of generated instructions.

Where compiled code does the work:

* **Size: oscar64 wins all three effect programs.** rust64's are 51–58%
  larger for rasterbars, 26–39% larger for scroller and 2.7–2.8 times as
  large for diagonal.
* **diagonal is the clearest case.** In rust64's listing the function `init`
  alone is 464 instructions and `main` another 270: nested loops over arrays
  with `iter_mut().enumerate()`, a `%`, a `match` and 16-bit address
  arithmetic, all with the values in memory. The start-up, which is that
  code, takes 4.3 times as long as in C; the per-frame work, mostly block
  copies, 27–31% longer.
* **rasterbars: rust64 needs 50–67% more cycles per frame** for one fill and
  three small copies. rust64 calls its general fill and copy routines four
  times, with the arguments passed through memory; oscar64 expands `memset`
  and `memcpy` of a known size in place (its program has no such routines).
  Part of rust64's size is not code: `raster.rat` aligns its timed code to a
  page and keeps the colour table in the program file, while the C version
  has the table outside the file.
* **The *cells* loop of drawing** is the only phase of that program that is
  purely compiler output on both sides, and there oscar64 is 5 to 6 times as
  fast (26–30 cycles per iteration against 148). rust64 keeps every value in
  memory, computes the 16-bit address and stores through a pointer; oscar64
  keeps the counter in X and stores with `sta $43c0,x`. The loop is 0.6% of
  the program's run time, so it does not show in the total.

The open optimizations (values in registers, loop counters in X/Y, passing
arguments in registers, expanding small fills and copies in place) are
exactly what these programs need.

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
```

The scripts build everything, run the timed programs in mimic-headless and
print the tables. They need oscar64, the built backend with `cargo-c64`,
mimic-headless with the C64 ROMs, and node (which reads the printed numbers
back out of the emulator's screenshot, `bench/drawing/screen.js`).

| File | |
|---|---|
| `bench/<name>/oscar64/<name>.c` | the C port; `-dTIMED` for the timed build, for drawing `-dUNCLIPPED` for the unclipped one |
| `bench/<name>/rust64-timed/` | the Rust example with the same timing code |
| `bench/drawing/run.ps1`, `bench/drawing/screen.js`, `bench/effects.ps1` | build, run, read the results |

---

## Repository layout

| Path | Contents |
|---|---|
| `crates/rustc_codegen_rust64` | the backend: `codegen.rs` (MIR to 6510, frames, calls, data), `asm.rs` (operands, byte-wise emission), `runtime.rat` (start/exit, panic, memcpy, mul/div) |
| `crates/cargo-c64` | the `cargo c64` command and the end-to-end test harness |
| `target-spec/mos-c64.json` | the target |
| `lib/c64`, `lib/c64-macros` | the library (Rust and `src/rat/*.rat`) and its proc macros |
| `lib/c64-*` | the extension crates |
| `examples/` | example programs |
| `tests/e2e/` | self-checking C64 programs |
| `bench/` | the comparisons with oscar64: `drawing`, `rasterbars`, `scroller`, `diagonal` |
| `docs/` | the user guide (Markdown and PDF), the phase-0 findings |
| `PLAN.md` | the design, the phases and the risks |

## Status and roadmap

| Phase | Status |
|---|---|
| 0. Spike: target, empty backend, `build-std` | done |
| 1. Core backend: integers, control flow, calls, static frames | done |
| 2. Language breadth: enums, structs, slices, closures, iterators, panics | done |
| 3. Library I: `hw`, `mem`, `sys`, `kernal`, `text`, `fmt`, `input`, assembly, `at`/`zp`, PETSCII | done |
| 4. Library II: `sprite`, `gfx`, `sound`, `irq`, `dos`, function pointers, D64 | done |
| 5. Optimization: frame overlay, zero-page allocation, 8-bit narrowing, block layout, peephole, pruning | mostly done; open: values in registers, X/Y loop counters, link-step inlining |
| 6. Tooling: `cargo c64 new/test`, Rust spans for assembler errors, memory map, debugger labels | open |
| 7. Extensions: recursion on a software stack, `alloc`, `dyn Trait`, `f32`, cartridges | open |

Known limits: values live in memory and are handled byte by byte, so tight
compiled loops are slower than hand-written assembly (see the
[test results](#test-results)); debug builds carry rustc's runtime checks and
are much bigger and slower than release builds; the toolchain is a pinned
nightly, because a codegen backend uses rustc's internal crates.
