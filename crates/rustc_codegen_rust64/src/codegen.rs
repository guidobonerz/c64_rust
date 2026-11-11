//! Whole-program translation of MIR to 6510 assembly.
//!
//! The model (see PLAN.md, section 3):
//!
//! * Every function has a static frame in the BSS: its locals and
//!   temporaries live at fixed addresses. The caller copies the arguments
//!   into the callee's argument locals (`L<callee>_<n>`) and the result out
//!   of `L<callee>_0`. Recursion is rejected.
//! * All values live in memory; operations work byte by byte through the
//!   accumulator. Memory behind pointers is reached with `(zp),y`.
//! * Constants and statics become data; pointers in them become `.word`
//!   relocations.

use std::collections::VecDeque;
use std::fmt::Write as _;

use rustc_abi::{HasDataLayout, Primitive, Size, TagEncoding, TargetDataLayout, VariantIdx, Variants};
use rustc_data_structures::fx::{FxIndexMap, FxIndexSet};
use rustc_hir::def::{DefKind, Res};
use rustc_hir::def_id::{DefId, LOCAL_CRATE};
use rustc_middle::mir::interpret::{AllocId, Allocation, GlobalAlloc, InitChunk, Scalar, alloc_range};
use rustc_middle::mono::MonoItem;
use rustc_middle::mir::visit::{MutatingUseContext, NonMutatingUseContext, PlaceContext, Visitor};
use rustc_ast::InlineAsmTemplatePiece;
use rustc_middle::mir::{
    AggregateKind, BasicBlock, BinOp, Body, CastKind, ConstOperand, ConstValue, InlineAsmOperand,
    Local, Location, NonDivergingIntrinsic, Operand, Place, ProjectionElem, Rvalue, StatementKind,
    Terminator, TerminatorKind, UnOp,
};
use rustc_middle::ty::adjustment::PointerCoercion;
use rustc_middle::ty::layout::{
    HasTyCtxt, HasTypingEnv, LayoutError, LayoutOf, LayoutOfHelpers, TyAndLayout,
};
use rustc_middle::ty::{self, Instance, InstanceKind, ShimKind, Ty, TyCtxt, TypeFoldable, TypingEnv};
use rustc_span::Span;

use crate::asm::{Addr, Asm, Val};
use crate::runtime::RUNTIME;

/// Layout queries for fully monomorphized types.
#[derive(Clone, Copy)]
pub struct Cx<'tcx> {
    pub tcx: TyCtxt<'tcx>,
}

impl HasDataLayout for Cx<'_> {
    fn data_layout(&self) -> &TargetDataLayout {
        self.tcx.data_layout()
    }
}

impl<'tcx> HasTyCtxt<'tcx> for Cx<'tcx> {
    fn tcx(&self) -> TyCtxt<'tcx> {
        self.tcx
    }
}

impl<'tcx> HasTypingEnv<'tcx> for Cx<'tcx> {
    fn typing_env(&self) -> TypingEnv<'tcx> {
        TypingEnv::fully_monomorphized()
    }
}

impl<'tcx> LayoutOfHelpers<'tcx> for Cx<'tcx> {
    type LayoutOfResult = TyAndLayout<'tcx>;

    fn handle_layout_err(&self, err: LayoutError<'tcx>, span: Span, ty: Ty<'tcx>) -> ! {
        self.tcx.dcx().span_fatal(span, format!("rust64: no layout for `{ty}`: {err:?}"))
    }
}

/// The main program context. Functions reached through a function pointer
/// (interrupt handlers, callbacks, assembly) are compiled a second time in
/// [`CTX_PTR`] with their own static frames, so a handler that interrupts
/// the main program cannot overwrite the frame of a function the main
/// program is running.
const CTX_MAIN: u8 = 0;
const CTX_PTR: u8 = 1;

/// Size of the uniform argument and result areas of indirect calls.
const RT_ARGS: u64 = 32;
const RT_RET: u64 = 16;

/// The first zero-page address for `zp` statics and the last one.
const ZP_FIRST: u64 = 0x02;
const ZP_LAST: u64 = 0x8f;

/// Bytes of BSS the runtime uses (`rt_zpsave`, the saved hardware state
/// and the runtime registers, see runtime.rat).
const RT_BSS: u64 = 160 + 87;

/// Crates that are never scanned for rat sources and exports.
const STD_CRATES: [&str; 5] = ["core", "compiler_builtins", "alloc", "std", "rustc_std_workspace_core"];

/// A block of data at a fixed address (`#[c64::at]` statics with data).
struct Fixed {
    addr: u64,
    size: u64,
    name: String,
    text: String,
}

/// The program being built: reachable functions, data, the call graph.
struct Program<'tcx> {
    tcx: TyCtxt<'tcx>,
    cx: Cx<'tcx>,
    /// Functions with the context they are compiled for.
    funcs: FxIndexSet<(Instance<'tcx>, u8)>,
    queue: VecDeque<usize>,
    allocs: FxIndexSet<AllocId>,
    statics: FxIndexSet<DefId>,
    /// Call graph: callees of each function (by index in `funcs`).
    calls: Vec<Vec<usize>>,
    /// Functions whose address is taken: id -> the arguments are a tuple
    /// (closure shims, "rust-call").
    addr_taken: FxIndexMap<usize, bool>,
    /// Argument types and result size of each compiled function.
    sigs: FxIndexMap<usize, (Vec<Ty<'tcx>>, u64)>,
    /// Per function: where the names of its frame are, and where the
    /// result and the arguments end (see `asm::forward`).
    layouts: FxIndexMap<usize, (crate::asm::FrameLayout, u64)>,
    asm: Asm,
    /// Frame symbol definitions, emitted after the `__bss` label.
    defs: String,
    bss: u64,
    /// The frame size of each compiled function (placed at the end: the
    /// frames of the most deeply nested functions go to the zero page).
    frames: Vec<(usize, u64, String)>,
    /// Assembly functions (`extern "C"` declarations): symbol ->
    /// (argument bytes, result bytes).
    foreign: FxIndexMap<String, (u64, u64)>,
    /// Rat sources of all scanned crates.
    rat: String,
    /// `symbol = label` for statics and functions exported to assembly.
    exports: String,
    /// Statics that are in the program because they are exported: their
    /// data goes with the library assembly, where it is dropped again if
    /// nothing refers to it.
    exported_statics: FxIndexSet<usize>,
    zp_next: u64,
    /// Code that initializes the zero-page statics.
    zp_init: Asm,
    fixed: Vec<Fixed>,
    /// Addresses of `at` statics without data (the program must end below
    /// them if they are in RAM).
    fixed_vars: Vec<(u64, String)>,
    /// What code compiled for [`CTX_PTR`] (interrupt handlers) uses, and
    /// the interrupt dispatcher therefore has to save: the arithmetic
    /// registers of the runtime, the registers of calls through pointers
    /// (`rt_args`, `rt_ret`), assembly functions (their argument areas).
    ptr_ctx: PtrCtx,
}

impl<'tcx> Program<'tcx> {
    fn func_id(&mut self, inst: Instance<'tcx>, ctx: u8) -> usize {
        let (id, new) = self.funcs.insert_full((inst, ctx));
        if new {
            self.queue.push_back(id);
            self.calls.push(Vec::new());
        }
        id
    }

    /// The label of a pointer to `inst`: its uniform entry, which takes the
    /// arguments from `rt_args` and returns the result in `rt_ret`.
    fn fn_ptr(&mut self, inst: Instance<'tcx>, tupled: bool) -> (usize, String) {
        let id = self.func_id(inst, CTX_PTR);
        self.addr_taken.insert(id, tupled);
        (id, format!("F{id}_ind"))
    }

    /// The label of a global allocation (constant data, static, function).
    fn alloc_label(&mut self, id: AllocId, span: Span) -> String {
        match self.tcx.global_alloc(id) {
            GlobalAlloc::Memory(_) => format!("A{}", self.allocs.insert_full(id).0),
            GlobalAlloc::Static(def_id) => {
                if self.tcx.is_foreign_item(def_id) {
                    self.tcx.dcx().span_fatal(span, "rust64: extern statics are not supported yet")
                }
                format!("S{}", self.statics.insert_full(def_id).0)
            }
            GlobalAlloc::Function { instance } => self.fn_ptr(instance, false).1,
            GlobalAlloc::VTable(..) => {
                self.tcx.dcx().span_fatal(span, "rust64: trait objects (vtables) are not supported yet")
            }
            GlobalAlloc::TypeId { .. } => {
                self.tcx.dcx().span_fatal(span, "rust64: TypeId is not supported")
            }
        }
    }

    /// A shortest call path from a root (main or a function pointer) to
    /// `id`, `id` first.
    fn call_path(&self, id: usize) -> Vec<usize> {
        let mut prev = vec![usize::MAX; self.funcs.len()];
        let mut work = VecDeque::new();
        for root in std::iter::once(0).chain(self.addr_taken.keys().copied()) {
            if prev[root] == usize::MAX {
                prev[root] = root;
                work.push_back(root);
            }
        }
        while let Some(f) = work.pop_front() {
            for &c in &self.calls[f] {
                if prev[c] == usize::MAX {
                    prev[c] = f;
                    work.push_back(c);
                }
            }
        }
        let mut path = vec![id];
        let mut f = id;
        while prev[f] != f && prev[f] != usize::MAX {
            f = prev[f];
            path.push(f);
        }
        path
    }

    fn name(&self, id: usize) -> String {
        let (inst, ctx) = self.funcs[id];
        let n = rustc_middle::ty::print::with_no_trimmed_paths!(format!("{inst}"));
        if ctx == CTX_PTR { format!("{n} (pointer copy)") } else { n }
    }
}

/// Builds the assembly of the whole program, starting at `main`.
pub fn build_program(tcx: TyCtxt<'_>) -> String {
    let mut p = Program {
        tcx,
        cx: Cx { tcx },
        funcs: FxIndexSet::default(),
        queue: VecDeque::new(),
        allocs: FxIndexSet::default(),
        statics: FxIndexSet::default(),
        calls: Vec::new(),
        addr_taken: FxIndexMap::default(),
        sigs: FxIndexMap::default(),
        layouts: FxIndexMap::default(),
        asm: Asm::default(),
        defs: String::new(),
        bss: RT_BSS,
        frames: Vec::new(),
        foreign: FxIndexMap::default(),
        rat: String::new(),
        exports: String::new(),
        exported_statics: FxIndexSet::default(),
        zp_next: ZP_FIRST,
        zp_init: Asm::default(),
        fixed: Vec::new(),
        fixed_vars: Vec::new(),
        ptr_ctx: PtrCtx::default(),
    };

    let mut main = None;
    for cgu in tcx.collect_and_partition_mono_items(()).codegen_units {
        for item in cgu.items().keys() {
            if let MonoItem::Fn(inst) = item
                && tcx.symbol_name(*inst).name == "main"
            {
                main = Some(*inst);
            }
        }
    }
    let Some(main) = main else {
        tcx.dcx().fatal(
            "rust64: no entry point; use `#[c64::main] fn main()` or declare \
             `#[unsafe(no_mangle)] pub extern \"C\" fn main()` in a `#![no_main]` crate",
        )
    };
    let main_id = p.func_id(main, CTX_MAIN);

    scan_crates(&mut p);

    while let Some(id) = p.queue.pop_front() {
        compile_fn(&mut p, id);
    }

    check_recursion(&p);

    // Data: constant allocations and statics. Emitting one can discover
    // more (pointers in data), so loop until both lists are done.
    let mut data = Asm::default();
    let mut exported_data = Asm::default();
    // Constant allocations go through `asm::prune`: those the code only
    // used as immediates are not in the program.
    let mut const_data = Asm::default();
    let (mut na, mut ns) = (0, 0);
    while na < p.allocs.len() || ns < p.statics.len() {
        while na < p.allocs.len() {
            let id = p.allocs[na];
            let alloc = tcx.global_alloc(id).unwrap_memory();
            emit_alloc(&mut p, &mut const_data, &format!("A{na}"), alloc.inner());
            na += 1;
        }
        while ns < p.statics.len() {
            if p.exported_statics.contains(&ns) {
                emit_static(&mut p, &mut exported_data, ns);
            } else {
                emit_static(&mut p, &mut data, ns);
            }
            ns += 1;
        }
    }
    // Pointer entries can reach new functions only through data, which is
    // done; the entries themselves call already compiled functions.
    let wrappers = fn_ptr_entries(&mut p);

    // Frames. Functions that are never active at the same time share
    // memory: a frame lies above the frames of everything the function
    // calls (a leaf is at the bottom), so the frames of a call chain never
    // overlap. Each function pointer target (interrupt handlers) gets a
    // region of its own above the main program's: a handler may interrupt
    // anything. What fits into the rest of the zero page goes there: the
    // leaves first, which are called most.
    let frames = std::mem::take(&mut p.frames);
    let n = p.funcs.len();
    let mut size = vec![0u64; n];
    for (id, sz, _) in &frames {
        size[*id] = *sz;
    }
    let mut off: Vec<Option<u64>> = vec![None; n];
    fn place(p: &Program<'_>, f: usize, base: u64, size: &[u64], off: &mut Vec<Option<u64>>) -> u64 {
        if let Some(o) = off[f] {
            return o;
        }
        let mut o = base;
        for &c in &p.calls[f] {
            o = o.max(place(p, c, base, size, off) + size[c]);
        }
        off[f] = Some(o);
        o
    }
    let mut top = 0;
    for root in std::iter::once(main_id).chain(p.addr_taken.keys().copied()) {
        let base = top;
        place(&p, root, base, &size, &mut off);
        top = (0..n).filter_map(|f| off[f].map(|o| o + size[f])).max().unwrap_or(0);
    }
    // The labels of zero-page frames are defined before the code, so that
    // the assembler picks zero-page addressing.
    let zp_avail = ZP_LAST + 1 - p.zp_next;
    let mut zp_frames = String::new();
    let mut zp_ids = FxIndexSet::default();
    // Where the frames in the zero page end.
    let mut zp_frame_ends: Vec<(usize, u64)> = Vec::new();
    for (id, sz, defs) in frames {
        let o = off[id].unwrap_or_else(|| {
            // not reachable from a root: its own space
            top += sz;
            top - sz
        });
        if sz > 0 && o + sz <= zp_avail {
            zp_ids.insert(id);
            zp_frame_ends.push((id, p.zp_next + o + sz));
            let _ = writeln!(zp_frames, "F{id}f = ${:02x}", p.zp_next + o);
            zp_frames.push_str(&defs);
        } else {
            let _ = writeln!(p.defs, "F{id}f = __bss + {}", p.bss + o);
            p.defs.push_str(&defs);
        }
    }
    p.bss += top;

    // The library assembly: the runtime, the rat sources and the generated
    // helpers. Only what the program refers to is kept (asm::prune).
    let mut lib = String::new();
    lib.push_str(RUNTIME);
    lib.push_str(&p.rat);
    lib.push_str("\nrt_loc: .res 16                  ; dummy core::panic::Location\n");
    lib.push_str("\n; ---- exported to assembly ----\n");
    lib.push_str(&p.exports);
    lib.push_str(&exported_data.text);
    lib.push_str("\n; ---- constants ----\n");
    lib.push_str(&const_data.text);
    lib.push_str(&ctx_save(p.ptr_ctx));

    // Arguments nobody reads are not passed; what that frees in the
    // callers is cleaned up function by function.
    {
        let program = format!("{}{}{}", p.asm.text, wrappers, data.text);
        let kept = crate::asm::prune(&lib, &program);
        let code = crate::asm::drop_dead_args(&p.asm.text, &[&wrappers, &data.text, &kept], &|name| {
            // F<id>v<n>, F<id>t, L<id>_<n>
            let id = name
                .strip_prefix('F')
                .and_then(|r| r.split_once(['v', 't']))
                .or_else(|| name.strip_prefix('L').and_then(|r| r.split_once('_')))
                .and_then(|(id, _)| id.parse::<usize>().ok())?;
            let &(o, n) = p.layouts.get(&id)?.0.get(name)?;
            Some((id, o, n))
        });
        let mut text = String::with_capacity(code.len());
        let mut chunk = String::new();
        let mut cur: Option<usize> = None;
        let flush = |p: &mut Program<'_>, chunk: &mut String, cur: Option<usize>, text: &mut String| {
            match cur {
                Some(id) => text.push_str(&optimize_fn(p, chunk, id)),
                None => text.push_str(chunk),
            }
            chunk.clear();
        };
        for line in code.lines() {
            if let Some(id) = line
                .strip_prefix('F')
                .and_then(|r| r.strip_suffix(':'))
                .and_then(|r| r.parse::<usize>().ok())
            {
                flush(&mut p, &mut chunk, cur, &mut text);
                cur = Some(id);
            }
            chunk.push_str(line);
            chunk.push('\n');
        }
        flush(&mut p, &mut chunk, cur, &mut text);
        p.asm.text = text;
    }

    // Argument and result areas of the assembly functions: one block, so
    // that interrupt dispatchers can save it (rt_ctx_save). Only for the
    // functions the program uses: the code calls them, or assembly that is
    // kept refers to their areas.
    let used_areas: FxIndexSet<String> = {
        let program = format!("{}{}{}", p.asm.text, wrappers, data.text);
        let kept = crate::asm::prune(&lib, &program);
        let mut used = FxIndexSet::default();
        for text in [&program, &kept] {
            for line in text.lines() {
                let line = line.split(';').next().unwrap_or("");
                for w in line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
                    if w.ends_with("__args") || w.ends_with("__res") {
                        used.insert(w.to_owned());
                    }
                }
            }
        }
        used
    };
    let _ = writeln!(p.defs, "rt_fsave = __bss + {}", p.bss);
    for (sym, &(args, res)) in &p.foreign {
        for (suffix, n) in [("args", args), ("res", res)] {
            if n > 0 && used_areas.contains(&format!("{sym}__{suffix}")) {
                let _ = writeln!(p.defs, "{sym}__{suffix} = __bss + {}", p.bss);
                p.bss += n;
            }
        }
    }
    let _ = writeln!(p.defs, "rt_fsave_end = __bss + {}", p.bss);
    let _ = writeln!(p.defs, "rt_ctxbuf = __bss + {}", p.bss);
    p.bss += 256 + 8;


    let mut out = String::new();
    let _ = writeln!(out, "\nrt_main = F{main_id}");
    let zp_init = !p.zp_init.text.is_empty();
    if zp_init {
        out.push_str("\n; ---- zero-page statics ----\nrt_zpinit:\n");
        out.push_str(&p.zp_init.text);
        out.push_str("        rts\n");
    }
    // Pointers in zero-page frames are dereferenced directly.
    let code = crate::asm::zp_direct(&p.asm.text, |name| {
        // F<id>v<n>[+off] or F<id>t+off
        name.strip_prefix('F')
            .and_then(|r| r.split_once(['v', 't']))
            .and_then(|(id, _)| id.parse::<usize>().ok())
            .is_some_and(|id| zp_ids.contains(&id))
    });
    // Branch distances with the real sizes: frames in the zero page are
    // known now.
    let code = crate::asm::relax_with(
        &code,
        &|op, arg| {
            crate::asm::line_size(op, arg, &|name| {
                name.starts_with("zp_")
                    || name
                        .strip_prefix('F')
                        .and_then(|r| r.split_once(['v', 't']))
                        .or_else(|| name.strip_prefix('L').and_then(|r| r.split_once('_')))
                        .and_then(|(id, _)| id.parse::<usize>().ok())
                        .is_some_and(|id| zp_ids.contains(&id))
            })
        },
        120,
    );
    out.push_str(&code);
    out.push_str(&wrappers);
    out.push_str("\n; ---- data ----\n");
    out.push_str(&data.text);
    out.push_str("\n; ---- bss (not in the file) ----\n__bss:\n");
    out.push_str(&p.defs);
    let _ = writeln!(out, "__bss_end = __bss + {}", p.bss);
    out.push_str(".assert __bss_end <= $a000, \"rust64: program and its data do not fit below $a000\"\n");
    out.push_str(".assert rt_fsave_end - rt_fsave + rt_save_end - rt_save <= 256, \"rust64: too many assembly function arguments\"\n");
    for (addr, what) in &p.fixed_vars {
        if *addr >= 0x0801 {
            let _ = writeln!(
                out,
                ".assert __bss_end <= ${addr:04x}, \"rust64: the program overlaps {what} at ${addr:04x}\""
            );
        }
    }
    p.fixed.sort_by_key(|f| f.addr);
    for (i, f) in p.fixed.iter().enumerate() {
        if i > 0 {
            let q = &p.fixed[i - 1];
            if q.addr + q.size > f.addr {
                tcx.dcx().fatal(format!(
                    "rust64: {} at ${:04x} overlaps {} at ${:04x}",
                    f.name, f.addr, q.name, q.addr
                ));
            }
        }
        let _ = write!(out, "\n; ---- {} at ${:04x} ----\n", f.name, f.addr);
        let _ = writeln!(
            out,
            ".assert __bss_end <= ${:04x}, \"rust64: the program overlaps {} at ${:04x}\"",
            f.addr, f.name, f.addr
        );
        let _ = writeln!(out, ".org ${:04x}", f.addr);
        out.push_str(&f.text);
    }
    // Start and exit: only what this program needs. What it needs shows in
    // the program and the library routines it keeps.
    let kept = crate::asm::prune(&lib, &out);
    let all = format!("{kept}{out}");
    let exit_used = all.lines().any(|l| {
        let l = l.split(';').next().unwrap_or("");
        l.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).any(|t| t == "rt_exit")
    });
    let hw = crate::asm::touches_hw(&all)
        || p.fixed_vars.iter().any(|(a, _)| *a < 2 || (0x0314..0x031a).contains(a) || (0xd000..0xe000).contains(a));
    // The used part of the zero page: the statics, and the frames the
    // code refers to (an optimized function may not need its frame).
    let refers = |id: usize| {
        let (v, t, l) = (format!("F{id}v"), format!("F{id}t"), format!("L{id}_"));
        all.lines().any(|line| {
            line.starts_with(' ')
                && line
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .any(|w| w.starts_with(&v) || w.starts_with(&t) || w.starts_with(&l))
        })
    };
    let zp_end = zp_frame_ends
        .iter()
        .filter(|(id, _)| refers(*id))
        .map(|&(_, end)| end)
        .fold(p.zp_next, u64::max);
    let start = start_exit(zp_end - ZP_FIRST, hw, exit_used, zp_init);
    let lib = crate::asm::prune(&lib, &format!("{start}{out}"));
    format!(
        "; generated by rust64\n.basicupstart rt_start\n\n; ---- frames in the zero page ----\n\
         {zp_frames}\n{start}{lib}{out}"
    )
}

/// The start and exit code of the program.
///
/// * `zp`: the bytes of the zero page from `$02` that hold zero-page
///   statics and frames. They are BASIC's: saved and cleared at the
///   start, restored at the exit.
/// * `hw`: the program can change the interrupt setup, the memory
///   configuration, the VIC mode or bank or the SID
///   ([`crate::asm::touches_hw`]): saved at the start (what cannot be set
///   to a known value), restored at the exit.
/// * `exit_used`: something jumps to `rt_exit` (`sys::exit`, a panic), from
///   any depth of calls: the stack pointer is saved.
/// * `zp_init`: there are zero-page statics to initialize.
///
/// A program that needs none of this starts directly with `main`, whose
/// `rts` returns to BASIC.
fn start_exit(zp: u64, hw: bool, exit_used: bool, zp_init: bool) -> String {
    let mut a = Asm::default();
    a.raw("\n; ---- start and exit ----");
    if zp == 0 && !hw && !exit_used && !zp_init {
        a.raw("rt_start = rt_main");
        return a.text;
    }
    a.label("rt_start");
    if zp > 0 {
        a.ins(format!("ldx #{zp}"));
        a.raw("@save:");
        a.ins("lda $01,x");
        a.ins("sta rt_zpsave-1,x");
        a.ins("lda #0");
        a.ins("sta $01,x");
        a.ins("dex");
        a.ins("bne @save");
    }
    if exit_used {
        a.ins("tsx");
        a.ins("stx rt_sp");
    }
    const SAVED: [(&str, &str); 7] = [
        ("$0314", "rt_irqsave"),
        ("$0315", "rt_irqsave+1"),
        ("$01", "rt_port"),
        ("$dd00", "rt_dd00"),
        ("$d018", "rt_d018"),
        ("$d011", "rt_d011"),
        ("$d016", "rt_d016"),
    ];
    if hw {
        for (reg, at) in SAVED {
            a.ins(format!("lda {reg}"));
            a.ins(format!("sta {at}"));
        }
        a.ins("lda $d015");
        a.ins("sta rt_d015");
        a.ins("cld");
    }
    if zp_init {
        a.ins("jsr rt_zpinit");
    }
    if zp == 0 && !hw && !exit_used {
        a.ins("jmp rt_main");
        return a.text;
    }
    a.ins("jsr rt_main");
    // Back to BASIC from anywhere: restore the IRQ vector, the CIA
    // interrupt, the memory configuration, the VIC (except the colours, so
    // the program's output stays readable), silence the SID, restore the
    // zero page.
    a.label("rt_exit");
    if hw {
        a.ins("sei");
    }
    if exit_used {
        a.ins("ldx rt_sp");
        a.ins("txs");
    }
    if hw {
        for (i, (reg, at)) in SAVED.iter().enumerate() {
            a.ins(format!("lda {at}"));
            a.ins(format!("sta {reg}"));
            if i == 1 {
                a.ins("lda #0");
                a.ins("sta $d01a");
                a.ins("lda #$ff");
                a.ins("sta $d019");
                a.ins("lda #$81");
                a.ins("sta $dc0d");
            }
        }
        a.ins("lda rt_d015");
        a.ins("sta $d015");
        a.ins("lda #0");
        for reg in ["$d404", "$d40b", "$d412", "$d418"] {
            a.ins(format!("sta {reg}"));
        }
        a.ins("cli");
    }
    if zp > 0 {
        a.ins(format!("ldx #{zp}"));
        a.raw("@rest:");
        a.ins("lda rt_zpsave-1,x");
        a.ins("sta $01,x");
        a.ins("dex");
        a.ins("bne @rest");
    }
    a.ins("rts");
    a.text
}

/// Parses an address of a `link_section = "at:<addr>"`: `$d020`, `0xd020`
/// or decimal.
fn parse_addr(s: &str) -> Option<u64> {
    let s = s.trim();
    let v = if let Some(h) = s.strip_prefix('$').or_else(|| s.strip_prefix("0x")) {
        u64::from_str_radix(&h.replace('_', ""), 16).ok()?
    } else {
        s.replace('_', "").parse().ok()?
    };
    (v <= 0xffff).then_some(v)
}

/// Emits static `S<ns>`: as data, at a fixed address (`at:`), or in the
/// zero page (`zp`).
fn emit_static(p: &mut Program<'_>, data: &mut Asm, ns: usize) {
    let tcx = p.tcx;
    let def_id = p.statics[ns];
    let label = format!("S{ns}");
    let name = rustc_middle::ty::print::with_no_trimmed_paths!(tcx.def_path_str(def_id));
    let Ok(alloc) = tcx.eval_static_initializer(def_id) else {
        tcx.dcx().fatal(format!("rust64: cannot evaluate static {name}"))
    };
    let a = alloc.inner();
    let size = a.len() as u64;
    let section = tcx.codegen_fn_attrs(def_id).link_section.map(|s| s.as_str().to_owned());
    match section.as_deref() {
        Some(s) if s.starts_with("at:") => {
            let Some(addr) = parse_addr(&s[3..]) else {
                tcx.dcx().fatal(format!("rust64: static {name}: bad address in `{s}`"))
            };
            let uninit = a
                .init_mask()
                .range_as_init_chunks(alloc_range(Size::ZERO, a.size()))
                .all(|c| matches!(c, InitChunk::Uninit(_)));
            if uninit {
                // Placed, not loaded: `MaybeUninit::uninit()` statics.
                let _ = writeln!(p.defs, "{label} = ${addr:04x}            ; {name}");
                p.fixed_vars.push((addr, name));
            } else {
                if addr < 0x0801 || addr + size > 0x10000 {
                    tcx.dcx().fatal(format!(
                        "rust64: static {name} at ${addr:04x} has data, but the program file \
                         cannot load data there (use `MaybeUninit::uninit()` for memory that \
                         is only placed)"
                    ))
                }
                let mut block = Asm::default();
                emit_alloc(p, &mut block, &label, a);
                p.fixed.push(Fixed { addr, size, name, text: block.text });
            }
        }
        Some("zp") => {
            if p.zp_next + size > ZP_LAST + 1 {
                tcx.dcx().fatal(format!("rust64: the zero page is full: {name} does not fit"))
            }
            let addr = p.zp_next;
            p.zp_next += size;
            let _ = writeln!(p.defs, "{label} = ${addr:02x}             ; {name}");
            if !a.provenance().ptrs().is_empty() {
                tcx.dcx().fatal(format!("rust64: zero-page static {name} cannot hold pointers yet"))
            }
            let bytes = a.inspect_with_uninit_and_ptr_outside_interpreter(0..a.len());
            for (i, b) in bytes.iter().enumerate() {
                if *b != 0 {
                    p.zp_init.ins(format!("lda #${b:02x}"));
                    p.zp_init.ins(format!("sta {label}+{i}"));
                }
            }
        }
        _ => {
            data.comment(format!("static {name}"));
            emit_alloc(p, data, &label, a);
        }
    }
}

/// The uniform entries of the functions whose address is taken: copy the
/// arguments from `rt_args` into the frame, call, copy the result to
/// `rt_ret`.
fn fn_ptr_entries(p: &mut Program<'_>) -> String {
    let mut a = Asm::default();
    let ids: Vec<(usize, bool)> = p.addr_taken.iter().map(|(&k, &v)| (k, v)).collect();
    for (id, tupled) in ids {
        let (args, ret) = p.sigs[&id].clone();
        a.raw("");
        a.comment(format!("pointer entry of {}", p.name(id)));
        a.label(&format!("F{id}_ind"));
        let mut off = 0;
        let n = args.len();
        for (i, ty) in args.iter().enumerate() {
            let l = p.cx.layout_of(*ty);
            let dst = format!("L{id}_{}", i + 1);
            if tupled && i == n - 1 && i > 0 {
                for f in 0..l.fields.count() {
                    let fo = l.fields.offset(f).bytes();
                    let fs = l.field(&p.cx, f).size.bytes();
                    a.copy(&Addr::Abs(format!("{dst}+{fo}")), &Val::Mem(Addr::Abs(format!("rt_args+{off}"))), fs);
                    off += fs;
                }
            } else {
                let s = l.size.bytes();
                a.copy(&Addr::Abs(dst), &Val::Mem(Addr::Abs(format!("rt_args+{off}"))), s);
                off += s;
            }
        }
        if off > RT_ARGS {
            p.tcx.dcx().fatal(format!(
                "rust64: {} takes {off} bytes of arguments; calls through pointers allow {RT_ARGS}",
                p.name(id)
            ))
        }
        if ret > RT_RET {
            p.tcx.dcx().fatal(format!(
                "rust64: {} returns {ret} bytes; calls through pointers allow {RT_RET}",
                p.name(id)
            ))
        }
        if ret == 0 {
            a.ins(format!("jmp F{id}"));
        } else {
            a.ins(format!("jsr F{id}"));
            a.copy(&Addr::Abs("rt_ret".into()), &Val::Mem(Addr::Abs(format!("L{id}_0"))), ret);
            a.ins("rts");
        }
    }
    a.text
}

/// Scans the program's crates (not the standard library) for
/// * rat sources (`#[link_section = "rat"]` byte-array statics, see
///   `c64::rat_source!`),
/// * assembly function declarations: their argument/result areas are
///   defined even when they are not called, because rat code refers to them,
/// * statics and functions exported by name (`#[unsafe(no_mangle)]`,
///   `export_name`): they are included and their symbol is defined, so
///   assembly can use them. Exported functions are called with the pointer
///   convention (`rt_args`/`rt_ret`).
fn scan_crates(p: &mut Program<'_>) {
    let tcx = p.tcx;
    let mut crates = vec![LOCAL_CRATE];
    crates.extend(
        tcx.crates(())
            .iter()
            .copied()
            .filter(|&c| !STD_CRATES.contains(&tcx.crate_name(c).as_str())),
    );
    for krate in crates {
        let mut seen = FxIndexSet::default();
        let mut mods = vec![krate.as_def_id()];
        while let Some(m) = mods.pop() {
            let children = match m.as_local() {
                Some(local) => tcx.module_children_local(local),
                None => tcx.module_children(m),
            };
            for child in children {
                if !child.reexport_chain.is_empty() {
                    continue;
                }
                let Res::Def(kind, id) = child.res else { continue };
                if id.krate != krate || !seen.insert(id) {
                    continue;
                }
                match kind {
                    DefKind::Mod => mods.push(id),
                    DefKind::Fn if tcx.is_foreign_item(id) => {
                        let sig = tcx.instantiate_bound_regions_with_erased(
                            tcx.fn_sig(id).instantiate_identity().skip_normalization(),
                        );
                        let args: u64 =
                            sig.inputs().iter().map(|&t| p.cx.layout_of(t).size.bytes()).sum();
                        let res = p.cx.layout_of(sig.output()).size.bytes();
                        let sym = tcx.symbol_name(Instance::mono(tcx, id)).name.to_owned();
                        let e = p.foreign.entry(sym).or_default();
                        e.0 = e.0.max(args);
                        e.1 = e.1.max(res);
                    }
                    DefKind::Fn
                        if tcx.codegen_fn_attrs(id).contains_extern_indicator()
                            && tcx.generics_of(id).count() == 0
                            && tcx.lang_items().panic_impl() != Some(id) =>
                    {
                        let inst = Instance::mono(tcx, id);
                        let sym = tcx.symbol_name(inst).name.to_owned();
                        if sym != "main" {
                            let (_, label) = p.fn_ptr(inst, false);
                            let _ = writeln!(p.exports, "{sym} = {label}");
                        }
                    }
                    DefKind::Static { .. } => {
                        let attrs = tcx.codegen_fn_attrs(id);
                        if attrs.link_section.is_some_and(|s| s.as_str() == "rat") {
                            rat_source(p, id);
                        } else if attrs.contains_extern_indicator() {
                            let sym = tcx.symbol_name(Instance::mono(tcx, id)).name.to_owned();
                            let n = p.statics.insert_full(id).0;
                            p.exported_statics.insert(n);
                            let _ = writeln!(p.exports, "{sym} = S{n}");
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn rat_source(p: &mut Program<'_>, id: DefId) {
    let tcx = p.tcx;
    let name = rustc_middle::ty::print::with_no_trimmed_paths!(tcx.def_path_str(id));
    let Ok(alloc) = tcx.eval_static_initializer(id) else {
        tcx.dcx().fatal(format!("rust64: cannot evaluate rat source {name}"))
    };
    let a = alloc.inner();
    let bytes = a.inspect_with_uninit_and_ptr_outside_interpreter(0..a.len());
    let Ok(text) = std::str::from_utf8(bytes) else {
        tcx.dcx().fatal(format!("rust64: rat source {name} is not UTF-8"))
    };
    let _ = writeln!(p.rat, "\n; ---- rat source {name} ----");
    p.rat.push_str(text);
    if !text.ends_with('\n') {
        p.rat.push('\n');
    }
}

fn check_recursion(p: &Program<'_>) {
    // 0 = new, 1 = on the stack, 2 = done
    fn visit(p: &Program<'_>, n: usize, state: &mut [u8], stack: &mut Vec<usize>) {
        state[n] = 1;
        stack.push(n);
        for &c in &p.calls[n] {
            if state[c] == 1 {
                let start = stack.iter().position(|&x| x == c).unwrap();
                let cycle: Vec<String> = stack[start..].iter().map(|&f| p.name(f)).collect();
                p.tcx.dcx().fatal(format!(
                    "rust64: recursion is not supported (static frames): {} -> {}",
                    cycle.join(" -> "),
                    p.name(c)
                ));
            }
            if state[c] == 0 {
                visit(p, c, state, stack);
            }
        }
        stack.pop();
        state[n] = 2;
    }
    let mut state = vec![0u8; p.funcs.len()];
    for n in 0..p.funcs.len() {
        if state[n] == 0 {
            visit(p, n, &mut state, &mut Vec::new());
        }
    }
}

fn emit_alloc(p: &mut Program<'_>, out: &mut Asm, label: &str, alloc: &Allocation) {
    let len = alloc.len();
    let bytes = alloc.inspect_with_uninit_and_ptr_outside_interpreter(0..len).to_vec();
    let ptrs: Vec<(usize, AllocId)> = alloc
        .provenance()
        .ptrs()
        .iter()
        .map(|&(off, prov)| (off.bytes() as usize, prov.alloc_id()))
        .collect();
    out.label(label);
    let mut i = 0;
    let mut run: Vec<String> = Vec::new();
    let flush = |run: &mut Vec<String>, out: &mut Asm| {
        for chunk in run.chunks(16) {
            out.ins(format!(".byte {}", chunk.join(",")));
        }
        run.clear();
    };
    while i < len {
        if let Some(&(_, target)) = ptrs.iter().find(|(o, _)| *o == i) {
            flush(&mut run, out);
            let off = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
            let l = p.alloc_label(target, rustc_span::DUMMY_SP);
            out.ins(format!(".word {l}+{off}"));
            i += 2;
        } else {
            run.push(format!("${:02x}", bytes[i]));
            i += 1;
        }
    }
    flush(&mut run, out);
}

fn monomorphize_in<'tcx, T: TypeFoldable<TyCtxt<'tcx>>>(
    tcx: TyCtxt<'tcx>,
    inst: Instance<'tcx>,
    v: T,
) -> T {
    inst.instantiate_mir_and_normalize_erasing_regions(
        tcx,
        TypingEnv::fully_monomorphized(),
        ty::EarlyBinder::bind(tcx, v),
    )
}

/// The value of a constant integer operand.
fn const_operand<'tcx>(tcx: TyCtxt<'tcx>, inst: Instance<'tcx>, op: &Operand<'tcx>) -> Option<u128> {
    match op {
        Operand::RuntimeChecks(c) => Some(c.value(tcx.sess) as u128),
        Operand::Constant(c) => {
            let cv = monomorphize_in(tcx, inst, c.const_);
            match cv.eval(tcx, TypingEnv::fully_monomorphized(), c.span) {
                Ok(ConstValue::Scalar(Scalar::Int(int))) => Some(int.to_bits(int.size())),
                _ => None,
            }
        }
        _ => None,
    }
}

/// For each local of `mir`: the constant it always holds, if every write
/// to it stores the same constant (directly or as the result of a call to
/// a function that always returns that constant) and it is never borrowed.
/// This folds `ub_checks` and similar configuration tests.
fn const_locals<'tcx>(
    tcx: TyCtxt<'tcx>,
    inst: Instance<'tcx>,
    mir: &'tcx Body<'tcx>,
    depth: u32,
) -> Vec<Option<u128>> {
    #[derive(Clone, Copy, PartialEq)]
    enum S {
        Unset,
        Const(u128),
        Var,
    }
    let n = mir.local_decls.len();
    let mut st = vec![S::Unset; n];
    for a in 1..=mir.arg_count {
        st[a] = S::Var;
    }
    let set = |st: &mut Vec<S>, l: Local, v: Option<u128>| {
        let i = l.as_usize();
        st[i] = match (st[i], v) {
            (S::Unset, Some(v)) => S::Const(v),
            (S::Const(a), Some(b)) if a == b => S::Const(a),
            _ => S::Var,
        };
    };
    for data in mir.basic_blocks.iter() {
        for stmt in &data.statements {
            if let StatementKind::Assign(b) = &stmt.kind {
                let (place, rv) = &**b;
                if let Rvalue::Ref(_, _, p) | Rvalue::RawPtr(_, p) = rv {
                    st[p.local.as_usize()] = S::Var;
                }
                let v = match rv {
                    Rvalue::Use(op, _) if place.projection.is_empty() => {
                        const_operand(tcx, inst, op)
                    }
                    _ => None,
                };
                set(&mut st, place.local, v);
            }
        }
        if let TerminatorKind::Call { func, destination, .. } = &data.terminator().kind {
            let mut v = None;
            if depth > 0 && destination.projection.is_empty() {
                let fty = monomorphize_in(tcx, inst, func.ty(mir, tcx));
                if let ty::FnDef(def_id, args) = *fty.kind()
                    && let Ok(Some(callee)) = Instance::try_resolve(
                        tcx,
                        TypingEnv::fully_monomorphized(),
                        def_id,
                        args.skip_binder(),
                    )
                    && let InstanceKind::Item(cdef) = callee.def
                    && !tcx.is_foreign_item(cdef)
                    && tcx.is_mir_available(cdef)
                    && tcx.intrinsic(cdef).is_none()
                {
                    let cmir = tcx.instance_mir(callee.def);
                    v = const_locals(tcx, callee, cmir, depth - 1)[0];
                }
            }
            set(&mut st, destination.local, v);
        }
    }
    st.into_iter().map(|s| if let S::Const(v) = s { Some(v) } else { None }).collect()
}

/// How the locals of a function are used.
struct Uses<'tcx> {
    tcx: TyCtxt<'tcx>,
    /// Assignments (a write through a field counts twice: never "one def").
    defs: Vec<u32>,
    def_locs: Vec<Vec<Location>>,
    /// Reads of any kind (also as the base of a dereference).
    uses: Vec<u32>,
    /// Reads of the whole local as an operand, and where the last one is.
    plain: Vec<u32>,
    plain_loc: Vec<Option<Location>>,
    /// Uses as the index of an array access.
    index: Vec<u32>,
    /// Reads of a field (of a field ...) of the local as an operand.
    field_reads: Vec<u32>,
    /// Uses as the pointer of a dereference.
    derefs: Vec<u32>,
    /// Where the plain and index uses are.
    use_locs: Vec<Vec<Location>>,
    /// Reads of the local's value other than dereferencing it.
    value_uses: Vec<u32>,
    /// A reference to the local (or a part of it) is taken.
    borrowed: Vec<bool>,
}

impl<'tcx> Uses<'tcx> {
    fn new(tcx: TyCtxt<'tcx>, mir: &Body<'tcx>) -> Uses<'tcx> {
        let n = mir.local_decls.len();
        let mut u = Uses {
            tcx,
            defs: vec![0; n],
            def_locs: vec![Vec::new(); n],
            uses: vec![0; n],
            plain: vec![0; n],
            plain_loc: vec![None; n],
            index: vec![0; n],
            field_reads: vec![0; n],
            derefs: vec![0; n],
            use_locs: vec![Vec::new(); n],
            value_uses: vec![0; n],
            borrowed: vec![false; n],
        };
        u.visit_body(mir);
        u
    }
}

impl<'tcx> Visitor<'tcx> for Uses<'tcx> {
    fn visit_place(&mut self, place: &Place<'tcx>, ctx: PlaceContext, loc: Location) {
        let l = place.local.as_usize();
        let deref = place.projection.first() == Some(&ProjectionElem::Deref);
        if deref && !matches!(ctx, PlaceContext::NonUse(_)) {
            self.derefs[l] += 1;
            self.use_locs[l].push(loc);
        }
        for elem in place.projection.iter() {
            if let ProjectionElem::Index(i) = elem {
                self.uses[i.as_usize()] += 1;
                self.value_uses[i.as_usize()] += 1;
                self.index[i.as_usize()] += 1;
                self.use_locs[i.as_usize()].push(loc);
            }
        }
        match ctx {
            PlaceContext::NonUse(_) => {}
            PlaceContext::MutatingUse(MutatingUseContext::Borrow | MutatingUseContext::RawBorrow)
            | PlaceContext::NonMutatingUse(
                NonMutatingUseContext::SharedBorrow
                | NonMutatingUseContext::FakeBorrow
                | NonMutatingUseContext::RawBorrow,
            ) => {
                if deref {
                    self.uses[l] += 1;
                } else {
                    self.borrowed[l] = true;
                }
            }
            PlaceContext::MutatingUse(_) => {
                if deref {
                    self.uses[l] += 1;
                } else {
                    self.defs[l] += if place.projection.is_empty() { 1 } else { 2 };
                    self.def_locs[l].push(loc);
                }
            }
            PlaceContext::NonMutatingUse(c) => {
                self.uses[l] += 1;
                if !deref {
                    self.value_uses[l] += 1;
                    if matches!(c, NonMutatingUseContext::Copy | NonMutatingUseContext::Move) {
                        if place.projection.is_empty() {
                            self.plain[l] += 1;
                            self.plain_loc[l] = Some(loc);
                            self.use_locs[l].push(loc);
                        } else if place.projection.iter().all(|e| {
                            matches!(
                                e,
                                ProjectionElem::Field(..)
                                    | ProjectionElem::Downcast(..)
                                    | ProjectionElem::OpaqueCast(..)
                            )
                        }) {
                            self.field_reads[l] += 1;
                            self.use_locs[l].push(loc);
                        }
                    }
                }
            }
        }
    }

    fn visit_local(&mut self, local: Local, ctx: PlaceContext, loc: Location) {
        // Locals outside places (e.g. `StorageLive`) are no uses; anything
        // else is treated like a place.
        if !matches!(ctx, PlaceContext::NonUse(_)) {
            self.visit_place(&Place::from(local), ctx, loc);
        }
    }

    fn visit_terminator(&mut self, term: &Terminator<'tcx>, loc: Location) {
        match &term.kind {
            // The message operands of an assert are never evaluated.
            TerminatorKind::Assert { cond, .. } => self.visit_operand(cond, loc),
            TerminatorKind::Call { func, args, .. } => {
                self.super_terminator(term, loc);
                // The pointer of a volatile access is only dereferenced.
                if let Some((def_id, _)) = func.const_fn_def()
                    && let Some(i) = self.tcx.intrinsic(def_id)
                    && matches!(
                        i.name.as_str(),
                        "volatile_load" | "volatile_store" | "unaligned_volatile_load"
                            | "unaligned_volatile_store" | "read_via_copy" | "write_via_move"
                    )
                    && let Some(a) = args.first()
                    && let Operand::Copy(p) | Operand::Move(p) = &a.node
                    && p.projection.is_empty()
                {
                    self.value_uses[p.local.as_usize()] -= 1;
                }
            }
            _ => self.super_terminator(term, loc),
        }
    }
}

/// Where a fused comparison goes.
#[derive(Clone, Copy, PartialEq)]
enum Target {
    Block(BasicBlock),
    Panic,
}

/// Translation of one function.
struct FnCx<'a, 'tcx> {
    p: &'a mut Program<'tcx>,
    id: usize,
    /// The context this copy of the function is compiled for.
    ctx: u8,
    inst: Instance<'tcx>,
    mir: &'tcx Body<'tcx>,
    /// Locals whose storage is used (the others get none).
    touched: std::cell::RefCell<Vec<bool>>,
    /// Temporaries: current and maximum size.
    temp_cur: u64,
    temp_max: u64,
    span: Span,
    /// Locals known to hold a constant.
    consts: Vec<Option<u128>>,
    /// Pointer locals that always hold the same known address (an
    /// assembler expression): dereferencing them is a direct access.
    addrs: Vec<Option<String>>,
    uses: Uses<'tcx>,
    /// Locals that are not stored: single-use temporaries that are a copy,
    /// zero-extension or truncation of another value (see `lazy_locals`).
    lazy_cand: Vec<bool>,
    /// The defining rvalue of each lazy local.
    lazy_def: Vec<Option<&'tcx Rvalue<'tcx>>>,
}

fn compile_fn(p: &mut Program<'_>, id: usize) {
    let tcx = p.tcx;
    let (inst, ctx) = p.funcs[id];
    let def_id = inst.def_id();
    if let InstanceKind::Item(_) = inst.def {
        if tcx.is_foreign_item(def_id) {
            tcx.dcx().span_fatal(
                tcx.def_span(def_id),
                "rust64: extern functions (assembly functions) are not supported yet",
            )
        }
        if !tcx.is_mir_available(def_id) {
            tcx.dcx().fatal(format!(
                "rust64: no MIR for `{}`; build every crate with -Zalways-encode-mir \
                 (cargo c64 does this)",
                p.name(id)
            ))
        }
    }
    let mir = tcx.instance_mir(inst.def);
    if std::env::var_os("RUST64_TRACE").is_some() {
        let mut buf = Vec::new();
        let _ = rustc_middle::mir::pretty::MirWriter::new(tcx).write_mir_fn(mir, &mut buf);
        eprintln!("rust64: F{id} = {}
{}", p.name(id), String::from_utf8_lossy(&buf));
    }
    let mut f = FnCx {
        p,
        id,
        ctx,
        inst,
        mir,
        touched: std::cell::RefCell::new(vec![false; mir.local_decls.len()]),
        temp_cur: 0,
        temp_max: 0,
        span: mir.span,
        consts: const_locals(tcx, inst, mir, 3),
        addrs: Vec::new(),
        uses: Uses::new(tcx, mir),
        lazy_cand: Vec::new(),
        lazy_def: vec![None; mir.local_decls.len()],
    };
    let start = f.p.asm.text.len();
    f.addrs = f.addr_locals();
    f.lazy_cand = f.lazy_locals();
    f.body();
    f.finish();
    let opt = optimize_fn(f.p, &f.p.asm.text[start..].to_owned(), id);
    f.p.asm.text.truncate(start);
    f.p.asm.text.push_str(&opt);
    if ctx == CTX_PTR {
        let text = &f.p.asm.text[start..];
        let mut c = f.p.ptr_ctx;
        c.foreign |= text.contains("__args") || text.contains("__res");
        for (i, _) in text.match_indices("rt_") {
            let r = &text[i..];
            if r.starts_with("rt_args") || r.starts_with("rt_ret") {
                c.calls = true;
            } else if !(r.starts_with("rt_jvec") || r.starts_with("rt_jsrind") || r.starts_with("rt_panic")) {
                c.arith = true;
            }
        }
        f.p.ptr_ctx = c;
    }
}

/// The byte at `off` of the constant allocation `label` (`A<n>`) as an
/// immediate operand, if the allocation is immutable.
fn const_byte(p: &mut Program<'_>, label: &str, off: u64) -> Option<String> {
    let tcx = p.tcx;
    let n: usize = label[1..].parse().ok()?;
    let aid = *p.allocs.get_index(n)?;
    let GlobalAlloc::Memory(m) = tcx.global_alloc(aid) else { return None };
    let alloc = m.inner();
    if alloc.mutability.is_mut() || off >= alloc.len() as u64 {
        return None;
    }
    for &(o, prov) in alloc.provenance().ptrs().iter() {
        let o = o.bytes();
        if off == o || off == o + 1 {
            let target = prov.alloc_id();
            if !matches!(tcx.global_alloc(target), GlobalAlloc::Memory(_) | GlobalAlloc::Static(_)) {
                return None;
            }
            let b = alloc.inspect_with_uninit_and_ptr_outside_interpreter(o as usize..o as usize + 2);
            let rel = u16::from_le_bytes([b[0], b[1]]);
            let l = p.alloc_label(target, rustc_span::DUMMY_SP);
            return Some(format!("#{}({l}+{rel})", if off == o { '<' } else { '>' }));
        }
    }
    let b = alloc.inspect_with_uninit_and_ptr_outside_interpreter(off as usize..off as usize + 1);
    Some(format!("#${:02x}", b[0]))
}

/// The assembly-level passes over the text of one function.
fn optimize_fn(p: &mut Program<'_>, text: &str, id: usize) -> String {
    let opt = crate::asm::relax(text);
    let Some((layout, args_end)) = p.layouts.get(&id).cloned() else { return opt };
    let opt = crate::asm::forward(&opt, id, &layout, args_end, &mut |l, o| const_byte(p, l, o));
    crate::asm::peephole(&crate::asm::relax(&opt))
}

/// `rt_ctx_save` / `rt_ctx_restore` for interrupt dispatchers: copy what
/// the code reached through function pointers can change to `rt_ctxbuf`
/// and back: always the zero-page pointers and `rt_jvec`, the runtime
/// registers and assembly argument areas only if that code uses them.
/// See `Program::ptr_ctx`.
#[derive(Clone, Copy, Default)]
struct PtrCtx {
    arith: bool,
    calls: bool,
    foreign: bool,
}

fn ctx_save(c: PtrCtx) -> String {
    let mut a = Asm::default();
    a.raw("");
    a.comment(
        "interrupt context: the zero-page pointers and rt_jvec, and what the code\n\
         reached through function pointers uses of the runtime registers and the\n\
         assembly argument areas",
    );
    for (name, save) in [("rt_ctx_save", true), ("rt_ctx_restore", false)] {
        a.label(name);
        let copy = |a: &mut Asm, from: &str, to: &str, n: &str, guard: bool| {
            let (src, dst) = if save { (from, to) } else { (to, from) };
            let (lloop, ldone) = (a.new_label(), a.new_label());
            a.ins(format!("ldx #{n}"));
            if guard {
                a.ins(format!("beq {ldone}"));
            }
            a.label(&lloop);
            a.ins(format!("lda {src}-1,x"));
            a.ins(format!("sta {dst}-1,x"));
            a.ins("dex");
            a.ins(format!("bne {lloop}"));
            a.label(&ldone);
        };
        copy(&mut a, "$f9", "rt_ctxbuf+256", "6", false);
        copy(&mut a, "rt_jvec", "rt_ctxbuf+262", "2", false);
        // Assembly functions may use the arithmetic registers too.
        if c.arith || c.foreign {
            copy(&mut a, "rt_save", "rt_ctxbuf", "rt_args-rt_save", false);
        }
        if c.calls {
            copy(&mut a, "rt_args", "rt_ctxbuf+rt_args-rt_save", "rt_save_end-rt_args", false);
        }
        if c.foreign {
            copy(&mut a, "rt_fsave", "rt_ctxbuf+rt_save_end-rt_save", "rt_fsave_end-rt_fsave", true);
        }
        a.ins("rts");
    }
    a.text
}

impl<'a, 'tcx> FnCx<'a, 'tcx> {
    fn tcx(&self) -> TyCtxt<'tcx> {
        self.p.tcx
    }

    fn a(&mut self) -> &mut Asm {
        &mut self.p.asm
    }

    fn fatal(&self, msg: impl Into<String>) -> ! {
        let mut path = self.p.call_path(self.id);
        path.reverse();
        let names: Vec<String> = path.iter().map(|&f| format!("  {}", self.p.name(f))).collect();
        self.tcx()
            .dcx()
            .struct_span_fatal(self.span, format!("rust64: {}", msg.into()))
            .with_note(format!("reached from `main` through:\n{}", names.join("\n")))
            .emit();
        unreachable!()
    }

    fn monomorphize<T: TypeFoldable<TyCtxt<'tcx>>>(&self, v: T) -> T {
        self.inst.instantiate_mir_and_normalize_erasing_regions(
            self.tcx(),
            TypingEnv::fully_monomorphized(),
            ty::EarlyBinder::bind(self.tcx(), v),
        )
    }

    fn layout(&self, ty: Ty<'tcx>) -> TyAndLayout<'tcx> {
        self.p.cx.layout_of(ty)
    }

    fn local_ty(&self, l: Local) -> Ty<'tcx> {
        self.monomorphize(self.mir.local_decls[l].ty)
    }

    fn frame(&self) -> String {
        format!("F{}f", self.id)
    }

    /// The label of a local's storage (`F<id>v<n>`, defined in `finish`).
    fn local_name(&self, l: Local) -> String {
        format!("F{}v{}", self.id, l.as_usize())
    }

    /// The address of a local; marks its storage as used.
    fn local_addr(&self, l: Local) -> String {
        self.touched.borrow_mut()[l.as_usize()] = true;
        self.local_name(l)
    }

    fn block(&self, bb: BasicBlock) -> String {
        format!("F{}_b{}", self.id, bb.as_usize())
    }

    /// A temporary of `size` bytes, valid until the end of the statement.
    fn temp(&mut self, size: u64) -> String {
        let t = format!("F{}t+{}", self.id, self.temp_cur);
        self.temp_cur += size;
        self.temp_max = self.temp_max.max(self.temp_cur);
        t
    }

    /// Lays out the frame: the result, the arguments and the locals whose
    /// storage is used, then the temporaries.
    fn finish(&mut self) {
        let id = self.id;
        let frame = self.frame();
        let mut defs = String::new();
        let mut off = 0;
        let mut layout = crate::asm::FrameLayout::new();
        let mut args_end = 0;
        let touched = self.touched.borrow().clone();
        for (l, _) in self.mir.local_decls.iter_enumerated() {
            let i = l.as_usize();
            if i > self.mir.arg_count && !touched[i] {
                continue;
            }
            let size = self.layout(self.local_ty(l)).size.bytes();
            let _ = writeln!(defs, "F{id}v{i} = {frame}+{off}");
            layout.insert(format!("F{id}v{i}"), (off, size));
            if i <= self.mir.arg_count {
                let _ = writeln!(defs, "L{id}_{i} = {frame}+{off}");
                layout.insert(format!("L{id}_{i}"), (off, size));
                args_end = off + size;
            }
            off += size;
        }
        let _ = writeln!(defs, "F{id}t = {frame}+{off}");
        layout.insert(format!("F{id}t"), (off, self.temp_max));
        self.p.layouts.insert(id, (layout, args_end));
        self.p.frames.push((id, off + self.temp_max, defs));
        let args = (1..=self.mir.arg_count).map(|l| self.local_ty(Local::from_usize(l))).collect();
        let ret = self.layout(self.local_ty(Local::from_usize(0))).size.bytes();
        self.p.sigs.insert(id, (args, ret));
    }

    fn body(&mut self) {
        let name = self.p.name(self.id);
        self.a().raw("");
        self.a().comment(format!("fn {name}"));
        let label = format!("F{}", self.id);
        self.a().label(&label);
        let order = self.block_order();
        let panics = self.panic_blocks();
        for (k, &bb) in order.iter().enumerate() {
            let data = &self.mir.basic_blocks[bb];
            let l = self.block(bb);
            self.a().label(&l);
            if panics[bb.as_usize()] {
                self.a().ins("jmp rt_panic");
                continue;
            }
            let term = data.terminator();
            let next = order.get(k + 1).copied();
            let fused = self.fusable(bb);
            for (i, stmt) in data.statements.iter().enumerate() {
                if fused.is_some_and(|(k, ..)| k == i) {
                    continue;
                }
                self.span = stmt.source_info.span;
                self.temp_cur = 0;
                self.statement(&stmt.kind);
            }
            self.span = term.source_info.span;
            self.temp_cur = 0;
            if let Some((_, op, x, y, t_true, t_false)) = fused {
                self.branch_on(op, x, y, t_true, t_false, next);
            } else {
                self.terminator(&term.kind, next);
            }
        }
    }

    /// The order in which the blocks are emitted: each block is followed by
    /// the successor that control usually continues with (the target of a
    /// goto, call or assert, the "otherwise" branch of a switch), if that
    /// is not placed yet, so that most jumps disappear. Unreachable and
    /// cleanup blocks (panic = abort: no unwinding) are left out.
    fn block_order(&self) -> Vec<BasicBlock> {
        let blocks = &self.mir.basic_blocks;
        let reachable = self.reachable_blocks();
        let ok = |b: BasicBlock| reachable[b.as_usize()] && !blocks[b].is_cleanup;
        let mut placed = vec![false; blocks.len()];
        let mut order = Vec::with_capacity(blocks.len());
        for start in blocks.indices() {
            let mut cur = start;
            while ok(cur) && !std::mem::replace(&mut placed[cur.as_usize()], true) {
                order.push(cur);
                let next = match &blocks[cur].terminator().kind {
                    TerminatorKind::Goto { target, .. }
                    | TerminatorKind::Assert { target, .. }
                    | TerminatorKind::Drop { target, .. }
                    | TerminatorKind::FalseEdge { real_target: target, .. }
                    | TerminatorKind::FalseUnwind { real_target: target, .. } => Some(*target),
                    TerminatorKind::Call { target, .. } => *target,
                    TerminatorKind::SwitchInt { discr, targets } => Some(match self.const_bits(discr) {
                        Some(c) => targets.target_for_value(c),
                        None => targets.otherwise(),
                    }),
                    _ => None,
                };
                match next {
                    Some(n) => cur = n,
                    None => break,
                }
            }
        }
        order
    }

    /// The value of an operand that is known to be constant: a constant, a
    /// runtime check, or a local that only ever gets the same constant.
    fn const_bits(&self, op: &Operand<'tcx>) -> Option<u128> {
        if let Operand::Copy(p) | Operand::Move(p) = op
            && p.projection.is_empty()
        {
            return self.consts[p.local.as_usize()];
        }
        const_operand(self.tcx(), self.inst, op)
    }

    /// Blocks reachable from the entry, following only the taken edge of a
    /// switch on a constant (e.g. disabled `ub_checks`).
    fn reachable_blocks(&self) -> Vec<bool> {
        let blocks = &self.mir.basic_blocks;
        let mut seen = vec![false; blocks.len()];
        let mut work = vec![BasicBlock::from_usize(0)];
        while let Some(bb) = work.pop() {
            if std::mem::replace(&mut seen[bb.as_usize()], true) {
                continue;
            }
            let term = blocks[bb].terminator();
            if let TerminatorKind::SwitchInt { discr, targets } = &term.kind
                && let Some(v) = self.const_bits(discr)
            {
                work.push(targets.target_for_value(v));
                continue;
            }
            work.extend(term.successors());
        }
        seen
    }

    /// Whether a call is a panic: `abort`, `unreachable`, or a diverging
    /// function of `core` (see `call`).
    fn is_panic_call(&self, func: &Operand<'tcx>) -> bool {
        let tcx = self.tcx();
        let fty = self.monomorphize(func.ty(self.mir, tcx));
        let ty::FnDef(def_id, fargs) = *fty.kind() else {
            return false;
        };
        let inst =
            Instance::expect_resolve(tcx, TypingEnv::fully_monomorphized(), def_id, fargs.skip_binder(), self.span);
        match inst.def {
            InstanceKind::Intrinsic(_) => {
                matches!(tcx.item_name(inst.def_id()).as_str(), "abort" | "abort_immediate" | "unreachable")
            }
            InstanceKind::Item(d) => {
                !tcx.is_foreign_item(d)
                    && tcx.crate_name(d.krate) == rustc_span::sym::core
                    && fty.fn_sig(tcx).output().skip_binder().is_never()
            }
            _ => false,
        }
    }

    /// Blocks that can only end in a panic and do nothing before it that
    /// could be seen: no call, no store through a pointer (which may be a
    /// hardware register). A panic has no message, so what such a block
    /// computes (the arguments of the message, when rustc has inlined
    /// `panic_fmt`) is not needed: the block is compiled as `jmp rt_panic`.
    /// That also keeps `core::fmt`, whose functions the arguments point
    /// to, out of the program.
    fn panic_blocks(&self) -> Vec<bool> {
        let blocks = &self.mir.basic_blocks;
        let mut panics = vec![false; blocks.len()];
        loop {
            let mut changed = false;
            for (bb, data) in blocks.iter_enumerated() {
                if panics[bb.as_usize()] {
                    continue;
                }
                let quiet = data.statements.iter().all(|s| match &s.kind {
                    StatementKind::Assign(b) => !b.0.is_indirect(),
                    StatementKind::SetDiscriminant { place, .. } => !place.is_indirect(),
                    StatementKind::Intrinsic(..) => false,
                    _ => true,
                });
                let p = |t: BasicBlock| panics[t.as_usize()];
                let ends = match &data.terminator().kind {
                    TerminatorKind::Unreachable
                    | TerminatorKind::UnwindResume
                    | TerminatorKind::UnwindTerminate(_) => true,
                    TerminatorKind::Goto { target, .. }
                    | TerminatorKind::FalseEdge { real_target: target, .. }
                    | TerminatorKind::FalseUnwind { real_target: target, .. } => p(*target),
                    TerminatorKind::SwitchInt { discr, targets } => match self.const_bits(discr) {
                        Some(c) => p(targets.target_for_value(c)),
                        None => targets.all_targets().iter().all(|&t| p(t)),
                    },
                    TerminatorKind::Call { func, target: None, .. } => self.is_panic_call(func),
                    _ => false,
                };
                if quiet && ends {
                    panics[bb.as_usize()] = true;
                    changed = true;
                }
            }
            if !changed {
                return panics;
            }
        }
    }

    // ---- lazy values and fused comparisons ----

    /// The offset and layout of `place` inside its local, if the place is
    /// the local or a field (of a field ...) of it.
    fn field_offset(&self, place: Place<'tcx>) -> Option<(u64, TyAndLayout<'tcx>)> {
        let mut layout = self.layout(self.local_ty(place.local));
        let mut off = 0;
        for elem in place.projection.iter() {
            match elem {
                ProjectionElem::Field(fi, ty) => {
                    off += layout.fields.offset(fi.as_usize()).bytes();
                    layout = self.layout(self.monomorphize(ty));
                }
                ProjectionElem::Downcast(_, v) => layout = layout.for_variant(&self.p.cx, v),
                ProjectionElem::OpaqueCast(ty) => layout = self.layout(self.monomorphize(ty)),
                _ => return None,
            }
        }
        Some((off, layout))
    }

    /// The operands a lazy local can be a view of: `rv` is a copy, a cast
    /// that keeps the bytes (or truncates / zero-extends), or a tuple,
    /// struct, closure or fat pointer built from operands.
    fn view_operands(&self, dest: Local, rv: &'tcx Rvalue<'tcx>) -> Option<Vec<&'tcx Operand<'tcx>>> {
        let dl = self.layout(self.local_ty(dest));
        match rv {
            Rvalue::Use(op, _) => Some(vec![op]),
            Rvalue::Cast(kind, op, _) => {
                let st = self.monomorphize(op.ty(self.mir, self.tcx()));
                let ss = self.layout(st).size.bytes();
                match kind {
                    // a sign extension needs code
                    CastKind::IntToInt if !(Self::is_signed(st) && dl.size.bytes() > ss) => Some(vec![op]),
                    CastKind::PtrToPtr
                    | CastKind::Transmute
                    | CastKind::FnPtrToPtr
                    | CastKind::PointerExposeProvenance
                    | CastKind::PointerWithExposedProvenance
                        if dl.size.bytes() <= ss =>
                    {
                        Some(vec![op])
                    }
                    _ => None,
                }
            }
            Rvalue::Aggregate(kind, ops) => {
                let ok = match &**kind {
                    AggregateKind::Tuple | AggregateKind::Closure(..) | AggregateKind::RawPtr(..) => true,
                    AggregateKind::Adt(_, _, _, _, active) => {
                        active.is_none() && matches!(dl.variants, Variants::Single { .. })
                    }
                    _ => false,
                };
                ok.then(|| ops.iter().collect())
            }
            _ => None,
        }
    }

    /// Whether a use of a view (at one of `uses`) can be reached from the
    /// assignment at `from` without passing the view's definition `def`
    /// again: then the view would read a changed source.
    fn reaches_use(
        &self,
        from: Location,
        def: Location,
        uses: &rustc_data_structures::fx::FxHashSet<Location>,
    ) -> bool {
        let blocks = &self.mir.basic_blocks;
        // Scans a block from statement `start`; true: a use was found,
        // false + `stop`: the definition was passed.
        let scan = |bb: BasicBlock, start: usize, stop: &mut bool| -> bool {
            let n = blocks[bb].statements.len();
            for i in start..=n {
                let loc = Location { block: bb, statement_index: i };
                if uses.contains(&loc) {
                    return true;
                }
                if loc == def {
                    *stop = true;
                    return false;
                }
            }
            false
        };
        // The assigning statement itself may read the view while it writes.
        if uses.contains(&from) {
            return true;
        }
        let mut seen = vec![false; blocks.len()];
        let mut work = Vec::new();
        let mut stop = false;
        if scan(from.block, from.statement_index + 1, &mut stop) {
            return true;
        }
        if !stop {
            work.extend(blocks[from.block].terminator().successors());
        }
        while let Some(bb) = work.pop() {
            if blocks[bb].is_cleanup || std::mem::replace(&mut seen[bb.as_usize()], true) {
                continue;
            }
            let mut stop = false;
            if scan(bb, 0, &mut stop) {
                return true;
            }
            if !stop {
                work.extend(blocks[bb].terminator().successors());
            }
        }
        false
    }

    /// Locals that need no storage ("views"): defined once from operands
    /// that are constants or (fields of) unborrowed locals (see
    /// `view_operands`), and only read as a whole, by field or as an array
    /// index, where no assignment to the underlying locals can reach a use
    /// without passing the definition again. A view of a view counts down
    /// to the real locals.
    fn lazy_locals(&mut self) -> Vec<bool> {
        let n = self.mir.local_decls.len();
        let mut cand = vec![false; n];
        // The real locals a view reads.
        let mut roots: Vec<Vec<Local>> = vec![Vec::new(); n];
        // Definitions before uses: reverse postorder.
        let order: Vec<BasicBlock> =
            rustc_middle::mir::traversal::reverse_postorder(self.mir).map(|(bb, _)| bb).collect();
        for bb in order {
            let data = &self.mir.basic_blocks[bb];
            if data.is_cleanup {
                continue;
            }
            for (i, stmt) in data.statements.iter().enumerate() {
                let StatementKind::Assign(b) = &stmt.kind else { continue };
                let (place, rv) = &**b;
                let t = place.local.as_usize();
                let u = &self.uses;
                if !place.projection.is_empty()
                    || t == 0
                    || t <= self.mir.arg_count
                    || u.defs[t] != 1
                    || u.uses[t] == 0
                    || u.uses[t] != u.plain[t] + u.index[t] + u.field_reads[t] + u.derefs[t]
                    || u.borrowed[t]
                    || self.addrs[t].is_some()
                {
                    continue;
                }
                let def = Location { block: bb, statement_index: i };
                let size = self.layout(self.local_ty(place.local)).size.bytes();
                if size == 0 || size > 16 {
                    continue;
                }
                let Some(ops) = self.view_operands(place.local, rv) else { continue };
                let mut my_roots = Vec::new();
                let mut ok = true;
                for op in ops {
                    match op {
                        Operand::Constant(_) | Operand::RuntimeChecks(_) => {}
                        Operand::Copy(p) | Operand::Move(p) => {
                            if self.field_offset(*p).is_none() {
                                ok = false;
                                break;
                            }
                            let sl = p.local.as_usize();
                            if cand[sl] {
                                my_roots.extend(roots[sl].iter().copied());
                            } else {
                                my_roots.push(p.local);
                            }
                        }
                    }
                }
                if !ok {
                    continue;
                }
                let u = &self.uses;
                let uses: rustc_data_structures::fx::FxHashSet<Location> =
                    u.use_locs[t].iter().copied().collect();
                for r in &my_roots {
                    let r = r.as_usize();
                    if u.borrowed[r]
                        || u.def_locs[r].iter().any(|&d| d != def && self.reaches_use(d, def, &uses))
                    {
                        ok = false;
                        break;
                    }
                }
                if !ok {
                    continue;
                }
                cand[t] = true;
                roots[t] = my_roots;
                self.lazy_def[t] = Some(rv);
            }
        }
        cand
    }

    /// The value of a lazy local: a view of its operands, no code.
    fn lazy_val(&mut self, local: Local) -> Option<Val> {
        if !self.lazy_cand[local.as_usize()] {
            return None;
        }
        let rv = self.lazy_def[local.as_usize()]?;
        let dl = self.layout(self.local_ty(local));
        let dsize = dl.size.bytes();
        let mut bytes = vec!["#$00".to_owned(); dsize as usize];
        let put = |bytes: &mut Vec<String>, off: u64, v: &Val, size: u64| {
            for i in 0..size.min(dsize - off.min(dsize)) {
                bytes[(off + i) as usize] = v.byte(i).unwrap();
            }
        };
        match rv {
            Rvalue::Use(op, _) | Rvalue::Cast(_, op, _) => {
                let (v, sl) = self.operand(op);
                put(&mut bytes, 0, &v, sl.size.bytes());
            }
            Rvalue::Aggregate(kind, ops) => {
                let raw_ptr = matches!(&**kind, AggregateKind::RawPtr(..));
                let vl = match &**kind {
                    AggregateKind::Adt(_, variant, _, _, _) => dl.for_variant(&self.p.cx, *variant),
                    _ => dl,
                };
                let mut next = 0;
                for (i, op) in ops.iter_enumerated() {
                    let (v, l) = self.operand(op);
                    let off = if raw_ptr { next } else { vl.fields.offset(i.as_usize()).bytes() };
                    put(&mut bytes, off, &v, l.size.bytes());
                    next += l.size.bytes();
                }
            }
            _ => unreachable!(),
        }
        Some(Val::Imm(bytes))
    }

    /// A comparison at the end of block `bb` whose only use is the block's
    /// branch or assert: the statement index, the comparison and the
    /// targets for true and false.
    #[allow(clippy::type_complexity)]
    fn fusable(
        &self,
        bb: BasicBlock,
    ) -> Option<(usize, BinOp, &'tcx Operand<'tcx>, &'tcx Operand<'tcx>, Target, Target)> {
        let data = &self.mir.basic_blocks[bb];
        let (i, stmt) = data.statements.iter().enumerate().rev().find(|(_, s)| {
            !matches!(s.kind, StatementKind::StorageDead(_) | StatementKind::StorageLive(_) | StatementKind::Nop)
        })?;
        let StatementKind::Assign(b) = &stmt.kind else { return None };
        let (place, rv) = &**b;
        let Rvalue::BinaryOp(op, ops) = rv else { return None };
        if !matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) {
            return None;
        }
        let t = place.local.as_usize();
        let u = &self.uses;
        let term_loc = Location { block: bb, statement_index: data.statements.len() };
        if !place.projection.is_empty()
            || u.defs[t] != 1
            || u.uses[t] != 1
            || u.plain[t] != 1
            || u.borrowed[t]
            || u.plain_loc[t] != Some(term_loc)
            || self.consts[t].is_some()
        {
            return None;
        }
        let is_t = |o: &Operand<'tcx>| {
            matches!(o, Operand::Copy(p) | Operand::Move(p) if p.projection.is_empty() && p.local == place.local)
        };
        let (t_true, t_false) = match &data.terminator().kind {
            TerminatorKind::SwitchInt { discr, targets } if is_t(discr) => (
                Target::Block(targets.target_for_value(1)),
                Target::Block(targets.target_for_value(0)),
            ),
            TerminatorKind::Assert { cond, expected, target, .. } if is_t(cond) => {
                if *expected {
                    (Target::Block(*target), Target::Panic)
                } else {
                    (Target::Panic, Target::Block(*target))
                }
            }
            _ => return None,
        };
        Some((i, *op, &ops.0, &ops.1, t_true, t_false))
    }

    fn jump(&mut self, t: Target, next: Option<BasicBlock>) {
        match t {
            Target::Block(b) => self.goto(b, next),
            Target::Panic => self.a().ins("jmp rt_panic"),
        }
    }

    /// Branches to `t_true` if the flag test `b_true` holds (`b_false` is
    /// its opposite), else to `t_false`. Block targets may be far away, so
    /// the conditional branch only skips a `jmp`.
    fn two_way(
        &mut self,
        b_true: &str,
        b_false: &str,
        t_true: Target,
        t_false: Target,
        next: Option<BasicBlock>,
    ) {
        let skip = self.a().new_label();
        if next.is_some() && t_true == Target::Block(next.unwrap()) {
            self.a().ins(format!("{b_true} {skip}"));
            self.jump(t_false, None);
            self.a().label(&skip);
        } else {
            self.a().ins(format!("{b_false} {skip}"));
            self.jump(t_true, None);
            self.a().label(&skip);
            self.jump(t_false, next);
        }
    }

    /// A comparison and the branch on its result, without storing the bool.
    fn branch_on(
        &mut self,
        op: BinOp,
        x: &Operand<'tcx>,
        y: &Operand<'tcx>,
        t_true: Target,
        t_false: Target,
        next: Option<BasicBlock>,
    ) {
        let (va, la) = self.operand(x);
        let (vb, _) = self.operand(y);
        let n = la.size.bytes();
        let signed = Self::is_signed(la.ty);
        match op {
            BinOp::Eq | BinOp::Ne => {
                let (t_eq, t_ne) = if op == BinOp::Eq { (t_true, t_false) } else { (t_false, t_true) };
                let a = &mut self.p.asm;
                a.setup_val(&va, 0);
                a.setup_val(&vb, 1);
                let lne = a.new_label();
                let mut any = false;
                for i in 0..n {
                    if Self::same_const_byte(&va, &vb, i) {
                        continue;
                    }
                    a.lda(&va, i, 0);
                    if !vb.byte_is(i, 0) {
                        a.op_byte("cmp", &vb, i, 1);
                    }
                    a.ins(format!("bne {lne}"));
                    any = true;
                }
                if !any {
                    self.jump(t_eq, next);
                    return;
                }
                self.jump(t_eq, None);
                self.a().label(&lne);
                self.jump(t_ne, next);
            }
            _ => {
                // p < q  <=>  carry clear after p - q.
                let (p, q, lt_is_true) = match op {
                    BinOp::Lt => (&va, &vb, true),
                    BinOp::Ge => (&va, &vb, false),
                    BinOp::Gt => (&vb, &va, true),
                    _ => (&vb, &va, false), // Le: !(b < a)
                };
                let (p, q) = (p.clone(), q.clone());
                self.compare(&p, &q, n, signed);
                if lt_is_true {
                    self.two_way("bcc", "bcs", t_true, t_false, next);
                } else {
                    self.two_way("bcs", "bcc", t_true, t_false, next);
                }
            }
        }
    }

    /// Whether byte `i` of both values is the same constant.
    fn same_const_byte(p: &Val, q: &Val, i: u64) -> bool {
        match (p.byte(i), q.byte(i)) {
            (Some(a), Some(b)) => a.starts_with("#$") && a == b,
            _ => false,
        }
    }

    // ---- known addresses ----

    /// The address of `place` if it is known when assembling, given the
    /// known addresses of pointer locals `addrs`: a local of this frame (plus
    /// fields), or the target of a known pointer (plus fields).
    fn static_place_addr(&self, place: Place<'tcx>, addrs: &[Option<String>]) -> Option<String> {
        let mut layout = self.layout(self.local_ty(place.local));
        let mut base = self.local_addr(place.local);
        let mut off = 0u64;
        for (i, elem) in place.projection.iter().enumerate() {
            match elem {
                ProjectionElem::Deref => {
                    if i != 0 || layout.size.bytes() != 2 {
                        return None;
                    }
                    base = addrs[place.local.as_usize()].clone()?;
                    let pointee = layout.ty.builtin_deref(true)?;
                    layout = self.layout(pointee);
                    if !layout.is_sized() {
                        return None;
                    }
                }
                ProjectionElem::Field(fi, ty) => {
                    off += layout.fields.offset(fi.as_usize()).bytes();
                    layout = self.layout(self.monomorphize(ty));
                }
                ProjectionElem::Downcast(_, v) => layout = layout.for_variant(&self.p.cx, v),
                ProjectionElem::ConstantIndex { offset, from_end: false, .. } => {
                    let e = layout.field(&self.p.cx, 0);
                    off += offset * e.size.bytes();
                    layout = e;
                }
                ProjectionElem::OpaqueCast(ty) | ProjectionElem::UnwrapUnsafeBinder(ty) => {
                    layout = self.layout(self.monomorphize(ty));
                }
                _ => return None,
            }
        }
        if !layout.is_sized() {
            return None;
        }
        Some(if off == 0 { base } else { format!("{base}+{off}") })
    }

    /// The address in a constant pointer value (`#$20,#$d0` or `#<(X+n)`).
    fn imm_addr(v: &Val) -> Option<String> {
        let Val::Imm(b) = v else { return None };
        if b.len() != 2 {
            return None;
        }
        if let (Some(lo), Some(hi)) = (b[0].strip_prefix("#$"), b[1].strip_prefix("#$")) {
            return Some(format!("${hi}{lo}"));
        }
        let e = b[0].strip_prefix("#<(")?.strip_suffix(')')?;
        Some(e.to_owned())
    }

    /// For each local: the address it always holds, if it is a thin pointer
    /// that is assigned exactly once (from a constant, a copy or cast of
    /// such a local, or a reference to a place with a known address) and
    /// never borrowed or written through a projection.
    fn addr_locals(&mut self) -> Vec<Option<String>> {
        let n = self.mir.local_decls.len();
        let mut assigns = vec![0u32; n];
        let mut blocked = vec![false; n];
        for a in 1..=self.mir.arg_count {
            blocked[a] = true;
        }
        let mut cands: Vec<(Local, &'tcx Rvalue<'tcx>)> = Vec::new();
        for data in self.mir.basic_blocks.iter() {
            for stmt in &data.statements {
                match &stmt.kind {
                    StatementKind::Assign(b) => {
                        let (place, rv) = &**b;
                        if place.projection.is_empty() {
                            assigns[place.local.as_usize()] += 1;
                            cands.push((place.local, rv));
                        } else if place.projection.first() != Some(&ProjectionElem::Deref) {
                            blocked[place.local.as_usize()] = true;
                        }
                        if let Rvalue::Ref(_, _, p) | Rvalue::RawPtr(_, p) = rv
                            && p.projection.first() != Some(&ProjectionElem::Deref)
                        {
                            blocked[p.local.as_usize()] = true;
                        }
                    }
                    StatementKind::SetDiscriminant { place, .. } => {
                        blocked[place.local.as_usize()] = true;
                    }
                    _ => {}
                }
            }
            match &data.terminator().kind {
                TerminatorKind::Call { destination, .. } => {
                    blocked[destination.local.as_usize()] = true
                }
                TerminatorKind::Drop { place, .. } => blocked[place.local.as_usize()] = true,
                _ => {}
            }
        }
        let mut addrs: Vec<Option<String>> = vec![None; n];
        for _ in 0..6 {
            let mut changed = false;
            for &(l, rv) in &cands {
                let i = l.as_usize();
                if assigns[i] != 1 || blocked[i] || addrs[i].is_some() {
                    continue;
                }
                let ty = self.local_ty(l);
                if ty.builtin_deref(true).is_none() || self.layout(ty).size.bytes() != 2 {
                    continue;
                }
                let v = match rv {
                    Rvalue::Use(op, _) | Rvalue::Cast(_, op, _) => match op {
                        Operand::Copy(p) | Operand::Move(p) if p.projection.is_empty() => {
                            addrs[p.local.as_usize()].clone()
                        }
                        Operand::Constant(c) => {
                            let (v, l) = self.constant(c);
                            if l.size.bytes() == 2 { Self::imm_addr(&v) } else { None }
                        }
                        _ => None,
                    },
                    Rvalue::Ref(_, _, p) | Rvalue::RawPtr(_, p) => self.static_place_addr(*p, &addrs),
                    _ => None,
                };
                if v.is_some() {
                    addrs[i] = v;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        addrs
    }

    /// The location an operand that is a pointer points to.
    fn deref_operand(&mut self, op: &Operand<'tcx>) -> Addr {
        if let Operand::Copy(p) | Operand::Move(p) = op
            && p.projection.is_empty()
            && let Some(a) = self.addrs[p.local.as_usize()].clone()
        {
            return Addr::Abs(a);
        }
        let (v, _) = self.operand(op);
        self.deref_val(&v)
    }

    // ---- places and operands ----

    /// The location of a place, its layout and (behind a fat pointer) the
    /// pointer metadata.
    /// Splits `X+2+1` into (`X`, 3).
    fn split_offset(e: &str) -> (&str, u64) {
        let mut base = e;
        let mut off = 0;
        while let Some((b, k)) = base.rsplit_once('+') {
            let Ok(k) = k.parse::<u64>() else { break };
            base = b;
            off += k;
        }
        (base, off)
    }

    fn place(&mut self, place: Place<'tcx>) -> (Addr, TyAndLayout<'tcx>, Option<Val>) {
        // A view that is dereferenced: its bytes are the pointer.
        if self.lazy_cand[place.local.as_usize()]
            && place.projection.first() == Some(&ProjectionElem::Deref)
            && let Some(Val::Imm(b)) = self.lazy_val(place.local)
        {
            let pl = self.layout(self.local_ty(place.local));
            let Some(pointee) = pl.ty.builtin_deref(true) else {
                self.fatal(format!("deref of `{}`", pl.ty))
            };
            let ptr_val = Val::Imm(b[..2].to_vec());
            let addr = if let Some(e) = Self::imm_addr(&ptr_val) {
                Addr::Abs(e)
            } else {
                let (b0, o0) = Self::split_offset(&b[0]);
                let (b1, o1) = Self::split_offset(&b[1]);
                let ptr = if !b[0].starts_with('#') && b0 == b1 && o1 == o0 + 1 {
                    b[0].clone()
                } else {
                    let t = self.temp(2);
                    self.a().copy(&Addr::Abs(t.clone()), &ptr_val, 2);
                    t
                };
                Addr::Ind { ptr, off: 0 }
            };
            let meta = (b.len() > 2).then(|| Val::Imm(b[2..].to_vec()));
            return self.place_from(addr, self.layout(pointee), meta, place, 1);
        }
        let addr = Addr::Abs(self.local_addr(place.local));
        let layout = self.layout(self.local_ty(place.local));
        self.place_from(addr, layout, None, place, 0)
    }

    /// Applies the projections of `place` from number `skip` on to a
    /// location.
    fn place_from(
        &mut self,
        addr: Addr,
        layout: TyAndLayout<'tcx>,
        meta: Option<Val>,
        place: Place<'tcx>,
        skip: usize,
    ) -> (Addr, TyAndLayout<'tcx>, Option<Val>) {
        let (mut addr, mut layout, mut meta) = (addr, layout, meta);
        for elem in place.projection.iter().skip(skip) {
            match elem {
                ProjectionElem::Deref => {
                    let Some(pointee) = layout.ty.builtin_deref(true) else {
                        self.fatal(format!("deref of `{}`", layout.ty))
                    };
                    let psize = layout.size.bytes();
                    if psize == 2
                        && matches!(&addr, Addr::Abs(e) if *e == self.local_name(place.local))
                        && let Some(a) = self.addrs[place.local.as_usize()].clone()
                    {
                        addr = Addr::Abs(a);
                        layout = self.layout(pointee);
                        meta = None;
                        continue;
                    }
                    let ptr = match &addr {
                        Addr::Abs(e) => e.clone(),
                        Addr::Ind { .. } | Addr::Idx { .. } => {
                            let t = self.temp(psize);
                            let src = Val::Mem(addr.clone());
                            self.a().copy(&Addr::Abs(t.clone()), &src, psize);
                            t
                        }
                    };
                    meta = (psize > 2).then(|| Val::Mem(Addr::Abs(format!("{ptr}+2"))));
                    addr = Addr::Ind { ptr, off: 0 };
                    layout = self.layout(pointee);
                }
                ProjectionElem::Field(i, ty) => {
                    addr = addr.offset(layout.fields.offset(i.as_usize()).bytes());
                    layout = self.layout(self.monomorphize(ty));
                }
                ProjectionElem::Downcast(_, v) => {
                    layout = layout.for_variant(&self.p.cx, v);
                }
                ProjectionElem::Index(l) => {
                    let elem = layout.field(&self.p.cx, 0);
                    let idx = match self.lazy_val(l) {
                        Some(v) => v,
                        None => Val::Mem(Addr::Abs(self.local_addr(l))),
                    };
                    let isize = self.layout(self.local_ty(l)).size.bytes();
                    let stride = elem.size.bytes();
                    // A table of at most 256 bytes at a known address:
                    // `lda table,x` (the bounds check keeps the index small).
                    if let Addr::Abs(base) = &addr
                        && let ty::Array(_, len) = layout.ty.kind()
                        && let Some(len) = len.try_to_target_usize(self.tcx())
                        && stride.is_power_of_two()
                        && len * stride <= 256
                    {
                        let li = idx.byte(0).unwrap();
                        let x = if stride == 1 && !li.starts_with('#') {
                            li
                        } else {
                            let t = self.temp(1);
                            self.a().ins(format!("lda {li}"));
                            for _ in 0..stride.trailing_zeros() {
                                self.a().ins("asl");
                            }
                            self.a().ins(format!("sta {t}"));
                            t
                        };
                        addr = Addr::Idx { base: base.clone(), x, off: 0 };
                        layout = elem;
                        meta = None;
                        continue;
                    }
                    addr = self.index_addr(&addr, &idx, isize, elem.size.bytes());
                    layout = elem;
                    meta = None;
                }
                ProjectionElem::ConstantIndex { offset, from_end, .. } => {
                    let elem = layout.field(&self.p.cx, 0);
                    let index = if from_end {
                        match layout.ty.kind() {
                            ty::Array(_, n) => {
                                let n = n.try_to_target_usize(self.tcx()).unwrap();
                                n - offset
                            }
                            _ => self.fatal("slice patterns from the end are not supported yet"),
                        }
                    } else {
                        offset
                    };
                    addr = addr.offset(index * elem.size.bytes());
                    layout = elem;
                    meta = None;
                }
                ProjectionElem::OpaqueCast(ty) | ProjectionElem::UnwrapUnsafeBinder(ty) => {
                    layout = self.layout(self.monomorphize(ty));
                }
                other => self.fatal(format!("place projection {other:?} is not supported yet")),
            }
        }
        (addr, layout, meta)
    }

    /// The location `base + idx * stride` (idx: an unsigned value of
    /// `isize` bytes).
    fn index_addr(&mut self, base: &Addr, idx: &Val, isize: u64, stride: u64) -> Addr {
        if let Addr::Idx { .. } = base {
            let p = self.temp(2);
            self.a().addr_to(base, &p);
            return self.index_addr(&Addr::Ind { ptr: p, off: 0 }, idx, isize, stride);
        }
        let t = self.scaled(idx, isize, stride);
        let a = &mut self.p.asm;
        match base {
            Addr::Abs(e) => {
                a.ins("clc");
                a.ins(format!("lda {t}"));
                a.ins(format!("adc #<({e})"));
                a.ins(format!("sta {t}"));
                a.ins(format!("lda {t}+1"));
                a.ins(format!("adc #>({e})"));
                a.ins(format!("sta {t}+1"));
                Addr::Ind { ptr: t, off: 0 }
            }
            Addr::Ind { ptr, off } => {
                a.ins("clc");
                a.ins(format!("lda {t}"));
                a.ins(format!("adc {ptr}"));
                a.ins(format!("sta {t}"));
                a.ins(format!("lda {t}+1"));
                a.ins(format!("adc {ptr}+1"));
                a.ins(format!("sta {t}+1"));
                Addr::Ind { ptr: t, off: *off }
            }
            Addr::Idx { .. } => unreachable!(),
        }
    }

    /// A 16-bit temporary holding `v * k` (v: `vsize` bytes, unsigned).
    fn scaled(&mut self, v: &Val, vsize: u64, k: u64) -> String {
        let t = self.temp(2);
        let tv = Addr::Abs(t.clone());
        if k.is_power_of_two() {
            self.a().copy(&tv, v, vsize.min(2));
            if vsize < 2 {
                self.a().ins("lda #0");
                self.a().ins(format!("sta {t}+1"));
            }
            for _ in 0..k.trailing_zeros() {
                self.a().ins(format!("asl {t}"));
                self.a().ins(format!("rol {t}+1"));
            }
        } else {
            // A 16-bit view of the value (one byte: zero-extended).
            let wide = match (vsize < 2, v.byte(0)) {
                (false, _) => Some(v.clone()),
                (true, Some(b)) => Some(Val::Imm(vec![b, "#$00".into()])),
                (true, None) => None,
            };
            if let Some(w) = &wide
                && self.mul_const(&tv, w, 2, k)
            {
                return t;
            }
            let a = &mut self.p.asm;
            a.copy(&Addr::Abs("rt_a".into()), v, vsize.min(2));
            if vsize < 2 {
                a.ins("lda #0");
                a.ins("sta rt_a+1");
            }
            a.copy(&Addr::Abs("rt_b".into()), &Val::imm_u64(k, 2), 2);
            a.ins("jsr rt_mul16");
            a.copy(&tv, &Val::Mem(Addr::Abs("rt_r".into())), 2);
        }
        t
    }

    /// `dst = v * c` (n bytes, wrapping) with shifts and additions, for
    /// one- and two-byte values and constants with few bits. Returns false
    /// (and emits nothing) when the call to the runtime is the better choice.
    ///
    /// The product is built from the top bit of `c` down: shift left, and
    /// add `v` where `c` has a bit. One byte: all in A. Two bytes: the low
    /// byte in A, the high byte in a temporary; `dst` is only written at the
    /// end, so it may be `v` itself.
    fn mul_const(&mut self, dst: &Addr, v: &Val, n: u64, c: u64) -> bool {
        let c = if n >= 8 { c } else { c & ((1 << (8 * n)) - 1) };
        if c == 0 {
            self.a().copy(dst, &Val::imm_u64(0, n), n);
            return true;
        }
        if n > 2 {
            return false;
        }
        let shifts = 63 - c.leading_zeros() as u64;
        let adds = c.count_ones() as u64 - 1;
        // Bytes of code, roughly; a call costs about 20 (one byte) or 35.
        let (cost, limit) = if n == 1 { (shifts + 4 * adds, 20) } else { (4 * shifts + 15 * adds, 56) };
        if cost > limit {
            return false;
        }
        if n == 1 {
            let a = &mut self.p.asm;
            a.setup_val(v, 0);
            a.setup(dst, 2);
            a.lda(v, 0, 0);
            for bit in (0..shifts).rev() {
                a.ins("asl");
                if (c >> bit) & 1 == 1 {
                    a.ins("clc");
                    a.op_byte("adc", v, 0, 0);
                }
            }
            a.sta(dst, 0, 2);
            return true;
        }
        // Both bytes of v as plain operands (the additions use X).
        let v = match (v.byte(0), v.byte(1)) {
            (Some(_), Some(_)) => v.clone(),
            _ => {
                let t = Addr::Abs(self.temp(2));
                self.a().copy(&t, v, 2);
                Val::Mem(t)
            }
        };
        let (v0, v1) = (v.byte(0).unwrap(), v.byte(1).unwrap());
        let h = self.temp(1);
        let a = &mut self.p.asm;
        a.setup(dst, 2);
        a.ins(format!("lda {v1}"));
        a.ins(format!("sta {h}"));
        a.ins(format!("lda {v0}"));
        for bit in (0..shifts).rev() {
            a.ins("asl");
            a.ins(format!("rol {h}"));
            if (c >> bit) & 1 == 1 {
                a.ins("clc");
                a.ins(format!("adc {v0}"));
                a.ins("tax");
                a.ins(format!("lda {h}"));
                a.ins(format!("adc {v1}"));
                a.ins(format!("sta {h}"));
                a.ins("txa");
            }
        }
        a.sta(dst, 0, 2);
        a.ins(format!("lda {h}"));
        a.sta(dst, 1, 2);
        true
    }

    fn operand(&mut self, op: &Operand<'tcx>) -> (Val, TyAndLayout<'tcx>) {
        match op {
            Operand::Copy(p) | Operand::Move(p) => {
                if self.lazy_cand[p.local.as_usize()]
                    && let Some((off, l)) = self.field_offset(*p)
                    && let Some(Val::Imm(b)) = self.lazy_val(p.local)
                {
                    let size = l.size.bytes() as usize;
                    let part = b.into_iter().skip(off as usize).take(size).collect();
                    return (Val::Imm(part), l);
                }
                if p.projection.is_empty() {
                    if let Some(e) = self.addrs[p.local.as_usize()].clone() {
                        let v = Val::Imm(vec![format!("#<({e})"), format!("#>({e})")]);
                        return (v, self.layout(self.local_ty(p.local)));
                    }
                }
                let (a, l, _) = self.place(*p);
                (Val::Mem(a), l)
            }
            Operand::Constant(c) => self.constant(c),
            // ub_checks, contract and overflow checks: as configured for this crate.
            Operand::RuntimeChecks(c) => {
                let on = c.value(self.tcx().sess);
                (Val::imm_bytes(&[on as u8]), self.layout(self.tcx().types.bool))
            }
        }
    }

    /// The value of an immediate operand of `n` bytes (`#$xx` bytes only).
    fn imm_value(v: &Val, n: u64) -> Option<u64> {
        let Val::Imm(b) = v else { return None };
        let mut x = 0u64;
        for i in 0..n.min(8) {
            let byte = match b.get(i as usize) {
                Some(s) => u8::from_str_radix(s.strip_prefix("#$")?, 16).ok()?,
                None => 0,
            };
            x |= (byte as u64) << (8 * i);
        }
        Some(x)
    }

    /// Adds 1 (any size) or subtracts 1 (1 or 2 bytes) in place.
    fn inc_dec(a: &mut Asm, d: &str, n: u64, add: bool) {
        if n == 1 {
            a.ins(format!("{} {d}", if add { "inc" } else { "dec" }));
            return;
        }
        let l = a.new_label();
        if add {
            a.ins(format!("inc {d}"));
            for i in 1..n {
                a.ins(format!("bne {l}"));
                a.ins(format!("inc {d}+{i}"));
            }
        } else {
            a.ins(format!("lda {d}"));
            a.ins(format!("bne {l}"));
            a.ins(format!("dec {d}+1"));
            a.label(&l);
            a.ins(format!("dec {d}"));
            return;
        }
        a.label(&l);
    }

    /// `d = d + b` or `d = d - b` in place, where only the low byte of `b`
    /// is not zero: the upper bytes of `d` just take the carry (`inc`) or
    /// the borrow (`dec`, two bytes only). Returns false if this does not
    /// apply.
    fn add_in_place(a: &mut Asm, dst: &Addr, va: &Val, vb: &Val, n: u64, add: bool) -> bool {
        let (Val::Mem(Addr::Abs(s)), Addr::Abs(d), Val::Imm(_)) = (va, dst, vb) else {
            return false;
        };
        if s != d || n < 2 || (!add && n > 2) || vb.byte_is(0, 0) || !(1..n).all(|i| vb.byte_is(i, 0)) {
            return false;
        }
        let b0 = vb.byte(0).unwrap();
        let l = a.new_label();
        a.ins(if add { "clc" } else { "sec" });
        a.ins(format!("lda {d}"));
        a.ins(format!("{} {b0}", if add { "adc" } else { "sbc" }));
        a.ins(format!("sta {d}"));
        if add {
            a.ins(format!("bcc {l}"));
            a.ins(format!("inc {d}+1"));
            for i in 2..n {
                a.ins(format!("bne {l}"));
                a.ins(format!("inc {d}+{i}"));
            }
        } else {
            a.ins(format!("bcs {l}"));
            a.ins(format!("dec {d}+1"));
        }
        a.label(&l);
        true
    }

    /// Shifts the `n` bytes at `t` by the constant `k`.
    fn shift_const(a: &mut Asm, t: &str, n: u64, k: u64, left: bool, signed: bool) {
        for _ in 0..k {
            if left {
                a.ins(format!("asl {t}"));
                for i in 1..n {
                    a.ins(format!("rol {t}+{i}"));
                }
            } else {
                if signed {
                    a.ins(format!("lda {t}+{}", n - 1));
                    a.ins("cmp #$80");
                    a.ins(format!("ror {t}+{}", n - 1));
                } else {
                    a.ins(format!("lsr {t}+{}", n - 1));
                }
                for i in (0..n - 1).rev() {
                    a.ins(format!("ror {t}+{i}"));
                }
            }
        }
    }

    fn ptr_bytes(label: &str, off: u64) -> Vec<String> {
        vec![format!("#<({label}+{off})"), format!("#>({label}+{off})")]
    }

    fn constant(&mut self, c: &ConstOperand<'tcx>) -> (Val, TyAndLayout<'tcx>) {
        let cv = self.monomorphize(c.const_);
        let layout = self.layout(cv.ty());
        let Ok(val) = cv.eval(self.tcx(), TypingEnv::fully_monomorphized(), c.span) else {
            self.fatal("constant evaluation failed")
        };
        let v = match val {
            ConstValue::ZeroSized => Val::Imm(vec![]),
            ConstValue::Scalar(Scalar::Int(int)) => {
                let bits = int.to_bits(int.size());
                let n = int.size().bytes();
                let bytes: Vec<u8> = (0..n).map(|i| (bits >> (8 * i)) as u8).collect();
                Val::imm_bytes(&bytes)
            }
            ConstValue::Scalar(Scalar::Ptr(ptr, _)) => {
                let (prov, off) = ptr.prov_and_relative_offset();
                let l = self.p.alloc_label(prov.alloc_id(), c.span);
                Val::Imm(Self::ptr_bytes(&l, off.bytes()))
            }
            ConstValue::Slice { alloc_id, meta } => {
                let l = self.p.alloc_label(alloc_id, c.span);
                let mut b = Self::ptr_bytes(&l, 0);
                b.push(format!("#${:02x}", meta & 0xff));
                b.push(format!("#${:02x}", (meta >> 8) & 0xff));
                Val::Imm(b)
            }
            ConstValue::Indirect { alloc_id, offset } => {
                let l = self.p.alloc_label(alloc_id, c.span);
                Val::Mem(Addr::Abs(format!("{l}+{}", offset.bytes())))
            }
        };
        (v, layout)
    }

    /// The location a pointer value points to.
    fn deref_val(&mut self, v: &Val) -> Addr {
        match v {
            Val::Imm(b) if b.len() >= 2 => {
                // A constant pointer: `#$20`,`#$d0` -> `$d020`; `#<(X+n)` -> `X+n`.
                if let (Some(lo), Some(hi)) = (b[0].strip_prefix("#$"), b[1].strip_prefix("#$")) {
                    Addr::Abs(format!("${hi}{lo}"))
                } else if let Some(e) = b[0].strip_prefix("#<") {
                    Addr::Abs(e.to_owned())
                } else {
                    // A view of a pointer stored elsewhere: copy it.
                    let t = self.temp(2);
                    self.a().copy(&Addr::Abs(t.clone()), v, 2);
                    Addr::Ind { ptr: t, off: 0 }
                }
            }
            Val::Mem(Addr::Abs(e)) => Addr::Ind { ptr: e.clone(), off: 0 },
            Val::Mem(a @ (Addr::Ind { .. } | Addr::Idx { .. })) => {
                let t = self.temp(2);
                self.a().copy(&Addr::Abs(t.clone()), &Val::Mem(a.clone()), 2);
                Addr::Ind { ptr: t, off: 0 }
            }
            Val::Imm(_) => self.fatal("bad pointer constant"),
        }
    }

    // ---- statements ----

    fn statement(&mut self, kind: &StatementKind<'tcx>) {
        match kind {
            StatementKind::Assign(b) => {
                let (place, rvalue) = &**b;
                let l = place.local.as_usize();
                // Lazy locals are views; pointers with a known address
                // are used as constants: neither is stored.
                if place.projection.is_empty() && (self.lazy_cand[l] || self.addrs[l].is_some()) {
                    return;
                }
                self.assign(*place, rvalue);
            }
            StatementKind::SetDiscriminant { place, variant_index } => {
                let (a, l, _) = self.place(**place);
                self.set_discr(&a, l, *variant_index);
            }
            StatementKind::Intrinsic(i) => match &**i {
                NonDivergingIntrinsic::Assume(_) => {}
                NonDivergingIntrinsic::CopyNonOverlapping(c) => {
                    let (src, sl) = self.operand(&c.src);
                    let (dst, _) = self.operand(&c.dst);
                    let (count, cl) = self.operand(&c.count);
                    let elem = self.layout(sl.ty.builtin_deref(true).unwrap()).size.bytes();
                    self.memcpy_dyn(&dst, &src, &count, cl.size.bytes(), elem);
                }
            },
            _ => {}
        }
    }

    /// `rt_memcpy` of `count * elem` bytes between the pointer values
    /// `dst` and `src`.
    fn memcpy_dyn(&mut self, dst: &Val, src: &Val, count: &Val, csize: u64, elem: u64) {
        let n = self.scaled(count, csize, elem);
        let s = self.deref_val(src);
        let d = self.deref_val(dst);
        let a = &mut self.p.asm;
        a.addr_to(&s, "zp_p0");
        a.addr_to(&d, "zp_p1");
        a.copy(&Addr::Abs("rt_n".into()), &Val::Mem(Addr::Abs(n)), 2);
        a.ins("jsr rt_memcpy");
    }

    fn assign(&mut self, place: Place<'tcx>, rv: &Rvalue<'tcx>) {
        let (dst, dl, _) = self.place(place);
        let size = dl.size.bytes();
        match rv {
            Rvalue::Use(op, _) | Rvalue::WrapUnsafeBinder(op, _) => {
                let (v, _) = self.operand(op);
                self.a().copy(&dst, &v, size);
            }
            Rvalue::CopyForDeref(p) => {
                let (v, _) = self.operand(&Operand::Copy(*p));
                self.a().copy(&dst, &v, size);
            }
            Rvalue::Repeat(op, n) => {
                let count = self.monomorphize(*n).try_to_target_usize(self.tcx()).unwrap();
                let (v, el) = self.operand(op);
                let es = el.size.bytes();
                if count == 0 || es == 0 {
                    return;
                }
                self.a().copy(&dst, &v, es);
                let rest = (count - 1) * es;
                if rest > 0 {
                    // Forward copy from the first element replicates it.
                    let a = &mut self.p.asm;
                    a.addr_to(&dst, "zp_p0");
                    a.addr_to(&dst.offset(es), "zp_p1");
                    a.copy(&Addr::Abs("rt_n".into()), &Val::imm_u64(rest, 2), 2);
                    a.ins("jsr rt_memcpy");
                }
            }
            Rvalue::Ref(_, _, p) | Rvalue::RawPtr(_, p) => {
                let (a, _, meta) = self.place(*p);
                let (d, tmp) = self.abs_dst(&dst, size);
                self.a().addr_to(&a, &d);
                if size > 2 {
                    match meta {
                        Some(m) => self.a().copy(&Addr::Abs(format!("{d}+2")), &m, size - 2),
                        None => self.fatal("reference to an unsized place without metadata"),
                    }
                }
                self.flush(&dst, tmp, size);
            }
            Rvalue::Cast(kind, op, _) => self.cast(kind, op, &dst, dl),
            Rvalue::BinaryOp(op, b) => {
                let (x, y) = &**b;
                self.binop(*op, x, y, &dst, dl);
            }
            Rvalue::UnaryOp(op, x) => self.unop(*op, x, &dst, size),
            Rvalue::Discriminant(p) => {
                let (a, l, _) = self.place(*p);
                self.get_discr(&a, l, &dst, size);
            }
            Rvalue::Aggregate(kind, ops) => self.aggregate(kind, ops, &dst, dl),
            other => self.fatal(format!("{other:?} is not supported yet")),
        }
    }

    /// An absolute destination for code that needs one: `dst` itself or a
    /// temporary that [`FnCx::flush`] copies to `dst` afterwards.
    fn abs_dst(&mut self, dst: &Addr, size: u64) -> (String, Option<String>) {
        match dst {
            Addr::Abs(e) => (e.clone(), None),
            Addr::Ind { .. } | Addr::Idx { .. } => {
                let t = self.temp(size);
                (t.clone(), Some(t))
            }
        }
    }

    fn flush(&mut self, dst: &Addr, tmp: Option<String>, size: u64) {
        if let Some(t) = tmp {
            self.a().copy(dst, &Val::Mem(Addr::Abs(t)), size);
        }
    }

    fn is_signed(ty: Ty<'_>) -> bool {
        matches!(ty.kind(), ty::Int(_))
    }

    /// Writes `v` (`vsize` bytes) to `dst` (`dsize` bytes), truncating or
    /// extending with zeros or the sign.
    fn extend(&mut self, dst: &Addr, v: &Val, vsize: u64, dsize: u64, signed: bool) {
        let n = vsize.min(dsize);
        self.a().copy(dst, v, n);
        if dsize > n {
            let a = &mut self.p.asm;
            a.setup_val(v, 0);
            a.setup(dst, 2);
            if signed && n > 0 {
                let l = a.new_label();
                a.lda(v, n - 1, 0);
                a.ins("and #$80");
                a.ins(format!("beq {l}"));
                a.ins("lda #$ff");
                a.label(&l);
            } else {
                a.ins("lda #0");
            }
            for i in n..dsize {
                a.sta(dst, i, 2);
            }
        }
    }

    fn cast(&mut self, kind: &CastKind, op: &Operand<'tcx>, dst: &Addr, dl: TyAndLayout<'tcx>) {
        let dsize = dl.size.bytes();
        match kind {
            CastKind::PointerCoercion(PointerCoercion::ReifyFnPointer(_), _) => {
                let (_, sl) = self.operand(op);
                let ty::FnDef(def_id, args) = *sl.ty.kind() else {
                    self.fatal("fn pointer from a non-function")
                };
                let inst = Instance::resolve_for_fn_ptr(
                    self.tcx(),
                    TypingEnv::fully_monomorphized(),
                    def_id,
                    args.skip_binder(),
                )
                .unwrap_or_else(|| self.fatal("cannot resolve the function"));
                let (_, l) = self.p.fn_ptr(inst, false);
                self.a().copy(dst, &Val::Imm(Self::ptr_bytes(&l, 0)), 2);
            }
            CastKind::PointerCoercion(PointerCoercion::ClosureFnPointer(_), _) => {
                let (_, sl) = self.operand(op);
                let ty::Closure(def_id, args) = *sl.ty.kind() else {
                    self.fatal("closure fn pointer from a non-closure")
                };
                let inst =
                    Instance::resolve_closure(self.tcx(), def_id, args, ty::ClosureKind::FnOnce);
                let (_, l) = self.p.fn_ptr(inst, true);
                self.a().copy(dst, &Val::Imm(Self::ptr_bytes(&l, 0)), 2);
            }
            CastKind::PointerCoercion(PointerCoercion::Unsize, _) => {
                let (v, sl) = self.operand(op);
                self.a().copy(dst, &v, 2);
                if dsize > 2 {
                    let (Some(sp), Some(dp)) = (sl.ty.builtin_deref(true), dl.ty.builtin_deref(true))
                    else {
                        self.fatal(format!("unsizing `{}` to `{}` is not supported yet", sl.ty, dl.ty))
                    };
                    // `&S<[T; N]>` -> `&S<[T]>`: the metadata comes from the tails.
                    let (src_pointee, _) = self.tcx().struct_lockstep_tails_for_codegen(
                        sp,
                        dp,
                        TypingEnv::fully_monomorphized(),
                    );
                    match src_pointee.kind() {
                        ty::Array(_, n) => {
                            let n = n.try_to_target_usize(self.tcx()).unwrap();
                            self.a().copy(&dst.offset(2), &Val::imm_u64(n, 2), 2);
                        }
                        _ => self.fatal(format!(
                            "unsizing `{}` is not supported yet (trait objects)",
                            sl.ty
                        )),
                    }
                }
            }
            CastKind::IntToInt => {
                let (v, sl) = self.operand(op);
                let signed = Self::is_signed(sl.ty);
                self.extend(dst, &v, sl.size.bytes(), dsize, signed);
            }
            CastKind::FloatToInt | CastKind::IntToFloat | CastKind::FloatToFloat => {
                self.fatal("floating point is not supported yet")
            }
            _ => {
                // Pointer/int reinterpretations and transmutes keep the bytes.
                let (v, sl) = self.operand(op);
                self.extend(dst, &v, sl.size.bytes(), dsize, false);
            }
        }
    }

    // ---- arithmetic ----

    fn binop(
        &mut self,
        op: BinOp,
        x: &Operand<'tcx>,
        y: &Operand<'tcx>,
        dst: &Addr,
        dl: TyAndLayout<'tcx>,
    ) {
        let (mut va, la) = self.operand(x);
        let (mut vb, lb) = self.operand(y);
        let n = la.size.bytes();
        let signed = Self::is_signed(la.ty);
        use BinOp::*;
        // c + x, c * x, ...: the constant to the right, where the special
        // cases look for it.
        if matches!(
            op,
            Add | AddUnchecked | AddWithOverflow | Mul | MulUnchecked | MulWithOverflow | BitAnd | BitOr | BitXor
        ) && Self::imm_value(&va, n).is_some()
            && Self::imm_value(&vb, n).is_none()
        {
            std::mem::swap(&mut va, &mut vb);
        }
        // x * 2^k, x / 2^k, x % 2^k (unsigned): shifts and masks.
        if matches!(op, Mul | MulUnchecked | Div | Rem)
            && (!signed || matches!(op, Mul | MulUnchecked))
            && let Some(c) = Self::imm_value(&vb, n)
            && c.is_power_of_two()
            && n <= 8
        {
            let k = c.trailing_zeros() as u64;
            if op != Rem
                && let Addr::Abs(d) = dst
            {
                let d = d.clone();
                self.a().copy(dst, &va, n);
                Self::shift_const(&mut self.p.asm, &d, n, k, op != Div, false);
                return;
            }
            if op == Rem {
                let mask = Val::imm_u64(c - 1, n);
                let a = &mut self.p.asm;
                a.setup_val(&va, 0);
                a.setup(dst, 2);
                for i in 0..n {
                    match Self::imm_value(&mask.offset(i), 1) {
                        Some(0) => a.ins("lda #0"),
                        Some(0xff) => a.lda(&va, i, 0),
                        _ => {
                            a.lda(&va, i, 0);
                            a.op_byte("and", &mask, i, 1);
                        }
                    }
                    a.sta(dst, i, 2);
                }
            } else {
                let t = self.temp(n);
                let ta = Addr::Abs(t.clone());
                self.a().copy(&ta, &va, n);
                Self::shift_const(&mut self.p.asm, &t, n, k, op != Div, false);
                self.a().copy(dst, &Val::Mem(ta), n);
            }
            return;
        }
        match op {
            Add | AddUnchecked | AddWithOverflow | Sub | SubUnchecked | SubWithOverflow => {
                let add = matches!(op, Add | AddUnchecked | AddWithOverflow);
                // x = x + 1 / x - 1 in place
                let checked = matches!(op, AddWithOverflow | SubWithOverflow);
                if (n <= 2 || (add && n <= 8))
                    && !checked
                    && Self::imm_value(&vb, n) == Some(1)
                    && let (Val::Mem(Addr::Abs(s)), Addr::Abs(d)) = (&va, dst)
                    && s == d
                {
                    let d = d.clone();
                    Self::inc_dec(&mut self.p.asm, &d, n, add);
                    return;
                }
                // x = x + small / x - small in place
                if !checked && Self::add_in_place(&mut self.p.asm, dst, &va, &vb, n, add) {
                    return;
                }
                let a = &mut self.p.asm;
                a.setup_val(&va, 0);
                a.setup_val(&vb, 1);
                a.setup(dst, 2);
                // While only zero bytes were added, the carry is unchanged
                // and the byte is just copied.
                let mut plain = !checked;
                let first = (0..n).find(|&i| !vb.byte_is(i, 0)).unwrap_or(n);
                if !plain || first < n {
                    a.ins(if add { "clc" } else { "sec" });
                }
                for i in 0..n {
                    a.lda(&va, i, 0);
                    if plain && i < first {
                        // nothing to add yet
                    } else {
                        plain = false;
                        a.op_byte(if add { "adc" } else { "sbc" }, &vb, i, 1);
                    }
                    a.sta(dst, i, 2);
                }
                if matches!(op, AddWithOverflow | SubWithOverflow) {
                    let fo = dl.fields.offset(1).bytes();
                    if signed {
                        let l = a.new_label();
                        a.ins("lda #0");
                        a.ins(format!("bvc {l}"));
                        a.ins("lda #1");
                        a.label(&l);
                    } else {
                        a.ins("lda #0");
                        a.ins("rol");
                        if !add {
                            a.ins("eor #1");
                        }
                    }
                    a.sta(dst, fo, 2);
                }
            }
            BitAnd | BitOr | BitXor => {
                let ins = match op {
                    BitAnd => "and",
                    BitOr => "ora",
                    _ => "eor",
                };
                let a = &mut self.p.asm;
                a.setup_val(&va, 0);
                a.setup_val(&vb, 1);
                a.setup(dst, 2);
                for i in 0..n {
                    // and #$ff / ora #0 / eor #0 change nothing, and #0 gives 0,
                    // ora #$ff gives $ff.
                    match (ins, Self::imm_value(&vb.offset(i), 1)) {
                        ("and", Some(0)) => a.ins("lda #0"),
                        ("ora", Some(0xff)) => a.ins("lda #$ff"),
                        ("and", Some(0xff)) | ("ora" | "eor", Some(0)) => a.lda(&va, i, 0),
                        _ => {
                            a.lda(&va, i, 0);
                            a.op_byte(ins, &vb, i, 1);
                        }
                    }
                    a.sta(dst, i, 2);
                }
            }
            Eq | Ne => {
                let a = &mut self.p.asm;
                a.setup_val(&va, 0);
                a.setup_val(&vb, 1);
                a.setup(dst, 2);
                let (lne, lend) = (a.new_label(), a.new_label());
                for i in 0..n {
                    a.lda(&va, i, 0);
                    a.op_byte("cmp", &vb, i, 1);
                    a.ins(format!("bne {lne}"));
                }
                a.ins(format!("lda #{}", (op == Eq) as u8));
                a.ins(format!("jmp {lend}"));
                a.label(&lne);
                a.ins(format!("lda #{}", (op == Ne) as u8));
                a.label(&lend);
                a.sta(dst, 0, 2);
            }
            Lt | Le | Gt | Ge => {
                // a < b  <=>  carry clear after a - b.
                let (p, q, want_clear) = match op {
                    Lt => (&va, &vb, true),
                    Ge => (&va, &vb, false),
                    Gt => (&vb, &va, true),
                    _ => (&vb, &va, false), // Le: !(b < a)
                };
                let (p, q) = (p.clone(), q.clone());
                self.compare(&p, &q, n, signed);
                let a = &mut self.p.asm;
                a.setup(dst, 2);
                a.ins("lda #0");
                a.ins("rol");
                if want_clear {
                    a.ins("eor #1");
                }
                a.sta(dst, 0, 2);
            }
            Cmp => {
                let t = self.temp(1);
                self.compare(&va, &vb, n, signed); // C clear: a < b
                let a = &mut self.p.asm;
                a.ins("lda #0");
                a.ins("rol");
                a.ins("eor #1");
                a.ins(format!("sta {t}"));
                let (vb2, va2) = (vb.clone(), va.clone());
                self.compare(&vb2, &va2, n, signed); // C clear: b < a
                let a = &mut self.p.asm;
                a.setup(dst, 2);
                a.ins("lda #0");
                a.ins("rol");
                a.ins("eor #1"); // 1 if a > b
                a.ins("sec");
                a.ins(format!("sbc {t}"));
                a.sta(dst, 0, 2);
            }
            Shl | ShlUnchecked | Shr | ShrUnchecked => {
                if let Some(k) = Self::imm_value(&vb, 1)
                    && let Addr::Abs(d) = dst
                {
                    let d = d.clone();
                    let left = matches!(op, Shl | ShlUnchecked);
                    self.a().copy(dst, &va, n);
                    Self::shift_const(&mut self.p.asm, &d, n, k & (8 * n - 1), left, signed);
                    return;
                }
                let t = self.temp(n);
                let ta = Addr::Abs(t.clone());
                self.a().copy(&ta, &va, n);
                if let Some(k) = Self::imm_value(&vb, 1) {
                    let left = matches!(op, Shl | ShlUnchecked);
                    Self::shift_const(&mut self.p.asm, &t, n, k & (8 * n - 1), left, signed);
                    self.a().copy(dst, &Val::Mem(ta), n);
                    return;
                }
                let a = &mut self.p.asm;
                a.setup_val(&vb, 1);
                a.lda(&vb, 0, 1);
                a.ins(format!("and #{}", 8 * n - 1));
                a.ins("tax");
                let (lloop, ldone) = (a.new_label(), a.new_label());
                a.ins(format!("beq {ldone}"));
                a.label(&lloop);
                if matches!(op, Shl | ShlUnchecked) {
                    a.ins(format!("asl {t}"));
                    for i in 1..n {
                        a.ins(format!("rol {t}+{i}"));
                    }
                } else {
                    if signed {
                        a.ins(format!("lda {t}+{}", n - 1));
                        a.ins("cmp #$80");
                        a.ins(format!("ror {t}+{}", n - 1));
                    } else {
                        a.ins(format!("lsr {t}+{}", n - 1));
                    }
                    for i in (0..n - 1).rev() {
                        a.ins(format!("ror {t}+{i}"));
                    }
                }
                a.ins("dex");
                a.ins(format!("bne {lloop}"));
                a.label(&ldone);
                a.copy(dst, &Val::Mem(ta), n);
            }
            Mul | MulUnchecked | MulWithOverflow | Div | Rem => {
                if n > 8 {
                    self.fatal("128-bit multiplication/division is not supported")
                }
                let mul = matches!(op, Mul | MulUnchecked | MulWithOverflow);
                // x * constant: shifts and additions.
                let inline = mul
                    && match Self::imm_value(&vb, n) {
                        Some(c) => self.mul_const(dst, &va, n, c),
                        None => false,
                    };
                let a = &mut self.p.asm;
                if !inline {
                    a.copy(&Addr::Abs("rt_a".into()), &va, n);
                    a.copy(&Addr::Abs("rt_b".into()), &vb, n);
                    // One and two bytes have their own multiplication.
                    if !mul || n > 2 {
                        a.ins(format!("lda #{n}"));
                        a.ins("sta rt_n");
                    }
                    let (routine, result) = match op {
                        Div => (if signed { "rt_sdiv" } else { "rt_udiv" }, "rt_a"),
                        Rem => (if signed { "rt_sdiv" } else { "rt_udiv" }, "rt_r"),
                        _ if n == 1 => ("rt_mul8", "rt_r"),
                        _ if n == 2 => ("rt_mul16", "rt_r"),
                        _ => ("rt_mul", "rt_r"),
                    };
                    a.ins(format!("jsr {routine}"));
                    a.copy(dst, &Val::Mem(Addr::Abs(result.into())), n);
                }
                if op == MulWithOverflow {
                    // TODO: detect overflow (needs the full-width product).
                    let fo = dl.fields.offset(1).bytes();
                    a.copy(&dst.offset(fo), &Val::imm_bytes(&[0]), 1);
                }
            }
            Offset => {
                let pointee = la.ty.builtin_deref(true).unwrap();
                let elem = self.layout(pointee).size.bytes();
                // A constant offset: add it directly; p = p + 1 in place.
                if let Some(c) = Self::imm_value(&vb, lb.size.bytes().min(8)) {
                    let k = c.wrapping_mul(elem) & 0xffff;
                    if k == 1
                        && let (Val::Mem(Addr::Abs(s)), Addr::Abs(d)) = (&va, dst)
                        && s == d
                    {
                        let d = d.clone();
                        Self::inc_dec(&mut self.p.asm, &d, 2, true);
                        return;
                    }
                    let kv = Val::imm_u64(k, 2);
                    let a = &mut self.p.asm;
                    if Self::add_in_place(a, dst, &va, &kv, 2, true) {
                        return;
                    }
                    a.setup_val(&va, 0);
                    a.setup(dst, 2);
                    a.ins("clc");
                    for i in 0..2 {
                        a.lda(&va, i, 0);
                        a.op_byte("adc", &kv, i, 1);
                        a.sta(dst, i, 2);
                    }
                    return;
                }
                if elem == 1 {
                    let vb = if lb.size.bytes() >= 2 {
                        vb.clone()
                    } else if let Some(b) = vb.byte(0) {
                        // one byte, zero-extended
                        Val::Imm(vec![b, "#$00".into()])
                    } else {
                        let t = self.temp(1);
                        self.a().copy(&Addr::Abs(t.clone()), &vb, 1);
                        Val::Imm(vec![t, "#$00".into()])
                    };
                    let a = &mut self.p.asm;
                    if Self::add_in_place(a, dst, &va, &vb, 2, true) {
                        return;
                    }
                    a.setup_val(&va, 0);
                    a.setup_val(&vb, 1);
                    a.setup(dst, 2);
                    a.ins("clc");
                    for i in 0..2 {
                        a.lda(&va, i, 0);
                        a.op_byte("adc", &vb, i, 1);
                        a.sta(dst, i, 2);
                    }
                    return;
                }
                let t = self.scaled(&vb, lb.size.bytes(), elem);
                let a = &mut self.p.asm;
                a.setup_val(&va, 0);
                a.setup(dst, 2);
                a.ins("clc");
                for i in 0..2 {
                    a.lda(&va, i, 0);
                    a.ins(if i == 0 { format!("adc {t}") } else { format!("adc {t}+1") });
                    a.sta(dst, i, 2);
                }
            }
        }
    }

    /// Computes `p - q` (n bytes) for the carry: clear if p < q.
    fn compare(&mut self, p: &Val, q: &Val, n: u64, signed: bool) {
        // Equal constant high bytes (zero-extended values) do not matter;
        // what is left compares unsigned.
        let mut m = n;
        while m > 0 && Self::same_const_byte(p, q, m - 1) {
            m -= 1;
        }
        let (n, signed) = if m < n { (m, false) } else { (n, signed) };
        let a = &mut self.p.asm;
        a.setup_val(p, 0);
        a.setup_val(q, 1);
        if n == 1 && !signed {
            a.lda(p, 0, 0);
            a.op_byte("cmp", q, 0, 1);
            return;
        }
        // Signed p < 0: the sign bit, inverted, into the carry.
        if signed && n > 0 && (0..n).all(|i| q.byte_is(i, 0)) {
            a.lda(p, n - 1, 0);
            a.ins("eor #$80");
            a.ins("asl");
            return;
        }
        a.ins("sec");
        for i in 0..n {
            if signed && i == n - 1 {
                // Signed: p < q  <=>  N != V after the subtraction. Flip N
                // unless V is set and move it into the carry (set: p >= q).
                let l = a.new_label();
                a.lda(p, i, 0);
                a.op_byte("sbc", q, i, 1);
                a.ins(format!("bvs {l}"));
                a.ins("eor #$80");
                a.label(&l);
                a.ins("asl");
            } else {
                a.lda(p, i, 0);
                a.op_byte("sbc", q, i, 1);
            }
        }
    }

    fn unop(&mut self, op: UnOp, x: &Operand<'tcx>, dst: &Addr, size: u64) {
        let (v, l) = self.operand(x);
        let a = &mut self.p.asm;
        match op {
            UnOp::Not => {
                let mask = if l.ty.is_bool() { "#1" } else { "#$ff" };
                a.setup_val(&v, 0);
                a.setup(dst, 2);
                for i in 0..size {
                    a.lda(&v, i, 0);
                    a.ins(format!("eor {mask}"));
                    a.sta(dst, i, 2);
                }
            }
            UnOp::Neg => {
                if l.ty.is_floating_point() {
                    self.fatal("floating point is not supported yet")
                }
                a.setup_val(&v, 0);
                a.setup(dst, 2);
                a.ins("sec");
                for i in 0..size {
                    a.ins("lda #0");
                    a.op_byte("sbc", &v, i, 0);
                    a.sta(dst, i, 2);
                }
            }
            UnOp::PtrMetadata => {
                if size > 0 {
                    a.copy(dst, &v.offset(2), size);
                }
            }
        }
    }

    // ---- enums ----

    fn get_discr(&mut self, src: &Addr, layout: TyAndLayout<'tcx>, dst: &Addr, dsize: u64) {
        let tcx = self.tcx();
        match &layout.variants {
            Variants::Empty => {}
            Variants::Single { index } => {
                let d = layout
                    .ty
                    .discriminant_for_variant(tcx, *index)
                    .map_or(index.as_u32() as u128, |d| d.val);
                self.a().copy(dst, &Val::imm_u64(d as u64, dsize), dsize);
            }
            Variants::Multiple { tag, tag_field, tag_encoding, .. } => {
                let tag_addr = src.offset(layout.fields.offset(tag_field.as_usize()).bytes());
                let tsize = tag.size(&self.p.cx).bytes();
                let tag_signed = matches!(tag.primitive(), Primitive::Int(_, true));
                match *tag_encoding {
                    TagEncoding::Direct => {
                        self.extend(dst, &Val::Mem(tag_addr), tsize, dsize, tag_signed);
                    }
                    TagEncoding::Niche { untagged_variant, ref niche_variants, niche_start } => {
                        let start = niche_variants.start.as_u32() as u64;
                        let relative_max = niche_variants.last.as_u32() as u64 - start;
                        // t = tag - niche_start
                        let t = self.temp(tsize);
                        let tv = Val::Mem(Addr::Abs(t.clone()));
                        let ns = Val::imm_u64(niche_start as u64, tsize);
                        let tag_v = Val::Mem(tag_addr);
                        let a = &mut self.p.asm;
                        a.setup_val(&tag_v, 0);
                        a.ins("sec");
                        for i in 0..tsize {
                            a.lda(&tag_v, i, 0);
                            a.op_byte("sbc", &ns, i, 1);
                            a.ins(format!("sta {t}+{i}"));
                        }
                        // relative_max < t: not a niche value -> untagged variant
                        let rm = Val::imm_u64(relative_max, tsize);
                        self.compare(&rm, &tv, tsize, false);
                        let a = &mut self.p.asm;
                        let (lniche, lend) = (a.new_label(), a.new_label());
                        a.ins(format!("bcs {lniche}"));
                        a.copy(dst, &Val::imm_u64(untagged_variant.as_u32() as u64, dsize), dsize);
                        a.ins(format!("jmp {lend}"));
                        a.label(&lniche);
                        // dst = t + start
                        let tl = Addr::Abs(t.clone());
                        self.extend(dst, &Val::Mem(tl), tsize, dsize, false);
                        let a = &mut self.p.asm;
                        if start != 0 {
                            let sv = Val::imm_u64(start, dsize);
                            a.setup(dst, 2);
                            a.ins("clc");
                            for i in 0..dsize {
                                a.lda(&Val::Mem(dst.clone()), i, 2);
                                a.op_byte("adc", &sv, i, 1);
                                a.sta(dst, i, 2);
                            }
                        }
                        a.label(&lend);
                    }
                }
            }
        }
    }

    fn set_discr(&mut self, dst: &Addr, layout: TyAndLayout<'tcx>, v: VariantIdx) {
        let tcx = self.tcx();
        if let Variants::Multiple { tag, tag_field, tag_encoding, .. } = &layout.variants {
            let tag_addr = dst.offset(layout.fields.offset(tag_field.as_usize()).bytes());
            let tsize = tag.size(&self.p.cx).bytes();
            let value = match *tag_encoding {
                TagEncoding::Direct => layout.ty.discriminant_for_variant(tcx, v).unwrap().val,
                TagEncoding::Niche { untagged_variant, ref niche_variants, niche_start } => {
                    if v == untagged_variant {
                        return;
                    }
                    ((v.as_u32() - niche_variants.start.as_u32()) as u128)
                        .wrapping_add(niche_start)
                }
            };
            self.a().copy(&tag_addr, &Val::imm_u64(value as u64, tsize), tsize);
        }
    }

    fn aggregate(
        &mut self,
        kind: &AggregateKind<'tcx>,
        ops: &rustc_index::IndexVec<rustc_abi::FieldIdx, Operand<'tcx>>,
        dst: &Addr,
        dl: TyAndLayout<'tcx>,
    ) {
        match kind {
            AggregateKind::Array(_) => {
                let stride = dl.field(&self.p.cx, 0).size.bytes();
                for (i, op) in ops.iter().enumerate() {
                    let (v, l) = self.operand(op);
                    self.a().copy(&dst.offset(i as u64 * stride), &v, l.size.bytes());
                }
            }
            AggregateKind::Adt(_, variant, _, _, active_field) => {
                let vl = dl.for_variant(&self.p.cx, *variant);
                if let Some(af) = active_field {
                    let (v, l) = self.operand(&ops[rustc_abi::FieldIdx::from_u32(0)]);
                    let off = vl.fields.offset(af.as_usize()).bytes();
                    self.a().copy(&dst.offset(off), &v, l.size.bytes());
                } else {
                    for (i, op) in ops.iter_enumerated() {
                        let (v, l) = self.operand(op);
                        let off = vl.fields.offset(i.as_usize()).bytes();
                        self.a().copy(&dst.offset(off), &v, l.size.bytes());
                    }
                }
                self.set_discr(dst, dl, *variant);
            }
            AggregateKind::RawPtr(..) => {
                let mut off = 0;
                for op in ops.iter() {
                    let (v, l) = self.operand(op);
                    self.a().copy(&dst.offset(off), &v, l.size.bytes());
                    off += l.size.bytes();
                }
            }
            _ => {
                // Tuples, closures.
                for (i, op) in ops.iter_enumerated() {
                    let (v, l) = self.operand(op);
                    let off = dl.fields.offset(i.as_usize()).bytes();
                    self.a().copy(&dst.offset(off), &v, l.size.bytes());
                }
            }
        }
    }

    // ---- terminators ----

    fn goto(&mut self, target: BasicBlock, next: Option<BasicBlock>) {
        if Some(target) != next {
            let l = self.block(target);
            self.a().ins(format!("jmp {l}"));
        }
    }

    fn terminator(&mut self, kind: &TerminatorKind<'tcx>, next: Option<BasicBlock>) {
        match kind {
            TerminatorKind::Goto { target, .. } => self.goto(*target, next),
            TerminatorKind::FalseEdge { real_target, .. }
            | TerminatorKind::FalseUnwind { real_target, .. } => self.goto(*real_target, next),
            TerminatorKind::Return => self.a().ins("rts"),
            TerminatorKind::Unreachable
            | TerminatorKind::UnwindResume
            | TerminatorKind::UnwindTerminate(_) => self.a().ins("jmp rt_panic"),
            TerminatorKind::SwitchInt { discr, targets } => {
                if let Some(c) = self.const_bits(discr) {
                    self.goto(targets.target_for_value(c), next);
                    return;
                }
                let (v, l) = self.operand(discr);
                let n = l.size.bytes();
                self.a().setup_val(&v, 0);
                for (value, target) in targets.iter() {
                    let tl = self.block(target);
                    let a = &mut self.p.asm;
                    let lnext = a.new_label();
                    for i in 0..n {
                        a.lda(&v, i, 0);
                        let byte = (value >> (8 * i)) as u8;
                        if byte != 0 {
                            a.ins(format!("cmp #${byte:02x}"));
                        }
                        a.ins(format!("bne {lnext}"));
                    }
                    a.ins(format!("jmp {tl}"));
                    a.label(&lnext);
                }
                self.goto(targets.otherwise(), next);
            }
            TerminatorKind::Assert { cond, expected, target, .. } => {
                let (v, _) = self.operand(cond);
                let a = &mut self.p.asm;
                a.setup_val(&v, 0);
                a.lda(&v, 0, 0);
                let ok = a.new_label();
                a.ins(format!("{} {ok}", if *expected { "bne" } else { "beq" }));
                a.ins("jmp rt_panic");
                a.label(&ok);
                self.goto(*target, next);
            }
            TerminatorKind::Drop { place, target, .. } => {
                let ty = self.monomorphize(place.ty(self.mir, self.tcx()).ty);
                let inst = Instance::resolve_drop_glue(self.tcx(), ty);
                if !matches!(inst.def, InstanceKind::Shim(ShimKind::DropGlue(_, None))) {
                    let (a, _, _) = self.place(*place);
                    let id = self.p.func_id(inst, self.ctx);
                    self.p.calls[self.id].push(id);
                    self.a().addr_to(&a, &format!("L{id}_1"));
                    self.a().ins(format!("jsr F{id}"));
                }
                self.goto(*target, next);
            }
            TerminatorKind::Call { func, args, destination, target, .. } => {
                let args: Vec<Operand<'tcx>> = args.iter().map(|a| a.node.clone()).collect();
                self.call(func, &args, *destination);
                if let Some(t) = target {
                    self.goto(*t, next);
                }
            }
            TerminatorKind::TailCall { func, args, .. } => {
                let args: Vec<Operand<'tcx>> = args.iter().map(|a| a.node.clone()).collect();
                self.call(func, &args, Place::return_place());
                self.a().ins("rts");
            }
            TerminatorKind::InlineAsm { template, operands, targets, .. } => {
                self.inline_asm(template, operands);
                match targets.first() {
                    Some(t) => self.goto(*t, next),
                    // options(noreturn): the code does not come back
                    None => {}
                }
            }
            other => self.fatal(format!("{other:?} is not supported")),
        }
    }

    /// `asm!`: the lines of the template become part of the function.
    ///
    /// Values live in memory here, not in registers, so a placeholder
    /// stands for the memory location of its operand (an address
    /// expression: `lda {x}`, `sta {x}+1`), whatever register class the
    /// operand names:
    ///
    /// * `in(reg) value`: where the value is; a constant or a value behind
    ///   a pointer is copied to a temporary first.
    /// * `out(reg) place`, `inout`, `lateout`: where the result goes; a
    ///   place behind a pointer gets a temporary that is copied afterwards.
    /// * `const N`: the number. `sym STATIC`: the label of the static.
    ///
    /// A, X, Y and the flags are free inside the block and undefined
    /// after it. The block gets labels of its own before and after: local
    /// `@labels` of the template stay inside it, and the peephole pass
    /// forgets what it knew about the registers.
    fn inline_asm(&mut self, template: &[InlineAsmTemplatePiece], operands: &[InlineAsmOperand<'tcx>]) {
        let mut names: Vec<String> = Vec::with_capacity(operands.len());
        // (place, temporary, size) to copy after the block
        let mut after: Vec<(Place<'tcx>, String, u64)> = Vec::new();
        for op in operands {
            let name = match op {
                InlineAsmOperand::In { value, .. } => {
                    let (v, l) = self.operand(value);
                    match v {
                        Val::Mem(Addr::Abs(e)) => e,
                        v => {
                            let n = l.size.bytes();
                            let t = self.temp(n.max(1));
                            self.a().copy(&Addr::Abs(t.clone()), &v, n);
                            t
                        }
                    }
                }
                InlineAsmOperand::Out { place, .. } => match place {
                    Some(p) => {
                        let (a, l, _) = self.place(*p);
                        match a {
                            Addr::Abs(e) => e,
                            _ => {
                                let n = l.size.bytes();
                                let t = self.temp(n.max(1));
                                after.push((*p, t.clone(), n));
                                t
                            }
                        }
                    }
                    None => self.temp(2),
                },
                InlineAsmOperand::InOut { in_value, out_place, .. } => {
                    let (v, l) = self.operand(in_value);
                    let n = l.size.bytes();
                    let t = self.temp(n.max(1));
                    self.a().copy(&Addr::Abs(t.clone()), &v, n);
                    if let Some(p) = out_place {
                        after.push((*p, t.clone(), n));
                    }
                    t
                }
                InlineAsmOperand::Const { value } => {
                    let op = Operand::Constant(value.clone());
                    match self.const_bits(&op) {
                        Some(c) => format!("{c}"),
                        None => self.fatal("asm!: the `const` operand is not an integer"),
                    }
                }
                InlineAsmOperand::SymStatic { def_id } => {
                    if self.tcx().is_foreign_item(*def_id) {
                        self.fatal("asm!: extern statics are not supported yet")
                    }
                    format!("S{}", self.p.statics.insert_full(*def_id).0)
                }
                InlineAsmOperand::SymFn { .. } | InlineAsmOperand::Label { .. } => {
                    self.fatal("asm!: `sym` of a function and `label` operands are not supported yet")
                }
            };
            names.push(name);
        }
        let mut text = String::new();
        for piece in template {
            match piece {
                InlineAsmTemplatePiece::String(s) => text.push_str(s),
                InlineAsmTemplatePiece::Placeholder { operand_idx, .. } => text.push_str(&names[*operand_idx]),
            }
        }
        let id = self.id;
        let a = &mut self.p.asm;
        let n = a.new_label();
        let n = n.trim_start_matches("_L");
        a.label(&format!("F{id}_i{n}"));
        for line in text.lines() {
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            // `label:` or `label: instruction` starts in the first column
            let first = t.split_whitespace().next().unwrap_or("");
            if first.ends_with(':') {
                a.raw(t);
            } else {
                a.ins(t);
            }
        }
        a.label(&format!("F{id}_e{n}"));
        for (place, t, n) in after {
            let (d, _, _) = self.place(place);
            self.a().copy(&d, &Val::Mem(Addr::Abs(t)), n);
        }
    }

    fn call(&mut self, func: &Operand<'tcx>, args: &[Operand<'tcx>], dest: Place<'tcx>) {
        let tcx = self.tcx();
        let fty = self.monomorphize(func.ty(self.mir, tcx));
        let ty::FnDef(def_id, fargs) = *fty.kind() else {
            if let ty::FnPtr(..) = fty.kind() {
                self.indirect_call(func, args, dest);
                return;
            }
            self.fatal(format!("calls of `{fty}` are not supported yet"))
        };
        let mut inst =
            Instance::expect_resolve(tcx, TypingEnv::fully_monomorphized(), def_id, fargs.skip_binder(), self.span);
        match inst.def {
            InstanceKind::Intrinsic(_) => {
                if self.intrinsic(inst, args, dest) {
                    return;
                }
                inst = Instance::new_raw(inst.def_id(), inst.args);
            }
            InstanceKind::Shim(ShimKind::DropGlue(_, None)) => return,
            InstanceKind::Virtual(..) => self.fatal("dynamic dispatch is not supported yet"),
            InstanceKind::Item(d) if tcx.is_foreign_item(d) => {
                self.foreign_call(inst, args, dest);
                return;
            }
            // Diverging functions of `core` are panics (`panic_fmt`,
            // `slice_index_fail`, `unwrap_failed`, ...). The target aborts
            // on panic, so don't compile them: they would only pull in
            // `core::fmt`.
            InstanceKind::Item(d)
                if tcx.crate_name(d.krate) == rustc_span::sym::core
                    && fty.fn_sig(tcx).output().skip_binder().is_never() =>
            {
                self.a().ins("jmp rt_panic");
                return;
            }
            _ => {}
        }
        let id = self.p.func_id(inst, self.ctx);
        self.p.calls[self.id].push(id);
        for (i, arg) in args.iter().enumerate() {
            let (v, l) = self.operand(arg);
            self.a().copy(&Addr::Abs(format!("L{id}_{}", i + 1)), &v, l.size.bytes());
        }
        self.a().ins(format!("jsr F{id}"));
        let (d, dl, _) = self.place(dest);
        let size = dl.size.bytes();
        self.a().copy(&d, &Val::Mem(Addr::Abs(format!("L{id}_0"))), size);
    }

    /// A call through a function pointer: the arguments go one after the
    /// other to `rt_args`, the pointer (a uniform entry, see
    /// `fn_ptr_entries`) to `rt_jvec`, the result comes back in `rt_ret`.
    fn indirect_call(&mut self, func: &Operand<'tcx>, args: &[Operand<'tcx>], dest: Place<'tcx>) {
        let mut off = 0;
        for arg in args {
            let (v, l) = self.operand(arg);
            let n = l.size.bytes();
            self.a().copy(&Addr::Abs(format!("rt_args+{off}")), &v, n);
            off += n;
        }
        if off > RT_ARGS {
            self.fatal(format!("{off} bytes of arguments; calls through pointers allow {RT_ARGS}"))
        }
        let (f, _) = self.operand(func);
        self.a().copy(&Addr::Abs("rt_jvec".into()), &f, 2);
        self.a().ins("jsr rt_jsrind");
        let (d, dl, _) = self.place(dest);
        let n = dl.size.bytes();
        if n > RT_RET {
            self.fatal(format!("{n} bytes of result; calls through pointers allow {RT_RET}"))
        }
        self.a().copy(&d, &Val::Mem(Addr::Abs("rt_ret".into())), n);
    }

    /// A call of an assembly function: an `extern "C"` declaration whose
    /// code comes from a rat source. Calling convention (as in go64): all
    /// argument bytes in `<sym>__args`, the first three also in A, X and Y;
    /// a result of up to three bytes comes back in A, X, Y, a larger one in
    /// `<sym>__res`.
    fn foreign_call(&mut self, inst: Instance<'tcx>, args: &[Operand<'tcx>], dest: Place<'tcx>) {
        let sym = self.tcx().symbol_name(inst).name.to_owned();
        let mut off = 0;
        let mut first: Vec<Option<String>> = Vec::new();
        for arg in args {
            let (v, l) = self.operand(arg);
            let n = l.size.bytes();
            self.a().copy(&Addr::Abs(format!("{sym}__args+{off}")), &v, n);
            for i in 0..n {
                // not a numeric address: an I/O register is read once
                first.push(v.byte(i).filter(|b| b.starts_with('#') || !b.contains('$')));
            }
            off += n;
        }
        first.resize(3, None);
        let rsize = self.layout(self.monomorphize(dest.ty(self.mir, self.tcx()).ty)).size.bytes();
        let e = self.p.foreign.entry(sym.clone()).or_default();
        e.0 = e.0.max(off);
        e.1 = e.1.max(rsize);
        // The first three bytes go into A, X and Y: straight from where
        // they are, if that needs no code (the copies in the argument area
        // are removed later if the routine does not read them).
        let a = &mut self.p.asm;
        for (i, r) in ["lda", "ldx", "ldy"].iter().enumerate().take(off.min(3) as usize) {
            match &first[i] {
                Some(b) => a.ins(format!("{r} {b}")),
                None => a.ins(format!("{r} {sym}__args+{i}")),
            }
        }
        a.ins(format!("jsr {sym}"));
        if rsize > 0 {
            let (d, _, _) = self.place(dest);
            match &d {
                Addr::Abs(e) if rsize <= 3 => {
                    for (i, r) in ["sta", "stx", "sty"].iter().enumerate().take(rsize as usize) {
                        self.a().ins(format!("{r} {e}+{i}"));
                    }
                }
                _ => {
                    if rsize <= 3 {
                        for (i, r) in ["sta", "stx", "sty"].iter().enumerate().take(rsize as usize) {
                            self.a().ins(format!("{r} {sym}__res+{i}"));
                        }
                    }
                    self.a().copy(&d, &Val::Mem(Addr::Abs(format!("{sym}__res"))), rsize);
                }
            }
        }
    }

    /// Translates an intrinsic call; false: use its fallback body.
    fn intrinsic(&mut self, inst: Instance<'tcx>, args: &[Operand<'tcx>], dest: Place<'tcx>) -> bool {
        let tcx = self.tcx();
        let name = tcx.item_name(inst.def_id());
        match name.as_str() {
            "volatile_load" | "unaligned_volatile_load" | "read_via_copy" => {
                let src = self.deref_operand(&args[0]);
                let (d, dl, _) = self.place(dest);
                self.a().copy(&d, &Val::Mem(src), dl.size.bytes());
            }
            "volatile_store" | "unaligned_volatile_store" | "nontemporal_store"
            | "write_via_move" => {
                let dst = self.deref_operand(&args[0]);
                let (v, vl) = self.operand(&args[1]);
                self.a().copy(&dst, &v, vl.size.bytes());
            }
            "copy_nonoverlapping" => {
                let (src, sl) = self.operand(&args[0]);
                let (dst, _) = self.operand(&args[1]);
                let (count, cl) = self.operand(&args[2]);
                let elem = self.layout(sl.ty.builtin_deref(true).unwrap()).size.bytes();
                self.memcpy_dyn(&dst, &src, &count, cl.size.bytes(), elem);
            }
            "write_bytes" => {
                let (dst, dl) = self.operand(&args[0]);
                let (val, _) = self.operand(&args[1]);
                let (count, cl) = self.operand(&args[2]);
                let elem = self.layout(dl.ty.builtin_deref(true).unwrap()).size.bytes();
                let n = self.scaled(&count, cl.size.bytes(), elem);
                let d = self.deref_val(&dst);
                let a = &mut self.p.asm;
                a.addr_to(&d, "zp_p1");
                a.copy(&Addr::Abs("rt_n".into()), &Val::Mem(Addr::Abs(n)), 2);
                a.setup_val(&val, 0);
                a.lda(&val, 0, 0);
                a.ins("jsr rt_memset");
            }
            "raw_eq" => {
                // raw_eq(a: &T, b: &T) -> bool: the bytes of *a and *b are equal.
                let (pa, la) = self.operand(&args[0]);
                let (pb, _) = self.operand(&args[1]);
                let size = self.layout(la.ty.builtin_deref(true).unwrap()).size.bytes();
                let da = self.deref_val(&pa);
                let db = self.deref_val(&pb);
                let (d, _, _) = self.place(dest);
                let a = &mut self.p.asm;
                a.addr_to(&da, "zp_p0");
                a.addr_to(&db, "zp_p1");
                a.copy(&Addr::Abs("rt_n".into()), &Val::imm_u64(size, 2), 2);
                a.ins("jsr rt_memcmp");
                let (leq, lend) = (a.new_label(), a.new_label());
                a.ins("cmp #0");
                a.ins(format!("beq {leq}"));
                a.ins("lda #0");
                a.ins(format!("jmp {lend}"));
                a.label(&leq);
                a.ins("lda #1");
                a.label(&lend);
                a.ins("sta rt_r");
                a.copy(&d, &Val::Mem(Addr::Abs("rt_r".into())), 1);
            }
            "compare_bytes" => {
                // compare_bytes(a: *const u8, b: *const u8, n: usize) -> i32 (memcmp)
                let (pa, _) = self.operand(&args[0]);
                let (pb, _) = self.operand(&args[1]);
                let (n, nl) = self.operand(&args[2]);
                let da = self.deref_val(&pa);
                let db = self.deref_val(&pb);
                let (d, dl, _) = self.place(dest);
                let a = &mut self.p.asm;
                a.copy(&Addr::Abs("rt_n".into()), &n, nl.size.bytes().min(2));
                a.addr_to(&da, "zp_p0");
                a.addr_to(&db, "zp_p1");
                a.ins("jsr rt_memcmp");
                a.ins("sta rt_r");
                let r = Val::Mem(Addr::Abs("rt_r".into()));
                self.extend(&d, &r, 1, dl.size.bytes(), true);
            }
            "wrapping_add" | "unchecked_add" | "wrapping_sub" | "unchecked_sub"
            | "wrapping_mul" | "unchecked_mul" | "unchecked_div" | "exact_div"
            | "unchecked_rem" | "unchecked_shl" | "unchecked_shr" | "add_with_overflow"
            | "sub_with_overflow" | "mul_with_overflow" | "three_way_compare"
            | "disjoint_bitor" | "offset" | "arith_offset" => {
                let op = match name.as_str() {
                    "wrapping_add" | "unchecked_add" => BinOp::Add,
                    "wrapping_sub" | "unchecked_sub" => BinOp::Sub,
                    "wrapping_mul" | "unchecked_mul" => BinOp::Mul,
                    "unchecked_div" | "exact_div" => BinOp::Div,
                    "unchecked_rem" => BinOp::Rem,
                    "unchecked_shl" => BinOp::Shl,
                    "unchecked_shr" => BinOp::Shr,
                    "add_with_overflow" => BinOp::AddWithOverflow,
                    "sub_with_overflow" => BinOp::SubWithOverflow,
                    "mul_with_overflow" => BinOp::MulWithOverflow,
                    "three_way_compare" => BinOp::Cmp,
                    "disjoint_bitor" => BinOp::BitOr,
                    _ => BinOp::Offset,
                };
                let (d, dl, _) = self.place(dest);
                self.binop(op, &args[0], &args[1], &d, dl);
            }
            "ptr_offset_from" | "ptr_offset_from_unsigned" => {
                let (va, la) = self.operand(&args[0]);
                let (vb, _) = self.operand(&args[1]);
                let elem = self.layout(la.ty.builtin_deref(true).unwrap()).size.bytes();
                let signed = name.as_str() == "ptr_offset_from";
                let t = self.temp(2);
                let a = &mut self.p.asm;
                a.setup_val(&va, 0);
                a.setup_val(&vb, 1);
                a.ins("sec");
                for i in 0..2 {
                    a.lda(&va, i, 0);
                    a.op_byte("sbc", &vb, i, 1);
                    a.ins(format!("sta {t}+{i}"));
                }
                if elem.is_power_of_two() {
                    for _ in 0..elem.trailing_zeros() {
                        if signed {
                            a.ins(format!("lda {t}+1"));
                            a.ins("cmp #$80");
                            a.ins(format!("ror {t}+1"));
                        } else {
                            a.ins(format!("lsr {t}+1"));
                        }
                        a.ins(format!("ror {t}"));
                    }
                } else {
                    a.copy(&Addr::Abs("rt_a".into()), &Val::Mem(Addr::Abs(t.clone())), 2);
                    a.copy(&Addr::Abs("rt_b".into()), &Val::imm_u64(elem, 2), 2);
                    a.ins("lda #2");
                    a.ins("sta rt_n");
                    a.ins(if signed { "jsr rt_sdiv" } else { "jsr rt_udiv" });
                    a.copy(&Addr::Abs(t.clone()), &Val::Mem(Addr::Abs("rt_a".into())), 2);
                }
                let (d, _, _) = self.place(dest);
                self.a().copy(&d, &Val::Mem(Addr::Abs(t)), 2);
            }
            "ctpop" | "ctlz" | "ctlz_nonzero" | "cttz" | "cttz_nonzero" => {
                let (v, l) = self.operand(&args[0]);
                let n = l.size.bytes();
                let routine = match name.as_str() {
                    "ctpop" => "rt_ctpop",
                    "ctlz" | "ctlz_nonzero" => "rt_ctlz",
                    _ => "rt_cttz",
                };
                let (d, dl, _) = self.place(dest);
                let a = &mut self.p.asm;
                a.copy(&Addr::Abs("rt_a".into()), &v, n);
                a.ins(format!("lda #{n}"));
                a.ins("sta rt_n");
                a.ins(format!("jsr {routine}"));
                a.ins("sta rt_r");
                let r = Val::Mem(Addr::Abs("rt_r".into()));
                self.extend(&d, &r, 1, dl.size.bytes(), false);
            }
            "saturating_add" | "saturating_sub" => {
                // Unsigned: on a carry (a borrow) the result is all ones
                // (zero).
                let (va, l) = self.operand(&args[0]);
                let (vb, _) = self.operand(&args[1]);
                if Self::is_signed(l.ty) {
                    self.fatal(format!("intrinsic `{name}` is not supported yet for signed integers"))
                }
                let n = l.size.bytes();
                let add = name.as_str() == "saturating_add";
                let (d, _, _) = self.place(dest);
                let t = self.temp(n);
                let a = &mut self.p.asm;
                a.setup_val(&va, 0);
                a.setup_val(&vb, 1);
                a.ins(if add { "clc" } else { "sec" });
                for i in 0..n {
                    a.lda(&va, i, 0);
                    a.op_byte(if add { "adc" } else { "sbc" }, &vb, i, 1);
                    a.ins(format!("sta {t}+{i}"));
                }
                let ok = a.new_label();
                a.ins(format!("{} {ok}", if add { "bcc" } else { "bcs" }));
                a.ins(if add { "lda #$ff" } else { "lda #$00" });
                for i in 0..n {
                    a.ins(format!("sta {t}+{i}"));
                }
                a.label(&ok);
                a.copy(&d, &Val::Mem(Addr::Abs(t)), n);
            }
            "bswap" => {
                let (v, l) = self.operand(&args[0]);
                let n = l.size.bytes();
                let (d, _, _) = self.place(dest);
                let t = self.temp(n);
                let a = &mut self.p.asm;
                a.setup_val(&v, 0);
                for i in 0..n {
                    a.lda(&v, i, 0);
                    a.ins(format!("sta {t}+{}", n - 1 - i));
                }
                a.copy(&d, &Val::Mem(Addr::Abs(t)), n);
            }
            "rotate_left" | "rotate_right" => {
                let (v, l) = self.operand(&args[0]);
                let (k, _) = self.operand(&args[1]);
                let n = l.size.bytes();
                let (d, _, _) = self.place(dest);
                let t = self.temp(n);
                let ta = Addr::Abs(t.clone());
                self.a().copy(&ta, &v, n);
                let a = &mut self.p.asm;
                a.setup_val(&k, 1);
                a.lda(&k, 0, 1);
                a.ins(format!("and #{}", 8 * n - 1));
                a.ins("tax");
                let (lloop, ldone) = (a.new_label(), a.new_label());
                a.ins(format!("beq {ldone}"));
                a.label(&lloop);
                if name.as_str() == "rotate_left" {
                    a.ins(format!("lda {t}+{}", n - 1));
                    a.ins("asl"); // carry = top bit
                    for i in 0..n {
                        a.ins(format!("rol {t}+{i}"));
                    }
                } else {
                    a.ins(format!("lda {t}"));
                    a.ins("lsr"); // carry = bit 0
                    for i in (0..n).rev() {
                        a.ins(format!("ror {t}+{i}"));
                    }
                }
                a.ins("dex");
                a.ins(format!("bne {lloop}"));
                a.label(&ldone);
                a.copy(&d, &Val::Mem(ta), n);
            }
            "const_eval_select" => {
                // const_eval_select(args, compile_time_fn, runtime_fn): runtime_fn(args...)
                let fty = self.monomorphize(args[2].ty(self.mir, tcx));
                let ty::FnDef(def_id, fargs) = *fty.kind() else {
                    self.fatal("const_eval_select with a non-function")
                };
                let callee = Instance::expect_resolve(
                    tcx,
                    TypingEnv::fully_monomorphized(),
                    def_id,
                    fargs.skip_binder(),
                    self.span,
                );
                let id = self.p.func_id(callee, self.ctx);
                self.p.calls[self.id].push(id);
                let (tv, tl) = self.operand(&args[0]);
                for i in 0..tl.fields.count() {
                    let off = tl.fields.offset(i).bytes();
                    let fsize = tl.field(&self.p.cx, i).size.bytes();
                    self.a().copy(&Addr::Abs(format!("L{id}_{}", i + 1)), &tv.offset(off), fsize);
                }
                self.a().ins(format!("jsr F{id}"));
                let (d, dl, _) = self.place(dest);
                self.a().copy(&d, &Val::Mem(Addr::Abs(format!("L{id}_0"))), dl.size.bytes());
            }
            "discriminant_value" => {
                let (p, pl) = self.operand(&args[0]);
                let src = self.deref_val(&p);
                let l = self.layout(pl.ty.builtin_deref(true).unwrap());
                let (d, dl, _) = self.place(dest);
                self.get_discr(&src, l, &d, dl.size.bytes());
            }
            "select_unpredictable" => {
                let (c, _) = self.operand(&args[0]);
                let (x, xl) = self.operand(&args[1]);
                let (y, _) = self.operand(&args[2]);
                let (d, _, _) = self.place(dest);
                let n = xl.size.bytes();
                let a = &mut self.p.asm;
                a.setup_val(&c, 0);
                a.lda(&c, 0, 0);
                let (lelse, lend) = (a.new_label(), a.new_label());
                a.ins(format!("beq {lelse}"));
                a.copy(&d, &x, n);
                a.ins(format!("jmp {lend}"));
                a.label(&lelse);
                a.copy(&d, &y, n);
                a.label(&lend);
            }
            "size_of_val" | "align_of_val" => {
                let (p, pl) = self.operand(&args[0]);
                let pointee = pl.ty.builtin_deref(true).unwrap();
                let l = self.layout(pointee);
                let (d, _, _) = self.place(dest);
                if name.as_str() == "align_of_val" {
                    self.a().copy(&d, &Val::imm_u64(l.align.bytes(), 2), 2);
                } else if l.is_sized() {
                    self.a().copy(&d, &Val::imm_u64(l.size.bytes(), 2), 2);
                } else if let ty::Slice(_) | ty::Str = pointee.kind() {
                    let elem = l.field(&self.p.cx, 0).size.bytes();
                    let t = self.scaled(&p.offset(2), 2, elem);
                    self.a().copy(&d, &Val::Mem(Addr::Abs(t)), 2);
                } else {
                    self.fatal(format!("size_of_val of `{pointee}` is not supported yet"))
                }
            }
            "aggregate_raw_ptr" => {
                let (d, _, _) = self.place(dest);
                let (data, _) = self.operand(&args[0]);
                let (meta, ml) = self.operand(&args[1]);
                self.a().copy(&d, &data, 2);
                self.a().copy(&d.offset(2), &meta, ml.size.bytes());
            }
            "ptr_metadata" => {
                let (p, _) = self.operand(&args[0]);
                let (d, dl, _) = self.place(dest);
                self.a().copy(&d, &p.offset(2), dl.size.bytes());
            }
            "is_val_statically_known" => {
                let (d, _, _) = self.place(dest);
                self.a().copy(&d, &Val::imm_bytes(&[0]), 1);
            }
            "ptr_guaranteed_cmp" => {
                let (d, _, _) = self.place(dest);
                self.a().copy(&d, &Val::imm_bytes(&[2]), 1); // unknown
            }
            "assert_inhabited" | "assert_zero_valid" | "assert_mem_uninitialized_valid"
            | "forget" => {}
            "abort" | "abort_immediate" => self.a().ins("jmp rt_panic"),
            "unreachable" => self.a().ins("jmp rt_panic"),
            "assume" | "cold_path" => {}
            "likely" | "unlikely" | "black_box" | "transmute" | "transmute_unchecked" => {
                let (v, _) = self.operand(&args[0]);
                let (d, dl, _) = self.place(dest);
                self.a().copy(&d, &v, dl.size.bytes());
            }
            "caller_location" => {
                let (d, _, _) = self.place(dest);
                self.a().copy(&d, &Val::Imm(Self::ptr_bytes("rt_loc", 0)), 2);
            }
            "ub_checks" | "contract_checks" | "overflow_checks" => {
                let (d, _, _) = self.place(dest);
                self.a().copy(&d, &Val::imm_bytes(&[0]), 1);
            }
            _ => {
                if let Some(i) = tcx.intrinsic(inst.def_id())
                    && !i.must_be_overridden
                {
                    return false;
                }
                self.fatal(format!("intrinsic `{name}` is not supported yet"))
            }
        }
        true
    }
}
