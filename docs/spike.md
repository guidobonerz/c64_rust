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
  overlay with zero-page frames, block layout, branch relaxation, pruning
  of unused runtime; since 2026-11-23 a data-flow pass over the assembly
  of each function, `opt.rs`: values and ranges, liveness, merged copies,
  index registers, loop counters in X/Y, loop tests at the end and 8 bits wide for slices of known length, an index in Y in place of an iterator's pointer, invariant code out of loops, and frames that only keep the
  bytes still in use; arguments and results in A, X and Y; since
  2026-11-24 `inline.rs`: the assembly of a function replaces its calls
  if it has one caller or is not longer than what the calls save).
* Start and exit (2026-11-24): the exit code no longer restores saved
  hardware registers. It sets what BASIC starts with: `$01`, the KERNAL's
  `IOINIT`, `$D011`-`$D01A` from the KERNAL's table at `$ECC9`, and the
  IRQ vector if the program names it. Nothing is saved at the start; most
  programs are 105 bytes smaller. Numbers of up to 16 bits have a digit
  routine of their own (78 bytes instead of 167), which only passes the
  number on where the program has the 32-bit one too (`.ifdef` in a
  library routine: `asm::prune` counts the first branch only if the other
  routine is kept).
* Structs and enums (2026-11-24, after the C ports of `dir` and `sd2iec`
  came out at half the size): views of more than 16 bytes, temporaries
  that are built where they go (`forwarded`), results that are read in
  the callee's frame until the next call (`result_views`), `!cmp` fused
  into the branch, comparisons that a dominating branch has made
  (`implied`), no test for the last variant of a match whose rest is
  unreachable, one comparison for an enum with a niche (`niche_switch`),
  and in `opt.rs` a direct access where pointer and Y are known
  (`Plan::Direct`). `dir` 2323 to 1945 bytes, `sd2iec` 4721 to 4004.
  Then: several jumps back to a loop's head are led through one block,
  so the step can sink there (`funnel`); the registers a function reads
  at its entry are noted and the callers load only those (`entry_regs`);
  a reference to a value without bytes that is only handed on is no
  argument (`unpassed_args`). `dir` 1827, `sd2iec` 3840, `hello` 402.
  Then the frames in two parts (`compact_frames`, `Frame`; values of up
  to `FRAME_SMALL` bytes in `F<n>f`, the others in `F<n>g`, both placed
  with the same rule, all small parts first): what the rule for inlined
  frames below had pushed out of the zero page is back in it, and more.
  `dir` 1744, `sd2iec` 3608, `uci` 2397; the other programs are as they
  were (their frames were in the zero page already).
  `RUST64_FRAME_SMALL=<n>` sets the limit for a build (1000: one part).
  Then counters of two bytes that stay below 256 (`narrow_locals`,
  `below` in `codegen.rs`: every assignment is a small constant, an
  extended byte, or `n + k` under a dominating `n < c` with `c + k <=
  256`; such a local is read as its low byte and `#$00`, the stores to
  the high byte die in `opt.rs`), and `address + constant` folded for the
  assembler (`fold_addr` in `opt.rs`; `span` knows `X` and `X+k`, so the
  low-byte comparison of `narrow_test` still applies). `dir` 1710,
  `sd2iec` 3553, `music` 1052, `rasterbars` 1314 and 1930 cycles a frame,
  `diagonal` 1332. Lengths in structs (`Name::len`) are still two bytes.
  Then locals that share their bytes (`join_locals`, `flow`, `Flow` in
  `codegen.rs`): for every move of four bytes or more between two locals
  (also through a view, and into an aggregate) the smaller one is placed
  in the larger one if the two do not clash: one is written (also through
  a pointer while a reference to it may exist) where the other is read or
  holds a value that is read later. The result is in `fwd`; groups are
  checked pair by pair; arguments and the result stay where they are.
  Addresses noted before (`addrs`) are rewritten. In `inline_fns` a
  pasted function writes its in-memory result to the caller's variable
  if the copy after the call takes all of it and no reference to the
  variable exists at the call (`inline::result_copy`, `free_dests`).
  `dir` 1647, `sd2iec` 3421. `RUST64_NO_JOIN=1`, `RUST64_JOIN_TRACE=1`.
  Left: the result of a function that stays a function is still copied
  out of its frame if it is needed after the next call (`Dir::line` to
  the entry of the loop: 23 bytes a line). Counted before building it:
  one such copy in `dir`, two in `sd2iec`, about 11 bytes each, against a
  new rule for the frames (the callee's frame above everything called
  while the result is read, with the inlined cases apart). Not done.
  Instead the test of a one-byte result, which every `match` on what
  `open` or `chkin` returns ends in: `lda v / bne a / lda #0 / beq b /
  a: jmp c / b:`. In `opt.rs` a load of a constant goes where A holds it
  and the flags are known to be its flags (`bne` not taken: zero, not
  negative); in `asm::relax_with` `bne a / beq b / a: jmp c / b:` becomes
  `bne c`; a load that is only there for the flags becomes `ora #$00`,
  and the store before it dies. A switch does not compare bytes of its
  value that are constants. `dir` 1583, `sd2iec` 3348, `uci` 2233,
  `wic64` 1944. The same test of a tag of two bytes (`if let Some(n) =
  t.0`: the upper byte is a constant only after `asm::forward`) leaves
  `lda #0` / `beq` as a branch that is always taken; `backward` in
  `opt.rs` makes a `jmp` of it and drops the load where neither the
  register nor the flags are read after it. `dir` 1566, `sd2iec` 3328,
  `music` 1039.
  `music` against the C port (559): 452 of its 1039 bytes are library
  assembly (the table of frequencies 192, the SID routines 171, text 85)
  where the C program has the frequencies as constants in its table and
  writes the registers itself; `wait` is in the program three times
  (rustc inlines it; `-Zinline-mir-threshold=50` and 70 change nothing
  there and nothing in total).
* The registers of the runtime routines and the argument areas of the
  assembly functions in the zero page (2026-11-25). Both were in the BSS:
  three bytes and four cycles for an access that the library's assembly
  makes in nearly every line. Now the code generator defines them in
  front of the code (rat takes the zero-page instruction only for a name
  it knows already): the runtime registers the program uses, as one block
  in their order (`RT_REGS`; the routines index from one to the other,
  `rt_save`/`rt_save_n` for `rt_ctx_save`), then the areas, then the
  frames. `runtime.rat` no longer defines them; `rt_args` and `rt_ret`
  stay in the BSS. All fifteen examples: 19 440 to 18 469 bytes (8% over
  oscar64 `-Os`, without `sd2iec` level). pi 17.49 to 14.81 million
  cycles, the PETSCII Mandelbrot set 21.69 to 19.15, drawing 907 701 to
  853 848; the multicolour sets 701 to 619 and 874 to 770 million. The
  frames lose 10 to 80 bytes of the zero page; no example got larger.
  `RUST64_NO_ZP_RT=1` and `RUST64_NO_ZP_ARGS=1` turn the two off.
  Still open there: loops whose blocks are not next to each other in the
  text (the passes of `opt.rs` take a loop as a range of lines), 16-bit
  lengths in structs where a byte would do, fat pointers that are
  copied out of memory before they are used, and results of functions
  that stay functions and are needed after the next call.
* dir and sd2iec, library and compiler (2026-11-25). All fifteen
  examples: 18 469 to 17 573 bytes (3% over oscar64 `-Os`, without
  `sd2iec` 2% under), with the constant arguments at the end of this
  entry 17 391; `dir` 1543 to 1351, `sd2iec` 3308 to 2617, `diagonal`
  1311 to 1298 (2353 cycles per frame, worst 5353, start 81 582).
  - Library: `dos::Name::new(&[name])` was a loop over a list of one
    slice around a loop over its bytes, with a `usize` length and a
    checked `&buf[..len]`. Now `Name::new(s)` and `push(s)`, a `u8`
    length, `kernal::setnam_len`; `dos::command_arg(dev, cmd, arg)` for
    the commands of two parts, which `c64-sd2iec` built in a buffer of
    its own before (`dir` -194, `sd2iec` -407).
  - A view of an aggregate whose bytes already are where they go (after
    `join_locals`) was copied byte by byte onto itself: 16 times
    `lda x` / `sta x` in `sd2iec`. `Asm::copy` skips such a byte (-96).
  - `raw_eq` up to 128 bytes: a loop in place (X counts the result, Y
    the bytes) instead of two pointers, a length and `rt_memcmp`; an
    address in brackets must not get `,y` behind it (it would read as
    indirect). The call that is left makes its result with `cmp #1` /
    `lda #0` / `rol` / `eor #1` (-87).
  - `addr_locals` follows an address through pointers to slices (which
    keep their storage: only where they point to is noted), through
    wrappers like `NonNull` (only for locals that are used as a whole)
    and through `Offset` by a constant; `ptr_offset_from` of two known
    addresses in the same object is a constant.
  - `sums`: a pointer that is a known address plus a local (bytes, the
    local assigned once and not borrowed); `ptr_offset_from` of it and
    that address is the local. Sound because each of the locals on the
    way is assigned once: the assignment of the local then dominates the
    sum, and the sum its use. `narrow_locals` runs in rounds and takes
    copies of narrow locals, `integer_min` with one narrow operand, such
    a difference, and `n + 1` below a narrow local (-85 in `sd2iec`).
  - Checks 81-85 in `tests/e2e/core`, 11-12 in `tests/e2e/sd2iec`
    (`scratch` and `rename` of a file that is not there: 01 and 62).
  - The same for slices behind a reference (`&self`): `syms` in
    `addr_locals` notes for every thin pointer local that never changes
    (assigned once and not borrowed, or an argument that is not
    assigned) what it is made of, a root local plus a constant, through
    copies, casts, pointers to slices, `&(*p).field` and `Offset` by a
    constant; `sym_sums` is such a value plus a local, and `sum_diff`
    matches it against the root and constant of the other pointer. A
    length that is an extended byte may be a view without storage
    (`lazy_cand`): `narrow_locals` takes it as fitting all the same.
    No example has this pattern; `zipped` in `tests/e2e/core` goes from
    a 16-bit counter and two 16-bit additions per round to
    `lda (ptr),y` / `sta out,x`.
  - Arguments that are the same constant in every call (`const_args`
    in codegen.rs, before `drop_dead_args`). `opt::optimize` reports
    the stores to slots of other functions (`L<id>_<n>`) with the value
    of the register if it is a constant; a byte counts if every store
    in the text was reported with the same value, nothing else names
    the slot outside the function (wrappers of function pointers do, so
    those copies are out), and the function does not store to it. The
    function is optimized again with the bytes as constants at its
    entry (`Opt::entry_consts`), and `drop_dead_args` removes the stores
    for the bytes that nothing reads any more; if the function still
    reads one (through an index, as an address), the callers keep
    storing it and both sides stay right. All fifteen examples 17 573
    to 17 391 (1% over oscar64 `-Os`, without `sd2iec` 3% under): `ui`
    1109 to 1077, `hello` 379 to 367, `dir` 1351 to 1323 (oscar64
    1320), `sd2iec` 2617 to 2555, `wic64` 1877 to 1759, `uci` 2173 to
    2135; cycles within 0.1% (diagonal 2356 per frame, drawing 853 861).
    Checks 86-87. Also `bne a` / `beq b` / `a:` to `beq b` in `relax`
    (13 places in the examples).
  - `Opt::join` keeps what is equal: places with the same value number
    on every edge into a block get one number there (that of the first
    of them) instead of one each, so "A and this byte hold the same" is
    still known after the arms of an `if` or `match`. Not for a place
    that already had its own number (the analysis must end), and not
    where an edge comes from further down in the text (`ahead`): at a
    loop head it made the counter live in A, and the step became `clc`
    / `lda` / `adc #1` / `sta` in place of `inc` (julia and mandelbrot
    +6). `dir` 1323 to 1288 (oscar64 1320), `sd2iec` 2555 to 2497,
    `uci` 2135 to 2056, `wic64` 1759 to 1741; all fifteen 17 284.
    The `match` in `diagonal` (twelve arms of `lda #k` / `sta v` /
    `jmp`) does not gain: the store through a pointer that follows
    sets up the pointer first, which takes A. With the join a branch in
    `diagonal` no longer reached its target: the optimizer had only
    ever happened not to lengthen code between a short branch and its
    label. `optimize_fn` now runs `asm::widen` on its result.
  - `rt_ctx_save` saved the six bytes of the scratch pointers and
    `rt_jvec` in every interrupt, in two loops, and restored them: 250
    cycles per interrupt. Now the pointers only if the code reached
    through pointers names `zp_p` or uses runtime or assembly routines
    (`PtrCtx::zp`), `rt_jvec` only if the program text has an indirect
    call (`PtrCtx::jvec`; the dispatcher in `irq.rat` is the only user
    in the library). scroller: interrupts 983 to 533 cycles per frame
    (oscar64 826), 1410 to 1368 bytes; sprites 1207 to 1165, ui 1077 to
    1053. All fifteen 17 176 (0.2% over oscar64 `-Os`).
    A handler now starts about three raster lines earlier: the colour
    band of `sprites` (lines 50 to 66) sits three lines higher on the
    screen than before. Check 18 in `tests/e2e/lib2`: two handlers per
    frame that write through a pointer while the main program fills,
    copies and compares arrays and calls through a function pointer.
  - Not done: loops of `opt.rs` whose way back is a conditional branch
    or that hold an `asm!` block (`narrow_test` skips them: the loop
    over `MELODY` in `music`, with a step of 3 and a 16-bit counter of
    `enumerate`).
* Run time (2026-11-25): every timed row is now at or below oscar64
  `-Os`.
  - `lookup_switch` in codegen.rs: a `SwitchInt` on a byte whose targets
    (and the default, unless it is unreachable) are blocks of one
    assignment of a constant byte to the same local and a `goto` to the
    same block becomes a table `T<n>` (emitted behind the data) and an
    indexed load. The arm blocks stay in the MIR and lose their only
    way in; the optimizer drops them. From four cases, if
    `range + 16 <= 11 * cases`. diagonal: start-up 81 599 to 67 548
    cycles (oscar64 69 490), 1292 to 1267 bytes.
  - `mem::copy` of an odd number of bytes from 3 on: one byte, then two
    per round like the even case (+10 bytes per call). rasterbars 1925
    to 1837 cycles per frame (oscar64 1877), scroller 253 to 228 on
    average and 938 to 838 in the worst frame.
  - `x as u32 * K` with `x` of 16 bits and `K` below 65 536 (signed:
    below 32 768) with more than three bits set: `rt_mul16u`/`rt_mul16s`
    in place of `rt_mul32`, which found out at run time that both
    halves were small. pi 14 814 781 to 14 463 527 cycles.
  - `rt_mul16u` returns at once for a multiplier of zero (the check is
    on the way that takes eight rounds: 5 cycles for a multiplier below
    256, nothing for others). The first round of every point of the
    Mandelbrot set multiplies 0 by 0 three times: PETSCII 19 148 140 to
    17 888 993 (oscar64 18 817 639), multicolour 618 951 569 to
    588 388 518; 13 bytes.
  - Checks 88-90 in `tests/e2e/core`, 19 in `tests/e2e/lib2`.
  - An access `op base,x` where X is Y plus a constant by an addition
    that is known not to carry (`adds`, `cout` false) becomes
    `op base+k,y` (and the other way round); what made X is then dead.
    The cells loop of drawing: 40 to 26 cycles per round (1656 to 1056
    for the phase; oscar64 1106 to 1188), 2076 to 2068 bytes.
  - `drop_unread_stores` in codegen.rs, after `zp_direct` and the last
    peephole: those take loads away after the optimizer of the
    functions has run, and the store that fed them stayed. A plain
    store to a frame byte that no line of the finished program reads
    (by `p.layouts`; a name used in any way but as a plain operand
    reads all its bytes) goes. diagonal: 3 cycles per cell, start-up
    67 548 to 66 067.
  - All fifteen examples 17 200 bytes (0.4% over oscar64 `-Os`).
* Two errors found on the way (2026-11-24). The frame of an inlined
  function that the holder's code treats as its own lay where the frames
  of other functions the holder calls lie: now above them (`merged` in
  codegen.rs; `dir` and `sd2iec` lose zero-page frames by that, about 100
  bytes each). And three loop steps of `opt.rs` (`promote`, `hoist`,
  `index_loop`) did not see what a return needs when the `rts` stands
  between the blocks of the loop: a 32-bit result built in a loop was
  left in registers.
* rustc's MIR inliner: raising `-Zinline-mir-threshold` alone (functions
  without `#[inline]`) gives mixed sizes. `cargo c64` sets it to 100 and
  `-Zinline-mir-hint-threshold` (functions with `#[inline]`: the helpers of
  iterators and ranges in `core`) to 500; higher values change nothing.
  Over the release builds of `examples/` that is 3.6% less code, in dev
  builds 7.6% (`drawing`: 3555 to 2663 bytes and 1.5% faster; only
  `diagonal` grows, by 6%). With that much inlining rustc also inlines
  `panic_fmt` and the construction of its arguments, which point to
  `core::fmt`: blocks that can only end in a panic are therefore compiled
  as `jmp rt_panic` (`panic_blocks` in codegen.rs).
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
