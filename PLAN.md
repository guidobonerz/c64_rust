# rust64 — a Rust compiler for the Commodore 64

Plan for a compiler, written in Rust, that turns Rust programs into 6510 code
for the C64. It works like go64: generate rat assembly, assemble it to
`.prg`/`.d64`, run it in mimic. It comes with a C64 library as complete as
go64's `c64/...` packages.

```rust
#![no_std]
#![no_main]
use c64::{hw::{self, Color}, sprite::Sprite};

#[c64::main]
fn main() {
    c64::println!("hello from rust64");
    hw::vic().border.write(Color::Green);   // lda #5 / sta $d020
    let s = Sprite(0);
    s.move_to(160, 120);
    s.show();
}
```

```
cargo c64 build --release -o hello.prg
cargo c64 run                 # build and start mimic
cargo c64 asm                 # print the generated rat assembly
cargo c64 build -o game.d64
```

---

## 1. The main decision: which frontend

go64 gets parsing, type checking and SSA from the Go toolchain
(`go/types` + `x/tools/go/ssa`) and only writes the backend. Rust has no
equivalent as a stable library. There are three ways to do it:

| Option | What it means | Verdict |
|---|---|---|
| **A. A rustc backend working on MIR** (like `rustc_codegen_cranelift` / Miri) | Link `rustc_driver` and `rustc_middle` and generate 6510 code from the monomorphized MIR | **Recommended** |
| B. Your own frontend for a subset of Rust (`syn` + your own type checker) | Stable toolchain, but you rewrite type inference, trait resolution, generics, closures and method lookup, and the language slowly drifts away from Rust | Fallback only |
| C. Use llvm-mos / rust-mos | It already exists, but it is a fork of LLVM and rustc. That is not "a compiler written in Rust", and it doesn't use rat or mimic | Prior art to compare against |

**Why A:** MIR is to Rust what `go/ssa` is to Go. rustc does the parsing,
name resolution, type and trait checking, **borrow checking**,
monomorphization, closures, iterators, `const fn` evaluation and MIR
optimizations (inlining, GVN, SROA, const propagation). We get real Rust
(`no_std`, `core`, traits, generics, iterators, `Option`/`Result`, `match`,
const generics) and only write the part that is specific to the C64: the
6510 backend and the libraries.

**What it costs:** we need a pinned nightly (`rust-toolchain.toml` with
`rustc-dev`, `rust-src`, `llvm-tools`) and have to keep up with
`rustc_middle` API changes, which means a toolchain bump about once a month.
cg_clif and Miri live with the same cost. The rustc-facing code goes into one
crate (`rust64-mir`) so that only that crate breaks when the API changes.

---

## 2. Architecture

```
 .rs ─► rustc (nightly, custom target mos-c64.json, usize = 16 bit)
        │  parse / typeck / borrowck / mono / MIR opts (-Zmir-opt-level=3)
        ▼
  rustc_codegen_rust64  (codegen backend, -Zcodegen-backend=…)
        │  library crates: no code, only metadata with MIR (-Zalways-encode-mir)
        ▼
  final crate (whole program, Miri-style: walk the MIR from `main` through all crates)
        │  call graph · recursion check · static frames + overlay
        │  ZP allocation · LIR optimizations · 6510 instruction selection · peephole
        ▼
  rat assembly ─► rat-asm (as a library) ─► .prg / .d64 ─► mimic
```

### Crates (Cargo workspace)

| Crate | Job |
|---|---|
| `rust64-target` | `mos-c64.json` target spec, data layout `e-p:16:8-i16:8-i32:8-i64:8-a:8-n8`, `panic-strategy: abort`, `max-atomic-width: 0` (or 8 with `cfg` polyfills) |
| `rust64-mir` | the only crate that uses rustc-private APIs: MIR → LIR lowering, type layouts (`tcx.layout_of`), attribute reading |
| `rust64-lir` | backend IR that does not depend on rustc: functions, basic blocks, 8/16/32-bit virtual registers, memory operands (absolute / zp / frame / indirect), serde-serializable |
| `rust64-opt` | passes on LIR: copy propagation, DCE, narrowing to 8 bit, const folding, strength reduction (`*2^n`, `/2^n`), loop counters in X/Y |
| `rust64-link` | whole-program step: call graph, frame layout and overlay, ZP allocator, IRQ context analysis, dead function elimination, data placement (`at`, `align`) |
| `rust64-6510` | instruction selection LIR → 6510, register use (A/X/Y), peephole optimizer, rat emitter |
| `rust64-runtime` | `runtime.rat` (mul/div 8/16/32, memcpy/memset, compare, shifts, panic, start/exit with ZP/IRQ/VIC save like go64) |
| `rustc_codegen_rust64` | `CodegenBackend` impl (dylib), glue between rustc and the crates above |
| `cargo-c64` | CLI: `build`, `run`, `asm`, `test`, `new`; finds rat/mimic (`RUST64_RAT`, `RUST64_MIMIC`, PATH) |
| `c64` (+ `c64-macros`) | the C64 library and its proc macros (`#[c64::main]`, `sid!`, `irq` helpers) |

**rat as a library:** rat is a Rust workspace (`rat-asm`, `rat-core`,
`rat-output`). Link it directly instead of starting a process. That gives
better error messages (map rat errors back to Rust spans) and no PATH
dependency. Keep the CLI as a fallback.

**mimic as a library:** `emu-headless` / `machine-c64` for in-process
end-to-end tests (no external process, faster CI).

### Build flow (like cargo-miri)

`cargo c64 build` calls `cargo build -Zbuild-std=core -Zjson-target-spec
--target mos-c64.json` with RUSTFLAGS `-Zcodegen-backend=<our dylib>
-Zalways-encode-mir -Zinline-mir`. Library crates (including `core`) only
produce metadata with MIR. When rustc compiles the executable, the backend
walks the program from `main` through `tcx.instance_mir` of every reachable
instance and emits the assembly. Only reachable functions get code, so
`core` costs nothing except what is used. (Phase 0 result: this is simpler
than serializing LIR per crate, see docs/spike.md.)

---

## 3. Compilation model (taken from go64 and extended)

* **Types:** `u8`/`i8` are native, `u16`/`i16`/`usize`/pointers are 16 bit,
  `u32`/`i32` go through the runtime, `u64`/`i128` only for
  add/sub/logic/compare (like go64). `bool` = 1 byte, `char` = 4 bytes
  (discourage it, use `u8` + PETSCII). `f32` comes later (software IEEE float
  in Rust/rat; the BASIC ROM's FAC is MBF, not IEEE).
* **Static frames:** every function has parameters, locals and temporaries
  at fixed addresses in the BSS. Calls copy the arguments into the callee's
  frame. `&local` is a plain fixed address, which works because there is no
  recursion.
* **Frame overlay (better than go64):** functions that can never be active
  at the same time (not on the same call-graph path) share BSS memory. This
  is graph coloring over the call graph and saves a lot of RAM.
* **Recursion:** first a compile error showing the cycle (like go64).
  Phase 7: recursive SCCs push their frame onto a software stack when
  entered. Only those functions pay for it.
* **No heap at first:** `no_std` without `alloc` is the normal case in Rust
  anyway. Phase 7: `c64::alloc` (bump or free-list `GlobalAlloc` between BSS
  and `$9FFF`), which brings `alloc::Vec`/`String`/`Box` along for free.
  go64 can't do that.
* **Traits/generics/closures/iterators:** rustc has already monomorphized
  them. Closures are structs plus a direct call. Iterator chains depend on
  the MIR inliner plus our LIR passes (benchmark: `for i in 0..40` must turn
  into a `dex/bne` loop).
* **Indirect calls** (`fn()` pointers, `dyn Trait`, IRQ handlers): the callee
  isn't known statically, so functions whose address is taken get a uniform
  argument area (`rt_args`, ZP) and copy it into their frame in the
  prologue. vtables become static data.
* **Panic:** `panic=abort` + `panic_immediate_abort`. `#[panic_handler]` in
  `c64` turns the border red and prints the location if `debug-panic`
  (feature) is on. Bounds checks stay on in debug and can be switched off in
  release via `-Zub-checks=no` + `get_unchecked` in the library.
* **Formatting:** don't use `core::fmt` (dyn-heavy, recursive, kilobytes of
  code). Instead use a `ufmt`-like trait `c64::fmt::Write`/`uDisplay` with
  macros `print!`/`println!`/`write!`. The macros work at compile time and
  have no vtables.
* **Strings:** `&str` = (ptr, len). Literals are converted to PETSCII. A
  macro `p!("HELLO")` or a crate attribute `#![c64::charset(mixed)]` picks
  upper case or mixed case. `b'A'` stays ASCII (like go64's rune literals).
* **Static initialization:** rustc's const eval turns `static T: [u16; 96] =
  …` into finished bytes. We emit them as `.byte` data and generate no code.

### go64 directives as Rust

rustc rejects unknown attributes. Stable ways around that are
`#[link_section]` and `#[repr(align)]`, and on nightly `#![register_tool(c64)]`:

| go64 | rust64 |
|---|---|
| `//go64:at $d020` | `#[c64::at(0xd020)] static mut X: u8;` or `#[link_section = "at:$d020"]`; hardware registers usually through `Volatile<T>` at a `const` address |
| `//go64:zp` | `#[c64::zp] static mut X: u8 = 0;` (`link_section = "zp"`) |
| `//go64:align 64` | `#[repr(align(64))]` (native Rust) |
| `//go:embed` | `include_bytes!` (native), placed at an address with `#[c64::at(0x2000)]` |
| `//go64:sid f.sid` | `c64::sid!(TUNE, "f.sid")`: a proc macro reads the PSID header and places the data at its load address |
| bodiless function + `.rat` | `unsafe extern "C" { fn mem_copy(d: u16, s: u16, n: u16); }` + `c64::rat_source!(MEM, "mem.rat")` (a byte-array static in section `rat`; the backend includes those of the program and of every crate that declares a called assembly function). `extern "rat"` is impossible: rustc rejects unknown ABIs. Calling convention as in go64: all argument bytes in `name__args`, the first 3 also in A/X/Y; results up to 3 bytes in A/X/Y, larger ones in `name__res` |

`asm!` does not work on custom arches, so for inline code a macro
`c64::asm!("…")` passes rat text through an intrinsic (Phase 6).

---

## 4. The `c64` library

The same scope as go64, with Rust idioms: newtypes, enums, RAII, no
`unsafe` in user code.

| Module | Contents (go64 equivalent + additions) |
|---|---|
| `c64::hw` | `vic()`, `sid()`, `cia1()`, `cia2()` as `#[repr(C)]` register structs made of `Volatile<u8>`; `Color` enum; `screen()`, `color_ram()`, `sprite_pointers()`, `cpu_port()`; `wait_raster`, `wait_frame`, `set_vic_bank`, `set_screen` |
| `c64::kernal` | `chrout`, `chrin`, `getin`, `scnkey`, `stop`, `plot`, `cursor`, `setlfs`, `setnam`, `open`, `close`, `chkin`, `chkout`, `clrchn`, `clall`, `readst`, `load`, `save`, `rdtim`, `settim`, `jiffies` |
| `c64::text` | `print`, `clear`, `locate`, `set_color`, `put_char`, `put_str`, `fill`, `fill_rect`, `scroll_up`, `screen_code`, `set_screen_addr`, `set_charset`, `lowercase`/`uppercase`; `print!`/`println!` |
| `c64::fmt` | `uWrite`/`uDisplay`, number output dec/hex/bin with no division in the loop (BCD) |
| `c64::sprite` | `Sprite(u8)` with `load`, `set_image`, `show`, `hide`, `move_to` (9-bit X), `x`, `y`, `set_color`, `set_multicolor`, `expand`, `behind`; `shared_colors`, `collisions`, `bg_collisions`, `hit` |
| `c64::gfx` | `Hires`/`Multicolor` modes as types (`gfx::hires() -> Bitmap<Hires>`), `clear`, `set_colors`, `plot`, `point`, `line` (Bresenham), `rect`, `fill_rect`, `circle`, `show_koala`, `Pen` |
| `c64::sound` | `Voice(u8)`: `freq`, `pulse_width`, `adsr`, `play`, `note_on/off`; `filter`, `volume`, `reset`; `Note` enum + PAL table; `Tune::start/stop` (PSID) |
| `c64::irq` | `irq::add(line, handler: fn())` (up to 8, chained), `on_frame`, `stop`, `critical_section(|| …)` |
| `c64::input` | `joy(Port) -> Joy` (bitflags: up/down/left/right/fire), `key`, `wait_key`, `pressed(row, col)` |
| `c64::dos` | `load`, `load_abs`, `save`, `command`, `scratch`, `rename`, `status`; `File` (`get`/`read`/`write`/`write_str`, **`Drop` closes it**); `Dir` as an `Iterator<Item = DirEntry>` |
| `c64::mem` | `peek`, `poke`, `bytes(addr, n) -> &'static mut [u8]` (unsafe), `copy`, `fill` (rat) |
| `c64::sys` | `exit`, `sei`/`cli`, `jsr`, `without_interrupts` |
| **additions** | `c64::math` (8.8 fixed point, sin/cos tables, `rnd` via SID noise / LFSR), `c64::charset` (custom charsets, `include_charset!`), `c64::scroll` (smooth scroll with `$d016`/`$d011`), `c64::timer` (CIA timers), later `c64::reu` |

**Builds on the host:** `#[cfg(target_os = "c64")]` picks the real
implementation, otherwise a stand-in (a simulated 64 KB array, printing goes
to stdout). That way `cargo test` on the host, rust-analyzer and clippy work
as usual. Library logic (fmt, math, line drawing) gets unit tests on the host.

---

## 5. Testing

1. **Unit tests** per LIR/6510 pass (`rust64-opt`, `rust64-6510`).
2. **Snapshot tests** (`insta`) of the generated rat code for small Rust
   functions, to catch regressions in code size and quality.
3. **End-to-end tests** like `go64/compiler/e2e_test.go`: programs in
   `tests/e2e/*` check themselves and set the border to green/red. They run in
   mimic headless (in-process via `emu-headless`), and the test checks the
   border colour or a memory byte after N frames. Topics: integer arithmetic
   at every width, overflow/wrapping, match/enum, slices, iterators, closures,
   traits/dyn, statics, IRQ, kernal/dos (with a D64 image), libs.
4. **Benchmarks:** code size and cycle counts (mimic's profiler) for a fixed
   set (fib, sieve, line drawing, memcpy), tracked over time.
5. **Host tests** of the `c64` crate with the stand-ins.

---

## 6. Phases

| Phase | Goal | Done when |
|---|---|---|
| **0. Spike** ✅ done 2026-11-10 (docs/spike.md) | Pin nightly, `mos-c64.json` with 16-bit pointers, empty `CodegenBackend`, `-Zbuild-std=core` builds. Check the risks: does rustc accept an unknown `arch` (otherwise use `msp430` as the 16-bit little-endian base)? Does `link()` get all LIR objects? | `fn main(){}` gives a `.prg` that returns cleanly to BASIC |
| **1. Core backend** | MIR → LIR for u8/u16/bool, locals, assignments, `if`/`loop`/`while`, direct calls, static frames, `runtime.rat` start/exit, linking rat-asm in | `poke(0xd020, 5)` and a 16-bit Fibonacci run in mimic |
| **2. Language breadth** | u32/i8/i16/i32, `match`, enums (discriminant + niche layout from `layout_of`), structs/tuples/arrays, references, slices, `&str`, statics/consts as data, generics/traits (already mono), closures, iterator chains, panic/bounds checks, recursion error | E2E core suite green (like `go64/testdata/core`) |
| **3. Library I** ✅ done 2026-11-10 (`lib/c64`, `lib/c64-macros`, `tests/e2e/lib`) | `hw`, `mem`, `sys`, `kernal`, `text`, `fmt`, `input`; `extern "C"` + `rat_source!`; `#[c64::at]`/`#[c64::zp]`; PETSCII literals (`c64::p!`/`pm!`); `#[c64::main]` | `hello`, keyboard/joystick examples |
| **4. Library II** ✅ done 2026-11-10 (`tests/e2e/lib2`, `tests/e2e/gfx`, `examples/{hello,sprites,drawing,music,dir}`) | `sprite`, `gfx`, `sound` (+ `sid!`), `irq` (fn pointers, uniform argument area, IRQ context: functions reached through pointers are compiled as separate copies), `dos` (+ D64 output) | Port of every go64 example: hello, sprites, drawing, music, dir |
| **5. Optimization** (mostly done 2026-11-10: known addresses, views, compare-and-branch, X-indexed tables, frame overlay + ZP frames, block layout, peephole/relaxation, runtime pruning; examples 48-76% smaller, up to 2.75x faster. Open: X/Y loop registers, values in registers, tail calls, link-step inlining) | Frame overlay, ZP allocator (hot variables/pointers into `$16-$8F`), 8-bit narrowing, X/Y as loop registers, peephole, tail calls → `jmp`, inlining small leaf functions in the link step | Code size ≤ go64 for the same examples; benchmarks recorded |
| **6. Tooling** | `cargo c64 new/run/asm/test`, error messages with Rust spans for rat/link errors, memory map report (`--map`), source-level labels for mimic's debugger, `c64::asm!` | A new project works without reading docs |
| **7. Extensions** | Software stack for recursive SCCs, `c64::alloc` + `alloc::Vec/String`, `dyn Trait` everywhere, soft float `f32`, cloning functions shared between IRQ and main instead of warning, cartridge/`.crt` output | Optional, by priority |

Phases 0–2 are the critical path. Libraries (3–4) can then be built in
parallel with optimization (5).

---

## 7. Risks and how to handle them

| Risk | Mitigation |
|---|---|
| rustc-private API changes | Keep all rustc code in `rust64-mir`, pin nightly, bump it on purpose with E2E as the check |
| Custom target / unknown arch is rejected | **Happened** (no C ABI lowering for unknown arches): using `arch: "msp430"` (16-bit LE) as the base, C64 = `target_os = "c64"` |
| Code bloat from `core` (iterators, bounds checks, `fmt`) | MIR opt level 3, our own inlining, no `core::fmt`, measure with benchmarks early |
| Recursion inside `core` (e.g. `sort`) | Compile error with a path to the cycle; `c64` provides non-recursive alternatives; later the software stack |
| IRQ and main share static frames | **Done in Phase 4:** functions reached through a function pointer (handlers) are compiled a second time with their own frames; the dispatcher saves the runtime registers (only the zero-page pointers if the handler code uses no runtime helpers). Remaining: a pointer copy running in main while an interrupt runs the same copy |
| 6510 performance of 16/32-bit code | Narrowing passes, encourage `u8` in the API, use ZP pointers for `(zp),y` |
| RAM: program + BSS must stay below `$A000` | Frame overlay, map report, linker check like go64 (also against `at` blocks) |

---

## 8. First concrete steps

1. Set up the workspace in `c64_rust` (crates from section 2, `rust-toolchain.toml`).
2. Phase 0 spike: `mos-c64.json` + empty backend + `build-std`, and write down
   the findings in `docs/spike.md`.
3. Port `runtime.rat` from go64 (start/exit/ZP save, mul/div, memcpy).
4. Link `rat-asm` as a path dependency and build an empty `.prg`.
5. E2E harness with `emu-headless`, first test: border green.
