# Phase 0 spike: results (2026-11-10)

Goal: `fn main() {}` -> `.prg` that returns to BASIC. **Reached**, and more:
`tests/e2e/core` (36 checks: 8/16/32-bit arithmetic, signed compare,
mul/div/shift, loops, arrays, slices, iterators, closures, structs, enums,
`Option` niches, statics, strings) passes in debug and release.

## Toolchain

* Nightly `nightly-2026-11-10` (rustc 1.102.0-nightly ac8310ba6), components
  `rustc-dev`, `rust-src`, `llvm-tools`. The rustc sources for API lookups
  are in `~/.rustup/toolchains/<nightly>/lib/rustlib/rustc-src/rust/compiler`.
* Build flags (set by `cargo c64`): `-Zbuild-std=core -Zjson-target-spec
  --target target/rust64/mos-c64.json`, RUSTFLAGS `-Zcodegen-backend=<dll>
  -Zalways-encode-mir -Zinline-mir`.

## Findings

| Question | Answer |
|---|---|
| Custom arch `"mos"`? | Parses (`Arch::Other`), but `fn_abi` for `extern "C"` panics: "no lowering implemented for mos" (rustc_target/src/callconv/mod.rs). **Fallback in use: `"arch": "msp430"`** (16-bit little endian; LLVM is never called). Identify the C64 by `target_os = "c64"`. |
| 16-bit pointers / data layout | `e-m:e-p:16:8-i16:8-i32:8-i64:8-f32:8-f64:8-a:8-n8-S8` works; alignment 1 everywhere. |
| Panics | `"panic-strategy": "immediate-abort"` in the target: panics end in the `abort_immediate` intrinsic -> `rt_panic` (red border, back to BASIC). A `#[panic_handler]` is still required. |
| `.prg` name | `"exe-suffix": ".prg"`, so cargo's output is `target/mos-c64/<profile>/<name>.prg`. |
| Per-crate objects vs. whole program | **Whole program at the final crate** (Miri-style): library crates emit no code, only metadata with MIR (`-Zalways-encode-mir`). The executable's `codegen_crate` walks from `main` through `tcx.instance_mir` of every reachable instance (including `core`). No LIR serialization needed. |
| MIR inliner | Off for `opt-level = "s"/"z"` unless `-Zinline-mir`. Without inlining, `ub_checks` stay as calls and pull in dead code. |
| `ub_checks` & co. | `Operand::RuntimeChecks(c)` must be evaluated with `c.value(sess)` (the final crate's settings). Plus a small analysis: locals that only ever get one constant (also via calls to functions that always return it) fold `switchInt`, and unreachable blocks are not compiled. |
| API changes vs. older docs | `FnDef` args are a `Binder` (`skip_binder()`), drop glue is `InstanceKind::Shim(ShimKind::DropGlue(..))`, `Instance::resolve_drop_glue`, `rustc_middle::mono::MonoItem`, `CrateType` in `rustc_structures`, `EncodedMetadata` in `rustc_metadata`, `EarlyBinder::bind(tcx, v)`, `niche_variants.start/.last` are fields. |
| Rebuilds | Cargo does not notice a new backend dll; `cargo c64` cleans the program's crate when the dll changes (stamp in `target/rust64`). |

## Not done yet (next phases)

* Code quality, Phase 5: mostly done (known addresses, views instead of
  copies also across branches, compare-and-branch, X-indexed tables, frame
  overlay with zero-page frames, block layout, branch relaxation, peephole,
  pruning of unused runtime: the examples are 48-76% smaller and up to
  2.75x as fast). Open: values in registers across statements, X/Y loop
  counters, inlining at program level. More aggressive rustc inlining
  (`-Zinline-mir-threshold`) was tried: mixed sizes, no speed gain.
* `jmp (vector)` must not be used with a vector that can lie across a page
  boundary (6502 bug): `rt_jsrind` is a self-modified `jmp`.
* `MulWithOverflow` never reports overflow (debug builds only).
* `dyn` calls (vtables), `ptr::copy` (memmove), floats, 128-bit mul/div.
  (Assembly functions work since Phase 3: `extern "C"` declarations +
  `c64::rat_source!`; calls through function pointers since Phase 4:
  arguments in `rt_args`, result in `rt_ret`, at most 32/16 bytes.)
* Recursion through function pointers is not detected.
* Unoptimized code is large and slow: a debug build that uses `c64::gfx`
  must end below `$4000` and only fits without debug assertions
  (`tests/e2e/gfx`). Busy-wait loops must not compare the raster line for
  equality (one check takes longer than a line; see `hw::wait_raster`).
* `panic!` with format arguments in user code still pulls in `core::fmt`
  (dyn calls, not supported). Diverging `core` functions (`panic_fmt`,
  `slice_index_fail`, ...) are compiled as `jmp rt_panic`.
* Real `Location`s for `#[track_caller]` (a zeroed dummy is passed).
* Phase 5 (optimization) and later.
