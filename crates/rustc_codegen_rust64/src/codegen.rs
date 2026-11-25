//! Whole-program translation of MIR to 6510 assembly.
//!
//! The model (see PLAN.md, section 3):
//!
//! * Every function has a static frame in the BSS: its locals and
//!   temporaries live at fixed addresses. The first three bytes of the
//!   arguments are passed in A, X and Y (the callee stores them into its
//!   argument locals, unless it can use them as they are); the caller
//!   copies the rest into the callee's argument locals (`L<callee>_<n>`).
//!   A result of up to three bytes comes back in A, X and Y, a larger one
//!   in `L<callee>_0`. Recursion is rejected.
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
use crate::inline::Func;
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
/// The registers of the runtime routines: name, offset in their block and
/// size (see `runtime.rat`).
const RT_REGS: [(&str, u64, u64); 8] = [
    ("rt_cnt", 0, 1),
    ("rt_sgn", 1, 1),
    ("rt_sgnq", 2, 1),
    ("rt_n", 3, 4),
    ("rt_a", 7, 8),
    ("rt_b", 15, 8),
    ("rt_r", 23, 8),
    ("rt_t", 31, 8),
];

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
    /// A function and the inlined functions whose frames its text treats
    /// as its own (`inline_fns`): their bytes may be in use across the
    /// calls of the function.
    merged: Vec<(usize, Vec<usize>)>,
    /// The registers each function reads at its entry (bit 0: A, 1: X,
    /// 2: Y), as far as known: an argument byte that is not read is not
    /// loaded by the callers.
    entry_regs: FxIndexMap<usize, u8>,
    /// The tables of matches that became a lookup (`T<n>`), as assembly
    /// for the data of the program, and their number.
    tables: String,
    ntables: usize,
    /// Bytes of arguments in memory that are the same constant in every
    /// call of a function (`const_args`): name of the slot, offset, value.
    const_args: FxIndexMap<usize, Vec<(String, u64, u8)>>,
    /// What each function stores to the argument slots of others, as its
    /// last optimization saw it (the value if it is a constant).
    arg_stores: FxIndexMap<usize, Vec<(String, u64, Option<u8>)>>,
    /// For each function: the arguments that are not passed
    /// (`unpassed_args`; `None` while the function is being looked at).
    unpassed: FxIndexMap<Instance<'tcx>, Option<Vec<bool>>>,
    /// The same by the number of the function, once it is compiled.
    unpassed_of: FxIndexMap<usize, Vec<bool>>,
    /// Variables (`F<id>v<n>`) that calls write their result to: whether
    /// no pointer to the variable exists at any of those calls
    /// (`free_dests`).
    free_dests: FxIndexMap<String, bool>,
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
    /// Statics that are all zero: label, size, name (see `emit_static`).
    zero_statics: Vec<(String, u64, String)>,
    /// Addresses of `at` statics without data (the program must end below
    /// them if they are in RAM).
    fixed_vars: Vec<(u64, String)>,
    /// What code compiled for [`CTX_PTR`] (interrupt handlers) uses, and
    /// the interrupt dispatcher therefore has to save: the arithmetic
    /// registers of the runtime, the registers of calls through pointers
    /// (`rt_args`, `rt_ret`), assembly functions (their argument areas).
    ptr_ctx: PtrCtx,
    /// The bytes of the arguments of each function that has been called
    /// or compiled (the first three are passed in registers).
    arg_bytes: FxIndexMap<usize, u64>,
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
        merged: Vec::new(),
        entry_regs: FxIndexMap::default(),
        tables: String::new(),
        ntables: 0,
        const_args: FxIndexMap::default(),
        arg_stores: FxIndexMap::default(),
        unpassed: FxIndexMap::default(),
        unpassed_of: FxIndexMap::default(),
        free_dests: FxIndexMap::default(),
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
        zero_statics: Vec::new(),
        fixed_vars: Vec::new(),
        ptr_ctx: PtrCtx::default(),
        arg_bytes: FxIndexMap::default(),
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
    // Statics that are all zero go to the BSS and are cleared at the
    // start, if that takes less room than their bytes in the file.
    let zero_statics = std::mem::take(&mut p.zero_statics);
    let zero_total: u64 = zero_statics.iter().map(|z| z.1).sum();
    let zero = if zero_total > if zero_total < 256 { 10 } else { 30 } { zero_total } else { 0 };
    if zero > 0 {
        let _ = writeln!(p.defs, "rt_zero = __bss + {}", p.bss);
    }
    for (label, size, name) in zero_statics {
        if zero > 0 {
            let _ = writeln!(p.defs, "{label} = __bss + {}            ; {name}", p.bss);
            p.bss += size;
        } else {
            data.comment(format!("static {name}"));
            data.label(&label);
            for row in 0..size.div_ceil(16) {
                let n = (size - 16 * row).min(16) as usize;
                data.ins(format!(".byte {}", vec!["$00"; n].join(",")));
            }
        }
    }
    // Pointer entries can reach new functions only through data, which is
    // done; the entries themselves call already compiled functions.
    let wrappers = fn_ptr_entries(&mut p);

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
    // (the dispatcher of `c64::irq` is the one place in the library that
    // calls through `rt_jvec`)
    p.ptr_ctx.jvec = p.asm.text.contains("rt_jsrind") || p.rat.matches("rt_jsrind").count() > 1;
    lib.push_str(&ctx_save(p.ptr_ctx));

    // Arguments nobody reads are not passed; what that frees in the
    // callers is cleaned up function by function.
    {
        let program = format!("{}{}{}", p.asm.text, wrappers, data.text);
        let kept = crate::asm::prune(&lib, &program);
        if std::env::var_os("RUST64_NO_OPT").is_none() && std::env::var_os("RUST64_NO_CONST_ARGS").is_none() {
            const_args(&mut p, &[&wrappers, &data.text, &kept]);
        }
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
        let mut chunks: Vec<(usize, String)> = Vec::new();
        for line in code.lines() {
            if let Some(id) = line
                .strip_prefix('F')
                .and_then(|r| r.strip_suffix(':'))
                .and_then(|r| r.parse::<usize>().ok())
            {
                chunks.push((id, String::new()));
            }
            let chunk = match chunks.last_mut() {
                Some((_, chunk)) => chunk,
                None => &mut text,
            };
            chunk.push_str(line);
            chunk.push('\n');
        }
        // (the comment lines at the end of a function's text belong to the
        // next one)
        let mut funcs: Vec<Func> = Vec::new();
        let mut pre: Vec<String> = Vec::new();
        for (id, chunk) in chunks {
            let mut lines: Vec<String> = optimize_fn(&mut p, &chunk, id, &[]).lines().map(str::to_owned).collect();
            let mut tail = Vec::new();
            while lines.last().is_some_and(|l| l.trim().is_empty() || l.trim().starts_with(';')) {
                tail.push(lines.pop().unwrap());
            }
            tail.reverse();
            funcs.push(Func { id, pre: std::mem::replace(&mut pre, tail), lines, live: true });
        }
        if std::env::var_os("RUST64_NO_OPT").is_none() && std::env::var_os("RUST64_NO_INLINE").is_none() {
            let fixed: Vec<&str> = p.fixed.iter().map(|f| f.text.as_str()).collect();
            let mut others: Vec<&str> = vec![&wrappers, &data.text, &lib];
            others.extend(fixed);
            let pinned = inline_roots(&p, main_id, &others);
            inline_fns(&mut p, &mut funcs, pinned);
        }
        for f in funcs.iter().filter(|f| f.live) {
            for l in f.pre.iter().chain(&f.lines) {
                text.push_str(l);
                text.push('\n');
            }
        }
        for l in &pre {
            text.push_str(l);
            text.push('\n');
        }
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
                    if w.ends_with("__args") || w.ends_with("__res") || RT_REGS.iter().any(|r| r.0 == w) {
                        used.insert(w.to_owned());
                    }
                }
            }
        }
        used
    };
    // What goes into the zero page besides the statics and the frames,
    // defined in front of the code (the assembler then takes the short
    // instructions): the registers of the runtime routines and the
    // argument areas of the assembly functions. The library's assembly
    // names them in nearly every line.
    let mut zp_defs = String::new();
    {
        // the registers the program uses, as one block in their order
        // (the routines index from one to the other)
        let regs: Vec<&(&str, u64, u64)> = RT_REGS.iter().filter(|r| used_areas.contains(r.0)).collect();
        let lo = regs.iter().map(|r| r.1).min().unwrap_or(0);
        let hi = regs.iter().map(|r| r.1 + r.2).max().unwrap_or(0);
        let len = hi - lo;
        let zp = len > 0 && std::env::var_os("RUST64_NO_ZP_RT").is_none() && p.zp_next + len <= ZP_LAST + 1;
        let (base, defs) = if zp {
            p.zp_next += len;
            (format!("${:02x}", p.zp_next - len), &mut zp_defs)
        } else {
            (format!("__bss+{}", 160 + lo), &mut p.defs)
        };
        let _ = writeln!(defs, "rt_save = {base}");
        let _ = writeln!(defs, "rt_save_n = {}", len.max(1));
        for r in regs {
            let _ = writeln!(defs, "{} = {base}+{}", r.0, r.1 - lo);
        }
    }
    {
        let areas: Vec<(String, u64)> = p
            .foreign
            .iter()
            .flat_map(|(sym, &(args, res))| [(format!("{sym}__args"), args), (format!("{sym}__res"), res)])
            .filter(|(name, n)| *n > 0 && used_areas.contains(name))
            .collect();
        let len: u64 = areas.iter().map(|a| a.1).sum();
        let zp = len > 0 && std::env::var_os("RUST64_NO_ZP_ARGS").is_none() && p.zp_next + len <= ZP_LAST + 1;
        if zp {
            let _ = writeln!(zp_defs, "rt_fsave = ${:02x}", p.zp_next);
            for (name, n) in areas {
                let _ = writeln!(zp_defs, "{name} = ${:02x}", p.zp_next);
                p.zp_next += n;
            }
            let _ = writeln!(zp_defs, "rt_fsave_end = ${:02x}", p.zp_next);
        } else {
            let _ = writeln!(p.defs, "rt_fsave = __bss + {}", p.bss);
            for (name, n) in areas {
                let _ = writeln!(p.defs, "{name} = __bss + {}", p.bss);
                p.bss += n;
            }
            let _ = writeln!(p.defs, "rt_fsave_end = __bss + {}", p.bss);
        }
    }
    let _ = writeln!(p.defs, "rt_ctxbuf = __bss + {}", p.bss);
    p.bss += 256 + 8;


    // Frames. Functions that are never active at the same time share
    // memory: a frame lies above the frames of everything the function
    // calls (a leaf is at the bottom), so the frames of a call chain never
    // overlap. Each function pointer target (interrupt handlers) gets a
    // region of its own above the main program's: a handler may interrupt
    // anything. What fits into the rest of the zero page goes there: the
    // leaves first, which are called most.
    //
    // A frame has two parts that are placed apart: the small values, and
    // the large ones (structs, arrays). The small parts of all frames come
    // first, so a function's counters and pointers are in the zero page
    // even where a buffer of something it calls would have filled it.
    let frames = compact_frames(&mut p, &[&wrappers, &data.text, &lib]);
    let n = p.funcs.len();
    let mut size = vec![0u64; n];
    let mut part = [vec![0u64; n], vec![0u64; n]];
    for f in &frames {
        size[f.id] = f.size[0] + f.size[1];
        part[0][f.id] = f.size[0];
        part[1][f.id] = f.size[1];
    }
    // What a frame lies above: the frames of the functions it calls. The
    // frame of an inlined function that its holder treats as its own
    // (`merged`) is in use across the holder's calls, unlike that of a
    // function that is called: it also lies above the frames of everything
    // else that is called in the holder's text, and the merged frames of
    // one holder lie above each other.
    let above: Vec<Vec<usize>> = {
        let mut above = p.calls.clone();
        fn reaches(g: &[Vec<usize>], from: usize, to: usize, seen: &mut Vec<bool>) -> bool {
            if from == to {
                return true;
            }
            if std::mem::replace(&mut seen[from], true) {
                return false;
            }
            g[from].iter().any(|&c| reaches(g, c, to, seen))
        }
        for (holder, all) in &p.merged {
            // The calls in the holder's text: its own and those of the
            // functions inlined into it.
            let mut called: Vec<usize> = Vec::new();
            for &f in std::iter::once(holder).chain(all) {
                for &c in &p.calls[f] {
                    if !all.contains(&c) && !called.contains(&c) {
                        called.push(c);
                    }
                }
            }
            // (the small ones at the bottom: they may still fit into the zero page)
            let mut inlined: Vec<usize> = all.iter().copied().filter(|&d| size[d] > 0).collect();
            inlined.sort_by_key(|&d| (size[d], d));
            if std::env::var_os("RUST64_FRAMES").is_some() {
                eprintln!(
                    "merged into F{holder} ({} bytes, calls {called:?}): {:?}",
                    size[*holder],
                    inlined.iter().map(|&d| (d, size[d])).collect::<Vec<_>>()
                );
            }
            for (i, &d) in inlined.iter().enumerate() {
                for &x in called.iter().chain(&inlined[..i]) {
                    // (never the other way round: what calls `d` has its text too)
                    if !above[d].contains(&x) && !reaches(&above, x, d, &mut vec![false; n]) {
                        above[d].push(x);
                    }
                }
            }
        }
        above
    };
    fn place(above: &[Vec<usize>], f: usize, base: u64, size: &[u64], off: &mut Vec<Option<u64>>) -> u64 {
        if let Some(o) = off[f] {
            return o;
        }
        let mut o = base;
        for &c in &above[f] {
            o = o.max(place(above, c, base, size, off) + size[c]);
        }
        off[f] = Some(o);
        o
    }
    // The labels of zero-page frames are defined before the code, so that
    // the assembler picks zero-page addressing.
    let zp_avail = ZP_LAST + 1 - p.zp_next;
    let mut zp_frames = String::new();
    // The names that are in the zero page.
    let mut zp_names: FxIndexSet<String> = FxIndexSet::default();
    // Where the frames in the zero page end.
    let mut zp_frame_ends: Vec<(usize, u64)> = Vec::new();
    let mut top = 0;
    for (k, base) in ["f", "g"].into_iter().enumerate() {
        let start = top;
        let mut off: Vec<Option<u64>> = vec![None; n];
        for root in std::iter::once(main_id).chain(p.addr_taken.keys().copied()) {
            place(&above, root, top, &part[k], &mut off);
            top = (0..n).filter_map(|f| off[f].map(|o| o + part[k][f])).max().unwrap_or(start).max(top);
        }
        for f in &frames {
            let (id, sz) = (f.id, f.size[k]);
            let o = off[id].unwrap_or_else(|| {
                // not reachable from a root: its own space
                top += sz;
                top - sz
            });
            if sz > 0 && o + sz <= zp_avail {
                zp_names.extend(f.names[k].iter().cloned());
                zp_frame_ends.push((id, p.zp_next + o + sz));
                let _ = writeln!(zp_frames, "F{id}{base} = ${:02x}", p.zp_next + o);
                zp_frames.push_str(&f.defs[k]);
            } else {
                let _ = writeln!(p.defs, "F{id}{base} = __bss + {}", p.bss + o);
                p.defs.push_str(&f.defs[k]);
            }
        }
    }
    p.bss += top;
    let in_zp = |name: &str| zp_names.contains(name.split('+').next().unwrap_or(name).trim());

    let mut out = String::new();
    let _ = writeln!(out, "\nrt_main = F{main_id}");
    let zp_init = !p.zp_init.text.is_empty();
    if zp_init {
        out.push_str("\n; ---- zero-page statics ----\nrt_zpinit:\n");
        out.push_str(&p.zp_init.text);
        out.push_str("        rts\n");
    }
    // Pointers in zero-page frames are dereferenced directly.
    // (F<id>v<n>[+off] or F<id>t+off)
    let code = crate::asm::zp_direct(&p.asm.text, in_zp);
    // (a value that was stored for the pointer setup and loaded again)
    let code = crate::asm::peephole(&code);
    // (and the store, if that was the only reader)
    let code = if std::env::var_os("RUST64_NO_OPT").is_none() && std::env::var_os("RUST64_KEEP_STORES").is_none() {
        drop_unread_stores(&code, &[&wrappers, &data.text, &lib], &|name| {
            let id = name
                .strip_prefix('F')
                .and_then(|r| r.split_once(['v', 't']))
                .or_else(|| name.strip_prefix('L').and_then(|r| r.split_once('_')))
                .and_then(|(id, _)| id.parse::<usize>().ok())?;
            let &(o, n) = p.layouts.get(&id)?.0.get(name)?;
            Some((id, o, n))
        })
    } else {
        code
    };
    // Branch distances with the real sizes: frames in the zero page are
    // known now.
    let code = crate::asm::relax_with(
        &code,
        &|op, arg| {
            crate::asm::line_size(op, arg, &|name| name.starts_with("zp_") || in_zp(name))
        },
        120,
    );
    out.push_str(&code);
    out.push_str(&wrappers);
    out.push_str("\n; ---- data ----\n");
    out.push_str(&data.text);
    out.push_str(&p.tables);
    out.push_str("\n; ---- bss (not in the file) ----\n__bss:\n");
    out.push_str(&p.defs);
    let _ = writeln!(out, "__bss_end = __bss + {}", p.bss);
    out.push_str(".assert __bss_end <= $a000, \"rust64: program and its data do not fit below $a000\"\n");
    out.push_str(".assert rt_fsave_end - rt_fsave + 87 <= 256, \"rust64: too many assembly function arguments\"\n");
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
    let irq = hw
        && (crate::asm::touches_irq(&all) || p.fixed_vars.iter().any(|(a, _)| (0x0314..0x031a).contains(a)));
    let start = start_exit(zp_end - ZP_FIRST, hw, irq, exit_used, zp_init, zero);
    let lib = crate::asm::prune(&lib, &format!("{start}{out}"));
    format!(
        "; generated by rust64\n.basicupstart rt_start\n\n; ---- in the zero page ----\n\
         {zp_defs}{zp_frames}\n{start}{lib}{out}"
    )
}

/// The frames with only the bytes the final code refers to: after the
/// optimizations most temporaries and many locals are not used any more.
/// The bytes that are left keep their order and move together, so more
/// frames fit into the zero page. `others` is the text besides the code:
/// the wrappers, then the data and the library assembly.
///
/// A plain operand `name+k` keeps the bytes from the start of `name` up to
/// the one it names (the callers of a function with spread arguments store
/// through the name of the first one); a name used in any other way keeps
/// all its bytes.
fn compact_frames(p: &mut Program<'_>, others: &[&String]) -> Vec<Frame> {
    let frames = std::mem::take(&mut p.frames);
    let mut names: FxIndexMap<String, (usize, u64, u64)> = FxIndexMap::default();
    for (&id, (layout, _)) in &p.layouts {
        for (name, &(o, n)) in layout {
            names.insert(name.clone(), (id, o, n));
        }
    }
    let mut keep: FxIndexMap<usize, Vec<bool>> =
        frames.iter().map(|(id, sz, _)| (*id, vec![false; *sz as usize])).collect();
    let mut used: FxIndexSet<String> = FxIndexSet::default();
    let mut mark = |name: &str, upto: Option<u64>| {
        let Some(&(id, o, n)) = names.get(name) else { return };
        let Some(v) = keep.get_mut(&id) else { return };
        used.insert(name.to_owned());
        let end = match upto {
            Some(k) => o + k + 1,
            None => o + n,
        };
        for b in o..end.min(v.len() as u64) {
            v[b as usize] = true;
        }
    };
    let words = |t: &str, mark: &mut dyn FnMut(&str, Option<u64>)| {
        for w in t.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if !w.is_empty() {
                mark(w, None);
            }
        }
    };
    for line in p.asm.text.lines().chain(others[0].lines()) {
        let t = line.split(';').next().unwrap_or("");
        if line.starts_with(' ')
            && let Some((_, arg)) = t.trim().split_once(' ')
            && let Some((b, k)) = crate::asm::sym_off(arg.trim())
        {
            mark(b, Some(k));
            continue;
        }
        words(t, &mut mark);
    }
    for text in &others[1..] {
        for line in text.lines() {
            words(line.split(';').next().unwrap_or(""), &mut mark);
        }
    }
    let small = std::env::var("RUST64_FRAME_SMALL").ok().and_then(|v| v.parse().ok()).unwrap_or(FRAME_SMALL);
    let mut out = Vec::with_capacity(frames.len());
    for (id, _, _) in frames {
        let kept = &keep[&id];
        // the new place of each byte
        let mut pos = Vec::with_capacity(kept.len() + 1);
        let mut n = 0u64;
        for &k in kept {
            pos.push(n);
            n += k as u64;
        }
        pos.push(n);
        let mut entries: Vec<(&String, u64, u64)> = p.layouts[&id]
            .0
            .iter()
            .filter(|(name, _)| used.contains(*name))
            .map(|(name, &(o, n))| (name, o, n))
            .collect();
        // (a name that lies in another one's bytes after it)
        entries.sort_by(|a, b| (a.1, std::cmp::Reverse(a.2), a.0).cmp(&(b.1, std::cmp::Reverse(b.2), b.0)));
        let mut frame = Frame { id, size: [0; 2], defs: Default::default(), names: Default::default() };
        // the value before: where it starts and ends, its part and its
        // place there. Names in its bytes go with it.
        let mut last: Option<(u64, u64, usize, u64)> = None;
        for (name, o, len) in entries {
            let (k, at) = match last {
                Some((start, end, k, at)) if o < end => (k, at + pos[o as usize] - pos[start as usize]),
                _ => {
                    let end = ((o + len) as usize).min(kept.len()).max(o as usize);
                    let bytes = pos[end] - pos[o as usize];
                    let k = (bytes > small) as usize;
                    let at = frame.size[k];
                    frame.size[k] += bytes;
                    if len > 0 {
                        last = Some((o, o + len, k, at));
                    }
                    (k, at)
                }
            };
            let _ = writeln!(frame.defs[k], "{name} = F{id}{}+{at}", ["f", "g"][k]);
            frame.names[k].push(name.clone());
        }
        debug_assert_eq!(frame.size[0] + frame.size[1], n);
        out.push(frame);
    }
    out
}

/// The largest value that is in the first part of a frame, in bytes.
const FRAME_SMALL: u64 = 4;

/// A function's frame after `compact_frames`, in its two parts (`F<id>f`:
/// the small values, `F<id>g`: the large ones): the sizes, the definitions
/// of the names, and the names.
struct Frame {
    id: usize,
    size: [u64; 2],
    defs: [String; 2],
    names: [Vec<String>; 2],
}

/// The start and exit code of the program.
///
/// * `zp`: the bytes of the zero page from `$02` that hold zero-page
///   statics and frames. They are BASIC's: saved and cleared at the
///   start, restored at the exit.
/// * `hw`: the program can change the interrupt setup, the memory
///   configuration, the VIC mode or bank or the SID
///   ([`crate::asm::touches_hw`]): the exit sets them to what BASIC starts
///   with. Nothing is saved for that: the KERNAL has the values.
/// * `irq`: the program names the interrupt vectors
///   ([`crate::asm::touches_irq`]): the exit sets the IRQ vector to the
///   KERNAL's handler.
/// * `exit_used`: something jumps to `rt_exit` (`sys::exit`, a panic), from
///   any depth of calls: the stack pointer is saved.
/// * `zp_init`: there are zero-page statics to initialize.
/// * `zero`: the bytes of the statics in the BSS (`rt_zero`), cleared at
///   the start.
///
/// A program that needs none of this starts directly with `main`, whose
/// `rts` returns to BASIC.
fn start_exit(zp: u64, hw: bool, irq: bool, exit_used: bool, zp_init: bool, zero: u64) -> String {
    let mut a = Asm::default();
    a.raw("\n; ---- start and exit ----");
    if zp == 0 && !hw && !exit_used && !zp_init && zero == 0 {
        a.raw("rt_start = rt_main");
        return a.text;
    }
    a.label("rt_start");
    // The statics in the BSS start as zero, at every start.
    if zero >= 256 {
        a.ins("lda #<rt_zero");
        a.ins("sta zp_p1");
        a.ins("lda #>rt_zero");
        a.ins("sta zp_p1+1");
        a.ins("lda #0");
        a.ins("tay");
        a.ins(format!("ldx #{}", zero >> 8));
        a.raw("@zpage:");
        a.ins("sta (zp_p1),y");
        a.ins("iny");
        a.ins("bne @zpage");
        a.ins("inc zp_p1+1");
        a.ins("dex");
        a.ins("bne @zpage");
        if zero & 0xff != 0 {
            a.raw("@zrest:");
            a.ins("sta (zp_p1),y");
            a.ins("iny");
            a.ins(format!("cpy #{}", zero & 0xff));
            a.ins("bne @zrest");
        }
    } else if zero > 0 {
        a.ins("lda #0");
        a.ins(format!("ldx #{zero}"));
        a.raw("@zero:");
        a.ins("sta rt_zero-1,x");
        a.ins("dex");
        a.ins("bne @zero");
    }
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
    if hw {
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
    // Back to BASIC from anywhere, with the machine as BASIC starts with
    // it: the memory configuration, the CIAs (timer interrupt on, VIC bank
    // 0) and the SID's volume (the KERNAL's IOINIT), the VIC's mode and
    // interrupts ($D011-$D01A from the KERNAL's table of start values; the
    // colours and the sprites' positions stay, so the program's output
    // stays readable), the IRQ vector, the zero page.
    a.label("rt_exit");
    if hw {
        a.ins("sei");
    }
    if exit_used {
        a.ins("ldx rt_sp");
        a.ins("txs");
    }
    if hw {
        a.ins("lda #$37");
        a.ins("sta $01");
        a.ins("jsr $ff84");
        a.ins("ldx #10");
        a.raw("@vic:");
        a.ins("lda $ecc9,x");
        a.ins("sta $d010,x");
        a.ins("dex");
        a.ins("bne @vic");
        if irq {
            a.ins("lda #$31");
            a.ins("sta $0314");
            a.ins("lda #$ea");
            a.ins("sta $0315");
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
            // All zero (or not initialized) and no pointers: the static
            // can live in the BSS, which is not in the program file.
            let bytes = a.inspect_with_uninit_and_ptr_outside_interpreter(0..a.len());
            if size > 0
                && a.provenance().ptrs().is_empty()
                && bytes.iter().all(|&b| b == 0)
                && !p.exported_statics.contains(&ns)
            {
                p.zero_statics.push((label, size, name));
                return;
            }
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
        // the first three argument bytes also in A, X and Y
        let mut regs: Vec<String> = Vec::new();
        let unpassed = p.unpassed_of.get(&id).cloned().unwrap_or_default();
        'bytes: for (i, ty) in args.iter().enumerate() {
            if unpassed.get(i) == Some(&true) {
                continue;
            }
            for o in 0..p.cx.layout_of(*ty).size.bytes() {
                if regs.len() == 3 {
                    break 'bytes;
                }
                regs.push(format!("L{id}_{}+{o}", i + 1));
            }
        }
        for (load, b) in ["lda", "ldx", "ldy"].iter().zip(&regs) {
            a.ins(format!("{load} {b}"));
        }
        if ret == 0 {
            a.ins(format!("jmp F{id}"));
        } else {
            a.ins(format!("jsr F{id}"));
            if ret <= 3 {
                for (i, store) in ["sta", "stx", "sty"].iter().enumerate().take(ret as usize) {
                    a.ins(format!("{store} rt_ret+{i}"));
                }
            } else {
                a.copy(&Addr::Abs("rt_ret".into()), &Val::Mem(Addr::Abs(format!("L{id}_0"))), ret);
            }
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

/// A local at one statement: it is read there, written there, read later,
/// and read later while it holds a value.
#[derive(Clone, Copy, Default)]
struct Point {
    used: bool,
    def: bool,
    live: bool,
    held: bool,
    /// A reference to it may exist before the statement.
    ptr: bool,
}

/// A local at every statement of the function (per block; the terminator
/// last).
struct Flow {
    points: Vec<Vec<Point>>,
}

impl Flow {
    /// Whether this local and `other` cannot share their bytes: one is
    /// written where the other is read or holds a value that is read
    /// later. The statement `exempt` is the move between the two.
    fn clashes(&self, other: &Flow, exempt: Option<Location>) -> bool {
        for (b, (ra, rb)) in self.points.iter().zip(&other.points).enumerate() {
            for (i, (a, o)) in ra.iter().zip(rb).enumerate() {
                if exempt.is_some_and(|e| e.block.as_usize() == b && e.statement_index == i) {
                    continue;
                }
                if a.def && (o.used || o.held) || o.def && (a.used || a.held) {
                    return true;
                }
            }
        }
        false
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
    /// Pointers that are a pointer with a known address plus a local, in
    /// bytes (`addr_locals`): that pointer and the local.
    sums: Vec<Option<(Local, Local)>>,
    /// Pointers whose value is a pointer local that never changes plus a
    /// constant (`addr_locals`): that local and the constant. For a pointer
    /// to a slice: where it points to.
    syms: Vec<Option<(Local, u64)>>,
    /// Pointers that are such a value plus a local, in bytes.
    sym_sums: Vec<Option<(Local, u64, Local)>>,
    uses: Uses<'tcx>,
    /// Locals that are not stored: single-use temporaries that are a copy,
    /// zero-extension or truncation of another value (see `lazy_locals`).
    lazy_cand: Vec<bool>,
    /// The defining rvalue of each lazy local.
    lazy_def: Vec<Option<&'tcx Rvalue<'tcx>>>,
    /// The real locals each lazy local reads.
    lazy_roots: Vec<Vec<Local>>,
    /// Temporaries that are built where they go (`forwarded`): the local
    /// and the offset in it.
    fwd: Vec<Option<(Local, u64)>>,
    /// Locals that others share their bytes with (`join_locals`).
    joined: Vec<bool>,
    /// Locals that can stay where a call left its result (`result_views`),
    /// and where that is once the call is compiled.
    res_cand: Vec<bool>,
    res_at: Vec<Option<String>>,
    /// Locals of two bytes whose value always fits one (`narrow_locals`):
    /// where they are read, the second byte is the constant 0.
    narrow: Vec<bool>,
    /// The statement that is being compiled.
    cur: Option<(BasicBlock, usize)>,
    /// Statements whose result an earlier one has stored: the other half
    /// of a division and remainder (see `div_partner`).
    done: FxIndexSet<(BasicBlock, usize)>,
    /// Locals that a reference or raw pointer is made to (when asked for).
    borrowed: Option<Vec<bool>>,
}

/// The locals that a statement or terminator names.
struct Mentions(Vec<Local>);

impl<'tcx> Visitor<'tcx> for Mentions {
    fn visit_local(&mut self, local: Local, _: PlaceContext, _: Location) {
        self.0.push(local);
    }
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
        sums: Vec::new(),
        syms: Vec::new(),
        sym_sums: Vec::new(),
        uses: Uses::new(tcx, mir),
        lazy_cand: Vec::new(),
        lazy_def: vec![None; mir.local_decls.len()],
        lazy_roots: Vec::new(),
        fwd: vec![None; mir.local_decls.len()],
        res_cand: Vec::new(),
        res_at: vec![None; mir.local_decls.len()],
        narrow: Vec::new(),
        joined: vec![false; mir.local_decls.len()],
        cur: None,
        done: FxIndexSet::default(),
        borrowed: None,
    };
    let start = f.p.asm.text.len();
    f.addrs = f.addr_locals();
    f.lazy_cand = f.lazy_locals();
    f.fwd = f.forwarded();
    f.join_locals();
    f.res_cand = f.result_views();
    f.narrow = f.narrow_locals();
    f.body();
    f.finish();
    f.free_dests();
    let opt = optimize_fn(f.p, &f.p.asm.text[start..].to_owned(), id, &[]);
    f.p.asm.text.truncate(start);
    f.p.asm.text.push_str(&opt);
    if ctx == CTX_PTR {
        let text = &f.p.asm.text[start..];
        let mut c = f.p.ptr_ctx;
        c.foreign |= text.contains("__args") || text.contains("__res");
        c.zp |= text.contains("zp_p");
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

/// The functions that must stay functions: the program's entry, those
/// whose address is taken, and those that the text besides the code
/// (`others`) names.
fn inline_roots(p: &Program<'_>, main_id: usize, others: &[&str]) -> std::collections::HashSet<usize> {
    let mut pinned = std::collections::HashSet::new();
    pinned.insert(main_id);
    pinned.extend(p.addr_taken.keys().copied());
    for text in others {
        crate::inline::named(text, &mut pinned);
    }
    pinned
}

/// Replaces calls by the text of the called function (see `inline.rs`),
/// where that makes the program smaller or no larger and faster:
///
/// * a function with one call in the whole program,
/// * a function whose text is not longer than what its calls save: with
///   `n` calls of a text of `b` bytes, `n * (b - 3) <= b + 1` (a `jsr` is
///   three bytes, and the function with its `rts` goes away).
///
/// All calls of a function are replaced or none, so the function itself
/// always goes away. The functions are looked at callees first: the text
/// of a function is final (its own calls replaced, optimized again) when it
/// is its turn.
///
/// A caller is optimized again with the text in it. The frame of the
/// inlined function still exists (`Program::calls` and the frames are as
/// before); if all of the function's text is now in one function, that
/// function is the only one to use the frame, and its bytes are followed
/// like those of the function's own frame (`optimize_fn`, `extra`).
fn inline_fns(p: &mut Program<'_>, funcs: &mut [Func], mut pinned: std::collections::HashSet<usize>) {
    use std::collections::{BTreeSet, HashMap, HashSet};
    let index: HashMap<usize, usize> = funcs.iter().enumerate().map(|(i, f)| (f.id, i)).collect();
    // callees first
    let callees: HashMap<usize, Vec<usize>> = funcs
        .iter()
        .map(|f| (f.id, crate::inline::sites(f, &mut pinned).iter().map(|s| s.callee).collect()))
        .collect();
    fn visit(f: usize, callees: &HashMap<usize, Vec<usize>>, seen: &mut HashSet<usize>, order: &mut Vec<usize>) {
        if !seen.insert(f) {
            return;
        }
        for &c in callees.get(&f).into_iter().flatten() {
            visit(c, callees, seen, order);
        }
        order.push(f);
    }
    let (mut seen, mut order) = (HashSet::new(), Vec::new());
    for f in funcs.iter() {
        visit(f.id, &callees, &mut seen, &mut order);
    }
    // an inlined function -> the functions that have its text now
    let mut holders: HashMap<usize, BTreeSet<usize>> = HashMap::new();
    let join = std::env::var_os("RUST64_NO_JOIN").is_none();
    let mut dirty: HashSet<usize> = HashSet::new();
    for c in order {
        let Some(&ci) = index.get(&c) else { continue };
        // A callee that does not read all the registers its arguments
        // come in: the loads in this function can go.
        let spare = callees.get(&c).into_iter().flatten().any(|g| {
            let taken = match p.arg_bytes.get(g) {
                Some(0) => 0,
                Some(1) => 1,
                Some(2) => 3,
                _ => 7,
            };
            p.entry_regs.get(g).is_some_and(|r| r & taken != taken)
        });
        if spare && funcs[ci].live {
            dirty.insert(c);
        }
        if dirty.contains(&c) {
            let mut extra: Vec<usize> =
                holders.iter().filter(|(_, h)| h.len() == 1 && h.contains(&c)).map(|(&d, _)| d).collect();
            extra.sort_unstable();
            let text = funcs[ci].lines.join("\n") + "\n";
            funcs[ci].lines = optimize_fn(p, &text, c, &extra).lines().map(str::to_owned).collect();
            if !extra.is_empty() {
                p.merged.push((c, extra));
            }
        }
        if pinned.contains(&c) {
            continue;
        }
        let Some(body) = crate::inline::body(&funcs[ci]) else { continue };
        let mut calls: Vec<(usize, Vec<crate::inline::Site>)> = Vec::new();
        for (i, f) in funcs.iter().enumerate() {
            if f.live && i != ci {
                let found: Vec<_> =
                    crate::inline::sites(f, &mut pinned).into_iter().filter(|s| s.callee == c).collect();
                if !found.is_empty() {
                    calls.push((i, found));
                }
            }
        }
        let n: i64 = calls.iter().map(|(_, s)| s.len() as i64).sum();
        if pinned.contains(&c) || n == 0 || (n > 1 && n * (body.inline_size - 3) > body.size) {
            continue;
        }
        let mut into = BTreeSet::new();
        for (i, found) in calls {
            let caller = funcs[i].id;
            for site in found.iter().rev() {
                let mut text = crate::inline::paste(&body, caller, site.tail);
                let mut last = site.line;
                // (a result in memory: the text writes it where it goes)
                if !site.tail
                    && join
                    && let Some(&(_, size)) = p.sigs.get(&c)
                    && let Some((place, n)) =
                        crate::inline::result_copy(&funcs[i].lines, site.line, c, caller, size, &|name| {
                            p.free_dests.get(name) == Some(&true)
                        })
                {
                    text = crate::inline::with_result_at(text, c, &place);
                    last += n;
                }
                funcs[i].lines.splice(site.line..=last, text);
            }
            crate::asm::widen(&mut funcs[i].lines);
            dirty.insert(caller);
            into.insert(caller);
        }
        funcs[ci].live = false;
        for h in holders.values_mut() {
            if h.remove(&c) {
                h.extend(into.iter().copied());
            }
        }
        holders.insert(c, into);
    }
}

/// The arguments of `inst` that are not passed: references to values
/// without bytes (`&mut Console`, the `self` of a type that only has
/// methods) which the function does nothing with but hand them on. Such a
/// reference would take the registers that the other arguments can use.
///
/// "Nothing but hand them on": the argument (and copies and reborrows of
/// it) is only dereferenced, which reads and writes nothing, or passed to
/// functions that do not take it either. A function that looks at the
/// address itself (a cast, a comparison, storing or returning it) gets it.
fn unpassed_args<'tcx>(p: &mut Program<'tcx>, inst: Instance<'tcx>) -> Vec<bool> {
    let tcx = p.tcx;
    match p.unpassed.get(&inst) {
        Some(Some(v)) => return v.clone(),
        // (a function that calls itself)
        Some(None) => return Vec::new(),
        None => {}
    }
    let def_id = inst.def_id();
    let plain = matches!(inst.def, InstanceKind::Item(_))
        && !tcx.is_foreign_item(def_id)
        && !tcx.is_closure_like(def_id)
        && tcx.is_mir_available(def_id);
    if !plain {
        p.unpassed.insert(inst, Some(Vec::new()));
        return Vec::new();
    }
    p.unpassed.insert(inst, None);
    let mir = tcx.instance_mir(inst.def);
    let mono = |ty: Ty<'tcx>| {
        inst.instantiate_mir_and_normalize_erasing_regions(
            tcx,
            TypingEnv::fully_monomorphized(),
            ty::EarlyBinder::bind(tcx, ty),
        )
    };
    struct Whole<'a> {
        set: &'a [bool],
        found: bool,
    }
    impl<'tcx> Visitor<'tcx> for Whole<'_> {
        fn visit_place(&mut self, place: &Place<'tcx>, ctx: PlaceContext, loc: Location) {
            if matches!(ctx, PlaceContext::NonUse(_)) {
                return;
            }
            // behind the reference there is nothing
            if self.set[place.local.as_usize()] && place.projection.first() != Some(&ProjectionElem::Deref) {
                self.found = true;
            }
            self.super_projection(place.as_ref(), ctx, loc);
        }
        fn visit_local(&mut self, local: Local, ctx: PlaceContext, _: Location) {
            if !matches!(ctx, PlaceContext::NonUse(_)) && self.set[local.as_usize()] {
                self.found = true;
            }
        }
    }
    let n = mir.local_decls.len();
    let mut out = vec![false; mir.arg_count];
    if mir.spread_arg.is_none() {
        for arg in 1..=mir.arg_count {
            let ty = mono(mir.local_decls[Local::from_usize(arg)].ty);
            let ty::Ref(_, to, _) = *ty.kind() else { continue };
            if !to.is_sized(tcx, TypingEnv::fully_monomorphized()) || p.cx.layout_of(to).size.bytes() != 0 {
                continue;
            }
            // the argument and the locals that are copies or reborrows of it
            let mut set = vec![false; n];
            set[arg] = true;
            let own = |l: Local| l.as_usize() > mir.arg_count;
            let mut ok = true;
            let mut grown = true;
            while grown && ok {
                grown = false;
                for (bb, data) in mir.basic_blocks.iter_enumerated() {
                    if data.is_cleanup {
                        continue;
                    }
                    for (i, stmt) in data.statements.iter().enumerate() {
                        let loc = Location { block: bb, statement_index: i };
                        if let StatementKind::Assign(b) = &stmt.kind {
                            let (dest, rv) = &**b;
                            let from = match rv {
                                Rvalue::Use(Operand::Copy(src) | Operand::Move(src), _)
                                    if src.projection.is_empty() && set[src.local.as_usize()] =>
                                {
                                    true
                                }
                                Rvalue::Ref(_, _, src)
                                    if set[src.local.as_usize()]
                                        && src.projection.first() == Some(&ProjectionElem::Deref) =>
                                {
                                    true
                                }
                                _ => false,
                            };
                            if from {
                                if !dest.projection.is_empty() || !own(dest.local) {
                                    ok = false;
                                } else if !set[dest.local.as_usize()] {
                                    set[dest.local.as_usize()] = true;
                                    grown = true;
                                }
                                continue;
                            }
                            // (a raw pointer is the address)
                            if matches!(rv, Rvalue::RawPtr(_, src) if set[src.local.as_usize()]) {
                                ok = false;
                            }
                        }
                        let mut w = Whole { set: &set, found: false };
                        w.visit_statement(stmt, loc);
                        ok &= !w.found;
                    }
                    let term = data.terminator();
                    let loc = Location { block: bb, statement_index: data.statements.len() };
                    match &term.kind {
                        TerminatorKind::Call { func, args, destination, .. } => {
                            let mut w = Whole { set: &set, found: false };
                            w.visit_operand(func, loc);
                            w.visit_place(destination, PlaceContext::MutatingUse(MutatingUseContext::Call), loc);
                            ok &= !w.found;
                            // the function that is called, if it is one of
                            // the program's
                            let callee = match *mono(func.ty(mir, tcx)).kind() {
                                ty::FnDef(d, fargs) => Some(Instance::expect_resolve(
                                    tcx,
                                    TypingEnv::fully_monomorphized(),
                                    d,
                                    fargs.skip_binder(),
                                    term.source_info.span,
                                )),
                                _ => None,
                            };
                            for (j, a) in args.iter().enumerate() {
                                match &a.node {
                                    Operand::Copy(src) | Operand::Move(src)
                                        if src.projection.is_empty() && set[src.local.as_usize()] =>
                                    {
                                        let taken = match callee {
                                            Some(c) => unpassed_args(p, c).get(j) != Some(&true),
                                            None => true,
                                        };
                                        ok &= !taken;
                                    }
                                    o => {
                                        let mut w = Whole { set: &set, found: false };
                                        w.visit_operand(o, loc);
                                        ok &= !w.found;
                                    }
                                }
                            }
                        }
                        _ => {
                            let mut w = Whole { set: &set, found: false };
                            w.visit_terminator(term, loc);
                            ok &= !w.found;
                        }
                    }
                }
            }
            out[arg - 1] = ok;
        }
    }
    p.unpassed.insert(inst, Some(out.clone()));
    out
}

/// The assembly-level passes over the text of one function. `extra` are
/// functions whose text is in this one and nowhere else (`inline_fns`): the
/// bytes of their frames are followed like those of the function's own.
fn optimize_fn(p: &mut Program<'_>, text: &str, id: usize, extra: &[usize]) -> String {
    let opt = crate::asm::relax(text);
    let Some((mut layout, args_end)) = p.layouts.get(&id).cloned() else { return opt };
    let mut base = layout.values().map(|&(o, n)| o + n).max().unwrap_or(0);
    for d in extra {
        let Some((other, _)) = p.layouts.get(d) else { continue };
        for (name, &(o, n)) in other {
            layout.insert(name.clone(), (base + o, n));
        }
        base += other.values().map(|&(o, n)| o + n).max().unwrap_or(0);
    }
    let mut opt = crate::asm::forward(&opt, id, &layout, args_end, &mut |l, o| const_byte(p, l, o));
    let mut found = None;
    let mut stores = Vec::new();
    if std::env::var_os("RUST64_NO_OPT").is_none() {
        // The data-flow pass follows every byte of the frame at every point
        // of the function. Large locals (arrays) would make that slow and
        // gain little: they are left out, which makes them memory like any
        // other for the pass. The bytes that are left move together.
        let layout = {
            const LARGE: u64 = 64;
            let kept = |name: &str, n: u64| n <= LARGE || name.ends_with('t');
            let mut spans: Vec<(u64, u64)> =
                layout.iter().filter(|(name, v)| kept(name, v.1)).map(|(_, &v)| v).collect();
            spans.sort_unstable();
            spans.dedup();
            // old start -> new start of each run of bytes that are kept
            let mut runs: Vec<(u64, u64, u64)> = Vec::new();
            let mut next = 0;
            for (o, n) in spans {
                match runs.last_mut() {
                    Some((start, end, _)) if o < *end => {
                        let _ = start;
                        if o + n > *end {
                            next += o + n - *end;
                            *end = o + n;
                        }
                    }
                    _ => {
                        runs.push((o, o + n, next));
                        next += n;
                    }
                }
            }
            let moved = |o: u64| {
                let &(start, _, new) = runs.iter().rev().find(|r| r.0 <= o).unwrap();
                new + o - start
            };
            let tracked: crate::asm::FrameLayout = layout
                .iter()
                .filter(|(name, v)| kept(name, v.1))
                .map(|(name, &(o, n))| (name.clone(), (moved(o), n)))
                .collect();
            tracked
        };
        // Statics at a fixed address may be hardware registers.
        let tcx = p.tcx;
        let statics = &p.statics;
        let is_io = |name: &str| {
            name.strip_prefix('S').and_then(|n| n.parse::<usize>().ok()).is_some_and(|n| {
                statics.get_index(n).is_none_or(|&d| {
                    tcx.codegen_fn_attrs(d).link_section.is_some_and(|s| s.as_str().starts_with("at:"))
                })
            })
        };
        // The bytes of the arguments of a function, in assembly or of this
        // program (the first three are in A, X and Y); the runtime's
        // routines take theirs in memory, but `rt_memset` the value in A.
        // Data that the program never changes: constants, and statics that
        // are not `mut`, have no interior mutability and are not exported
        // to assembly.
        let (allocs, exported) = (&p.allocs, &p.exported_statics);
        let is_const = |name: &str| {
            let index = |prefix: char| name.strip_prefix(prefix).and_then(|n| n.parse::<usize>().ok());
            if let Some(n) = index('S') {
                return statics.get_index(n).is_some_and(|&d| {
                    !exported.contains(&n)
                        && !tcx.is_mutable_static(d)
                        && tcx.codegen_fn_attrs(d).link_section.is_none()
                        && tcx
                            .type_of(d)
                            .instantiate_identity()
                            .skip_normalization()
                            .is_freeze(tcx, TypingEnv::fully_monomorphized())
                });
            }
            index('A').and_then(|n| allocs.get_index(n)).is_some_and(|&id| {
                matches!(tcx.global_alloc(id), GlobalAlloc::Memory(m) if !m.inner().mutability.is_mut())
            })
        };
        let (foreign, arg_bytes) = (&p.foreign, &p.arg_bytes);
        let call_args = |sym: &str| match foreign.get(sym) {
            Some(&(args, _)) => Some(args),
            // a function of this program (`None`: not compiled or called
            // yet, so it counts as taking all three registers)
            None if sym.strip_prefix('F').is_some_and(|n| n.parse::<usize>().is_ok()) => {
                arg_bytes.get(&sym[1..].parse::<usize>().unwrap()).copied()
            }
            None if sym == "rt_memset" => Some(1),
            None if sym.starts_with("rt_") => Some(0),
            None => None,
        };
        let entry_regs = &p.entry_regs;
        let call_reads = |sym: &str| {
            sym.strip_prefix('F').and_then(|n| n.parse::<usize>().ok()).and_then(|n| entry_regs.get(&n).copied())
        };
        let consts: Vec<(usize, u8)> = p
            .const_args
            .get(&id)
            .into_iter()
            .flatten()
            .filter_map(|(name, off, v)| {
                layout.get(name).filter(|&&(_, n)| *off < n).map(|&(o, _)| ((o + off) as usize, *v))
            })
            .collect();
        let mut entry = None;
        for _ in 0..4 {
            let next = crate::asm::relax(&crate::opt::optimize(
                &crate::asm::relax(&opt),
                id,
                &layout,
                &is_io,
                &is_const,
                &call_args,
                &call_reads,
                &mut entry,
                &consts,
                &mut stores,
            ));
            if next == opt {
                break;
            }
            // (what the last run found belongs to the text before it)
            entry = None;
            opt = next;
        }
        found = entry;
    }
    match found {
        Some(regs) => p.entry_regs.insert(id, regs),
        None => p.entry_regs.swap_remove(&id),
    };
    p.arg_stores.insert(id, stores);
    // (the optimizer may have put more between a branch and its target
    // than the branch reaches)
    let mut lines: Vec<String> = opt.lines().map(str::to_owned).collect();
    crate::asm::widen(&mut lines);
    let mut opt = lines.join("\n");
    opt.push('\n');
    crate::asm::peephole(&crate::asm::relax(&opt))
}

/// Removes the stores to frame bytes that nothing reads, in the finished
/// code: the last passes take loads away (a pointer in the zero page is
/// used where it is, a value is still in A) after the optimizer of the
/// functions has run. `frame` gives the frame, offset and size of a frame
/// name; `others` is the rest of the program. A plain load or operand
/// reads its byte; a name used in any other way (indexed, as an address,
/// as a pointer) reads all its bytes.
fn drop_unread_stores(code: &str, others: &[&str], frame: &dyn Fn(&str) -> Option<(usize, u64, u64)>) -> String {
    use std::collections::HashSet;
    let words = |line: &'_ str| -> Vec<String> {
        line.split(';')
            .next()
            .unwrap_or("")
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .filter(|w| w.starts_with(['F', 'L']))
            .map(str::to_owned)
            .collect()
    };
    // a plain operand `name+k`: its byte
    let plain = |arg: &str| -> Option<(usize, u64)> {
        let (base, off) = FnCx::split_offset(arg.trim());
        let (id, o, n) = frame(base)?;
        (off < n).then_some((id, o + off))
    };
    let mut read: HashSet<(usize, u64)> = HashSet::new();
    let all = |line: &str, read: &mut HashSet<(usize, u64)>| {
        for w in words(line) {
            if let Some((id, o, n)) = frame(&w) {
                read.extend((o..o + n).map(|b| (id, b)));
            }
        }
    };
    for text in others {
        for line in text.lines() {
            all(line, &mut read);
        }
    }
    let parts = |line: &'_ str| -> Option<(String, String)> {
        let t = line.split(';').next().unwrap_or("").trim();
        if !line.starts_with([' ', '\t']) {
            return None;
        }
        let (op, arg) = t.split_once(' ')?;
        Some((op.to_owned(), arg.trim().to_owned()))
    };
    for line in code.lines() {
        match parts(line) {
            Some((op, arg)) if matches!(op.as_str(), "sta" | "stx" | "sty") && plain(&arg).is_some() => {}
            Some((_, arg)) if plain(&arg).is_some() => {
                read.insert(plain(&arg).unwrap());
            }
            _ => all(line, &mut read),
        }
    }
    let mut out = String::with_capacity(code.len());
    for line in code.lines() {
        if let Some((op, arg)) = parts(line)
            && matches!(op.as_str(), "sta" | "stx" | "sty")
            && plain(&arg).is_some_and(|b| !read.contains(&b))
        {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Finds the bytes of arguments in memory that are the same constant in
/// every call (the upper byte of a length that is always short), and
/// optimizes the functions again that take them: they then have the
/// constant in place of the byte, and `drop_dead_args` takes the stores
/// out of the callers if nothing reads the byte any more.
///
/// A byte counts if all that names its slot outside the function are plain
/// stores (`others`: the rest of the program, which must not name it at
/// all), the function itself does not store to it, and the optimizer saw
/// every one of the stores with the same constant in the register.
fn const_args(p: &mut Program<'_>, others: &[&str]) {
    use std::collections::{HashMap, HashSet};
    // the argument slots a line names: the function and the name
    fn slots(line: &str) -> impl Iterator<Item = (usize, &str)> {
        let line = line.split(';').next().unwrap_or("");
        line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).filter_map(|w| {
            let (id, k) = w.strip_prefix('L')?.split_once('_')?;
            if k.is_empty() || !k.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some((id.parse().ok()?, w))
        })
    }
    let entry = |line: &str| line.strip_prefix('F').and_then(|r| r.strip_suffix(':')).and_then(|r| r.parse::<usize>().ok());
    let mut bad: HashSet<String> = HashSet::new();
    for text in others {
        for line in text.lines() {
            bad.extend(slots(line).map(|(_, w)| w.to_owned()));
        }
    }
    let mut count: HashMap<(String, u64), usize> = HashMap::new();
    let mut cur = usize::MAX;
    for line in p.asm.text.lines() {
        if let Some(id) = entry(line) {
            cur = id;
        }
        let t = line.split(';').next().unwrap_or("").trim();
        for (id, w) in slots(line) {
            let store = t
                .split_once(' ')
                .filter(|(op, _)| matches!(*op, "sta" | "stx" | "sty"))
                .map(|(_, arg)| FnCx::split_offset(arg.trim()))
                .filter(|(base, _)| *base == w);
            match store {
                Some((_, off)) if id != cur => *count.entry((w.to_owned(), off)).or_default() += 1,
                None if id == cur => {}
                _ => {
                    bad.insert(w.to_owned());
                }
            }
        }
    }
    let mut vals: HashMap<(&str, u64), Vec<Option<u8>>> = HashMap::new();
    for list in p.arg_stores.values() {
        for (name, off, v) in list {
            vals.entry((name.as_str(), *off)).or_default().push(*v);
        }
    }
    let mut found: Vec<(usize, String, u64, u8)> = Vec::new();
    for ((name, off), n) in &count {
        let Some(v) = vals.get(&(name.as_str(), *off)) else { continue };
        let Some(&Some(first)) = v.first() else { continue };
        let Some((id, _)) = slots(name).next() else { continue };
        if !bad.contains(name) && v.len() == *n && v.iter().all(|x| *x == Some(first)) {
            found.push((id, name.clone(), *off, first));
        }
    }
    if found.is_empty() {
        return;
    }
    found.sort();
    let mut args: FxIndexMap<usize, Vec<(String, u64, u8)>> = FxIndexMap::default();
    for (id, name, off, v) in found {
        args.entry(id).or_default().push((name, off, v));
    }
    p.const_args = args;
    // the functions that take them, again
    let text = std::mem::take(&mut p.asm.text);
    let mut out = String::with_capacity(text.len());
    let mut chunk: Option<(usize, String)> = None;
    let flush = |p: &mut Program<'_>, out: &mut String, chunk: Option<(usize, String)>| {
        if let Some((id, text)) = chunk {
            if p.const_args.contains_key(&id) {
                out.push_str(&optimize_fn(p, &text, id, &[]));
            } else {
                out.push_str(&text);
            }
        }
    };
    for line in text.lines() {
        if let Some(id) = entry(line) {
            flush(p, &mut out, chunk.take());
            chunk = Some((id, String::new()));
        }
        let to = match &mut chunk {
            Some((_, c)) => c,
            None => &mut out,
        };
        to.push_str(line);
        to.push('\n');
    }
    flush(p, &mut out, chunk.take());
    p.asm.text = out;
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
    /// That code uses the scratch pointers itself.
    zp: bool,
    /// The program calls through function pointers (`rt_jvec`, which a
    /// dispatcher sets too).
    jvec: bool,
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
        // The scratch pointers: only if the code that an interrupt runs
        // uses them, itself or in a routine it calls. `rt_jvec`: only if
        // the program can be interrupted between setting it and the call.
        let all = std::env::var_os("RUST64_CTX_ALL").is_some();
        if all || c.zp || c.arith || c.calls || c.foreign {
            copy(&mut a, "$f9", "rt_ctxbuf+256", "6", false);
        }
        if all || c.jvec {
            copy(&mut a, "rt_jvec", "rt_ctxbuf+262", "2", false);
        }
        // Assembly functions may use the arithmetic registers too.
        if c.arith || c.foreign {
            copy(&mut a, "rt_save", "rt_ctxbuf", "rt_save_n", false);
        }
        if c.calls {
            copy(&mut a, "rt_args", "rt_ctxbuf+39", "48", false);
        }
        if c.foreign {
            copy(&mut a, "rt_fsave", "rt_ctxbuf+87", "rt_fsave_end-rt_fsave", true);
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
        // (a result that is read where the call left it)
        if let Some(Some(at)) = self.res_at.get(l.as_usize()) {
            return at.clone();
        }
        match self.fwd.get(l.as_usize()) {
            // (a temporary that is built where it goes)
            Some(&Some((d, 0))) => format!("F{}v{}", self.id, d.as_usize()),
            Some(&Some((d, off))) => format!("F{}v{}+{off}", self.id, d.as_usize()),
            _ => format!("F{}v{}", self.id, l.as_usize()),
        }
    }

    /// The address of a local; marks its storage as used.
    fn local_addr(&self, l: Local) -> String {
        let at = match self.fwd.get(l.as_usize()) {
            Some(&Some((d, _))) => d,
            _ => l,
        };
        self.touched.borrow_mut()[at.as_usize()] = true;
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
            if i > self.mir.arg_count && !touched[i] || self.fwd[i].is_some() {
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
        // The first three argument bytes arrive in A, X and Y. (Stores of
        // bytes that the function reads from the register, or not at all,
        // go in `opt.rs`.)
        let mut k = 0;
        let mut total = 0;
        let unpassed = unpassed_args(self.p, self.inst);
        self.p.unpassed_of.insert(self.id, unpassed.clone());
        for l in 1..=self.mir.arg_count {
            let local = Local::from_usize(l);
            if unpassed.get(l - 1) == Some(&true) {
                continue;
            }
            let size = self.layout(self.local_ty(local)).size.bytes();
            total += size;
            for o in 0..size {
                if k < 3 {
                    let at = self.local_addr(local);
                    self.a().ins(format!("{} {at}+{o}", ["sta", "stx", "sty"][k]));
                    k += 1;
                }
            }
        }
        self.p.arg_bytes.insert(self.id, total);
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
            let niche = self.niche_switch(bb);
            for (i, stmt) in data.statements.iter().enumerate() {
                if fused.is_some_and(|(k, also, ..)| k == i || also == Some(i)) {
                    continue;
                }
                if niche.is_some_and(|(k, ..)| k == i) {
                    continue;
                }
                if self.done.contains(&(bb, i)) {
                    continue;
                }
                self.span = stmt.source_info.span;
                self.temp_cur = 0;
                self.cur = Some((bb, i));
                self.statement(&stmt.kind);
            }
            self.cur = None;
            self.span = term.source_info.span;
            self.temp_cur = 0;
            if let Some((i, _, op, x, y, Target::Block(t), Target::Panic)) = fused
                && self.implied(bb, i, op, x, y)
            {
                // (a check that the branch into this block has made)
                self.goto(t, next);
            } else if let Some((_, _, op, x, y, t_true, t_false)) = fused {
                self.branch_on(op, x, y, t_true, t_false, next);
            } else if let Some((_, place, start, t_niche, t_other)) = niche {
                // the tag is the niche value, or the variant with the data
                let (a, l, _) = self.place(place);
                let Variants::Multiple { tag_field, .. } = &l.variants else { unreachable!() };
                let v = Val::Mem(a.offset(l.fields.offset(tag_field.as_usize()).bytes()));
                let a = &mut self.p.asm;
                a.setup_val(&v, 0);
                a.lda(&v, 0, 0);
                if start != 0 {
                    a.ins(format!("cmp #${start:02x}"));
                }
                self.two_way("beq", "bne", Target::Block(t_niche), Target::Block(t_other), next);
            } else {
                self.terminator(&term.kind, next);
            }
        }
    }

    /// The order in which the blocks are emitted: reverse postorder, where
    /// the successor that control usually continues with (the target of a
    /// goto, call or assert; of a switch the branch that stays in the
    /// innermost loop, else the "otherwise" branch) is visited last and so
    /// follows its block directly. That keeps the body of a
    /// loop together with the exit after it, and puts both sides of an `if`
    /// and the cases of a `match` before the code they join in, so most
    /// jumps disappear and most branches are short. Unreachable and cleanup
    /// blocks (panic = abort: no unwinding) are left out.
    fn block_order(&self) -> Vec<BasicBlock> {
        let blocks = &self.mir.basic_blocks;
        let reachable = self.reachable_blocks();
        let panics = self.panic_blocks();
        let ok = |b: BasicBlock| reachable[b.as_usize()] && !blocks[b].is_cleanup;
        // In how many loops each block is: a loop is what can reach a
        // jump back to a block that dominates it, without passing that
        // block.
        let mut depth = vec![0u32; blocks.len()];
        {
            let dom = blocks.dominators();
            let preds = blocks.predecessors();
            let mut loops: FxIndexMap<BasicBlock, Vec<bool>> = FxIndexMap::default();
            for (u, data) in blocks.iter_enumerated() {
                if !ok(u) || !dom.is_reachable(u) {
                    continue;
                }
                for h in data.terminator().successors() {
                    if !ok(h) || !dom.dominates(h, u) {
                        continue;
                    }
                    let inside = loops.entry(h).or_insert_with(|| vec![false; blocks.len()]);
                    inside[h.as_usize()] = true;
                    let mut work = vec![u];
                    while let Some(x) = work.pop() {
                        if !std::mem::replace(&mut inside[x.as_usize()], true) {
                            work.extend(preds[x].iter().copied().filter(|&q| ok(q)));
                        }
                    }
                }
            }
            for inside in loops.values() {
                for (d, i) in depth.iter_mut().zip(inside) {
                    *d += *i as u32;
                }
            }
        }
        // The successors of a block, the preferred one last.
        let succs = |bb: BasicBlock| -> Vec<BasicBlock> {
            let mut v: Vec<BasicBlock> = match &blocks[bb].terminator().kind {
                TerminatorKind::Goto { target, .. }
                | TerminatorKind::Assert { target, .. }
                | TerminatorKind::Drop { target, .. }
                | TerminatorKind::FalseEdge { real_target: target, .. }
                | TerminatorKind::FalseUnwind { real_target: target, .. } => vec![*target],
                TerminatorKind::Call { target, .. } => target.iter().copied().collect(),
                TerminatorKind::InlineAsm { targets, .. } => targets.iter().rev().copied().collect(),
                TerminatorKind::SwitchInt { discr, targets } => match self.const_bits(discr) {
                    Some(c) => vec![targets.target_for_value(c)],
                    None => {
                        let mut v: Vec<BasicBlock> = targets.iter().map(|(_, t)| t).collect();
                        v.reverse();
                        v.push(targets.otherwise());
                        v
                    }
                },
                _ => Vec::new(),
            };
            v.retain(|&t| ok(t));
            // panics away from the code that runs; the body of a loop
            // before its exit
            v.sort_by_key(|&t| (!panics[t.as_usize()], depth[t.as_usize()]));
            v
        };
        let mut seen = vec![false; blocks.len()];
        let mut post = Vec::with_capacity(blocks.len());
        let entry = BasicBlock::from_usize(0);
        let mut stack = vec![(entry, succs(entry), 0usize)];
        seen[0] = true;
        while let Some((bb, list, i)) = stack.last_mut() {
            if let Some(&t) = list.get(*i) {
                *i += 1;
                if !std::mem::replace(&mut seen[t.as_usize()], true) {
                    stack.push((t, succs(t), 0));
                }
            } else {
                post.push(*bb);
                stack.pop();
            }
        }
        post.reverse();
        post
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
                if size == 0 {
                    continue;
                }
                // A large value is only a view of one place: it is where
                // that is (a view of parts would be copied byte by byte).
                if size > 16 && !matches!(rv, Rvalue::Use(Operand::Copy(_) | Operand::Move(_), _)) {
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
                // A local that is borrowed can change through a pointer:
                // a view of it holds only up to uses in the same block,
                // with nothing in between that stores through a pointer
                // (a call ends the block).
                let near = uses.iter().all(|l| l.block == bb && l.statement_index > i)
                    && uses.iter().map(|l| l.statement_index).max().is_some_and(|last| {
                        data.statements.iter().take(last + 1).skip(i + 1).all(|s| match &s.kind {
                            StatementKind::Assign(b) => !b.0.is_indirect(),
                            StatementKind::SetDiscriminant { place, .. } => !place.is_indirect(),
                            StatementKind::StorageLive(_) | StatementKind::StorageDead(_) | StatementKind::Nop => true,
                            _ => false,
                        })
                    });
                for r in &my_roots {
                    let r = r.as_usize();
                    if u.borrowed[r] && !near
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
        self.lazy_roots = roots;
        cand
    }

    /// Results that are read where the called function left them: a local
    /// of more than three bytes (a result in memory) whose only assignment
    /// is a call, and that is only read before the next call. The result
    /// area of a function keeps its contents until something is called
    /// (frames of functions that are not active at the same time share
    /// memory), so the copy into the local is not needed.
    ///
    /// "Read" includes the views of the local (`lazy_locals`). The
    /// arguments of the next call do not count as "before" it: they are
    /// written into another frame.
    fn result_views(&self) -> Vec<bool> {
        struct Reads(Vec<(Local, Location)>);
        impl<'tcx> Visitor<'tcx> for Reads {
            fn visit_local(&mut self, local: Local, ctx: PlaceContext, loc: Location) {
                if !matches!(ctx, PlaceContext::NonUse(_)) {
                    self.0.push((local, loc));
                }
            }
        }
        let mir = self.mir;
        let n = mir.local_decls.len();
        let u = &self.uses;
        let mut cand = vec![false; n];
        let mut reads = Reads(Vec::new());
        reads.visit_body(mir);
        let clobbers = |bb: BasicBlock| {
            matches!(
                mir.basic_blocks[bb].terminator().kind,
                TerminatorKind::Call { .. } | TerminatorKind::Drop { .. } | TerminatorKind::TailCall { .. }
            )
        };
        let preds = mir.basic_blocks.predecessors();
        for (bb, data) in mir.basic_blocks.iter_enumerated() {
            let TerminatorKind::Call { destination, target: Some(target), .. } = &data.terminator().kind else {
                continue;
            };
            let t = destination.local.as_usize();
            if !destination.projection.is_empty()
                || t == 0
                || t <= mir.arg_count
                || u.defs[t] != 1
                || u.borrowed[t]
                || self.lazy_cand[t]
                || self.addrs[t].is_some()
                || self.fwd[t].is_some()
                || self.joined[t]
                || self.layout(self.local_ty(destination.local)).size.bytes() <= 3
            {
                continue;
            }
            // Where the result is still there at the start of each block:
            // true until shown otherwise, false at the entry.
            let mut valid = vec![true; mir.basic_blocks.len()];
            valid[0] = false;
            let out = |valid: &[bool], p: BasicBlock, to: BasicBlock| {
                if p == bb {
                    to == *target
                } else {
                    valid[p.as_usize()] && !clobbers(p)
                }
            };
            loop {
                let mut changed = false;
                for b in mir.basic_blocks.indices() {
                    let i = b.as_usize();
                    if valid[i] && (i == 0 || !preds[b].iter().all(|&p| out(&valid, p, b))) {
                        valid[i] = false;
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
            }
            let ok = reads.0.iter().all(|&(l, loc)| {
                let mine = l == destination.local
                    || self.lazy_roots.get(l.as_usize()).is_some_and(|r| r.contains(&destination.local));
                if !mine {
                    return true;
                }
                let stmts = mir.basic_blocks[loc.block].statements.len();
                // (the call's own destination is its write)
                if loc.block == bb && loc.statement_index == stmts {
                    return l == destination.local && !self.call_reads(data.terminator(), destination.local);
                }
                valid[loc.block.as_usize()] && (loc.statement_index < stmts || !clobbers(loc.block))
            });
            cand[t] = ok;
        }
        cand
    }

    /// Whether the call reads `l` (as an argument or as the function).
    fn call_reads(&self, term: &Terminator<'tcx>, l: Local) -> bool {
        let TerminatorKind::Call { func, args, .. } = &term.kind else { return true };
        let reads = |o: &Operand<'tcx>| matches!(o, Operand::Copy(p) | Operand::Move(p) if p.local == l);
        reads(func) || args.iter().any(|a| reads(&a.node))
    }

    /// Temporaries that are built where they go: a local of four bytes or
    /// more that is assigned once and used once, further on in the same
    /// block, as the whole value or as a field of an assignment to (a
    /// field of) another local. It gets no storage of its own: its bytes
    /// are that part of the other local, and the assignment has nothing to
    /// copy for it.
    ///
    /// From the definition to the use nothing may name the other local
    /// (its bytes are in use for the temporary), neither directly nor as a
    /// view; if a reference to it exists, nothing may go through a pointer
    /// there either.
    fn forwarded(&self) -> Vec<Option<(Local, u64)>> {
        struct Scan {
            named: Vec<Local>,
            indirect: bool,
        }
        impl<'tcx> Visitor<'tcx> for Scan {
            fn visit_place(&mut self, place: &Place<'tcx>, ctx: PlaceContext, loc: Location) {
                self.indirect |= place.is_indirect();
                self.super_place(place, ctx, loc);
            }
            fn visit_local(&mut self, local: Local, _: PlaceContext, _: Location) {
                self.named.push(local);
            }
        }
        let n = self.mir.local_decls.len();
        let mut fwd: Vec<Option<(Local, u64)>> = vec![None; n];
        let mut targets = vec![false; n];
        let u = &self.uses;
        for (bb, data) in self.mir.basic_blocks.iter_enumerated() {
            if data.is_cleanup {
                continue;
            }
            for (i, stmt) in data.statements.iter().enumerate() {
                let StatementKind::Assign(b) = &stmt.kind else { continue };
                let (place, _) = &**b;
                let t = place.local.as_usize();
                if !place.projection.is_empty()
                    || t == 0
                    || t <= self.mir.arg_count
                    || u.defs[t] != 1
                    || u.uses[t] != 1
                    || u.plain[t] != 1
                    || u.borrowed[t]
                    || self.lazy_cand[t]
                    || self.addrs[t].is_some()
                    || self.consts[t].is_some()
                    || targets[t]
                    || self.layout(self.local_ty(place.local)).size.bytes() < 4
                {
                    continue;
                }
                let Some(at) = u.plain_loc[t] else { continue };
                if at.block != bb || at.statement_index <= i || at.statement_index >= data.statements.len() {
                    continue;
                }
                let j = at.statement_index;
                let StatementKind::Assign(ub) = &data.statements[j].kind else { continue };
                let (dest, rv) = &**ub;
                let d = dest.local.as_usize();
                let Some((base, dl)) = self.field_offset(*dest) else { continue };
                if d == t || self.lazy_cand[d] || self.addrs[d].is_some() || fwd[d].is_some() {
                    continue;
                }
                let is_t = |o: &Operand<'tcx>| {
                    matches!(o, Operand::Copy(p) | Operand::Move(p) if p.projection.is_empty() && p.local == place.local)
                };
                let off = match rv {
                    Rvalue::Use(op, _) if is_t(op) => 0,
                    Rvalue::Aggregate(kind, ops) => {
                        let Some((fi, _)) = ops.iter_enumerated().find(|(_, o)| is_t(o)) else { continue };
                        match &**kind {
                            AggregateKind::Adt(_, variant, _, _, None) => {
                                dl.for_variant(&self.p.cx, *variant).fields.offset(fi.as_usize()).bytes()
                            }
                            AggregateKind::Tuple | AggregateKind::Closure(..) => dl.fields.offset(fi.as_usize()).bytes(),
                            _ => continue,
                        }
                    }
                    _ => continue,
                };
                let mut scan = Scan { named: Vec::new(), indirect: false };
                for (k, s) in data.statements.iter().enumerate().take(j).skip(i) {
                    scan.visit_statement(s, Location { block: bb, statement_index: k });
                }
                scan.visit_rvalue(rv, at);
                // (the destination itself may be behind nothing but fields)
                let clash = scan.named.iter().any(|m| {
                    *m == dest.local || self.lazy_roots.get(m.as_usize()).is_some_and(|r| r.contains(&dest.local))
                });
                if clash || u.borrowed[d] && scan.indirect {
                    continue;
                }
                fwd[t] = Some((dest.local, base + off));
                targets[d] = true;
            }
        }
        fwd
    }

    /// Locals that share their bytes: where a value of four bytes or more
    /// is moved from one local to another (`d = move r.0`, `_0 = Some(move
    /// e)`, `x = move y`), the smaller one lives in the larger one if the
    /// two are never in use at the same time with different contents. The
    /// move then has nothing to copy. The result is in `fwd`, like the
    /// temporaries of `forwarded`.
    ///
    /// "In use at the same time": one of them is written (also through a
    /// pointer, if a reference to it exists) where the other one is read
    /// by the same statement, or holds a value that is read later. A local
    /// that a reference exists to counts as read until its storage ends or
    /// it is moved away as a whole. Locals that already share bytes are
    /// checked each against each.
    fn join_locals(&mut self) {
        if std::env::var_os("RUST64_NO_JOIN").is_some() {
            return;
        }
        let mir = self.mir;
        let blocks = &mir.basic_blocks;
        let n = mir.local_decls.len();
        let sizes: Vec<u64> = (0..n).map(|l| self.layout(self.local_ty(Local::from_usize(l))).size.bytes()).collect();
        let eligible = |l: Local| {
            let i = l.as_usize();
            sizes[i] >= 4 && !self.lazy_cand[i] && self.addrs[i].is_none() && self.consts[i].is_none()
        };
        let pinned = |l: Local| l.as_usize() <= mir.arg_count;
        // A local that is a view of a part of another one (`lazy_locals`):
        // that one, and the offset there.
        let view = |mut l: Local, mut off: u64| -> (Local, u64) {
            for _ in 0..4 {
                if !self.lazy_cand[l.as_usize()] {
                    break;
                }
                let Some(Rvalue::Use(Operand::Copy(src) | Operand::Move(src), ..)) = self.lazy_def[l.as_usize()] else {
                    break;
                };
                let Some((o, _)) = self.field_offset(*src) else { break };
                l = src.local;
                off += o;
            }
            (l, off)
        };
        // the moves: the local that would live in the other one, the other
        // one, the offset there, and the statement
        let mut moves: Vec<(Local, Local, u64, Location)> = Vec::new();
        for (bb, data) in blocks.iter_enumerated() {
            if data.is_cleanup {
                continue;
            }
            for (i, stmt) in data.statements.iter().enumerate() {
                let StatementKind::Assign(b) = &stmt.kind else { continue };
                let (dest, rv) = &**b;
                let loc = Location { block: bb, statement_index: i };
                let bare = |o: &Operand<'tcx>| match o {
                    Operand::Copy(p) | Operand::Move(p) if p.projection.is_empty() => Some(p.local),
                    _ => None,
                };
                // `len` bytes at `so` of `sl` go to `d0` of `dl`: one of
                // the two is all of its local
                let mut relate = |sl: Local, so: u64, dl: Local, d0: u64, len: u64| {
                    if so == 0 && len == sizes[sl.as_usize()] {
                        moves.push((sl, dl, d0, loc));
                    } else if d0 == 0 && len == sizes[dl.as_usize()] {
                        moves.push((dl, sl, so, loc));
                    }
                };
                match rv {
                    Rvalue::Use(Operand::Copy(src) | Operand::Move(src), ..) => {
                        if let (Some((d0, l)), Some((so, _))) = (self.field_offset(*dest), self.field_offset(*src)) {
                            let (sl, so) = view(src.local, so);
                            relate(sl, so, dest.local, d0, l.size.bytes());
                        }
                    }
                    Rvalue::Aggregate(kind, ops) => {
                        let Some((base, dl)) = self.field_offset(*dest) else { continue };
                        for (fi, o) in ops.iter_enumerated() {
                            let Some(src) = bare(o) else { continue };
                            let off = match &**kind {
                                AggregateKind::Adt(_, variant, _, _, None) => {
                                    dl.for_variant(&self.p.cx, *variant).fields.offset(fi.as_usize()).bytes()
                                }
                                AggregateKind::Tuple | AggregateKind::Closure(..) => {
                                    dl.fields.offset(fi.as_usize()).bytes()
                                }
                                _ => continue,
                            };
                            let (sl, so) = view(src, 0);
                            relate(sl, so, dest.local, base + off, sizes[src.as_usize()]);
                        }
                    }
                    _ => {}
                }
            }
        }
        if moves.is_empty() {
            return;
        }
        let mut flows: Vec<Option<Flow>> = (0..n).map(|_| None).collect();
        let trace = std::env::var_os("RUST64_JOIN_TRACE").is_some();
        let mut fwd = self.fwd.clone();
        let mut any = false;
        for (sub, sup, off, at) in moves {
            if trace {
                eprintln!(
                    "join F{}: {sub:?} in {sup:?}+{off} at {at:?} (eligible {} {})",
                    self.id,
                    eligible(sub),
                    eligible(sup)
                );
            }
            if sub == sup || !eligible(sub) || !eligible(sup) || off + sizes[sub.as_usize()] > sizes[sup.as_usize()] {
                continue;
            }
            let place = |fwd: &[Option<(Local, u64)>], l: Local| fwd[l.as_usize()].unwrap_or((l, 0));
            let (rs, os) = place(&fwd, sub);
            let (rp, op) = place(&fwd, sup);
            if rs == rp {
                continue;
            }
            // where the group of `sub` starts in the group of `sup`
            let delta = (op + off) as i64 - os as i64;
            let (root, moving, shift) = if delta >= 0
                && delta as u64 + sizes[rs.as_usize()] <= sizes[rp.as_usize()]
                && !pinned(rs)
            {
                (rp, rs, delta as u64)
            } else if delta <= 0 && (-delta) as u64 + sizes[rp.as_usize()] <= sizes[rs.as_usize()] && !pinned(rp) {
                (rs, rp, (-delta) as u64)
            } else {
                continue;
            };
            let members = |fwd: &[Option<(Local, u64)>], r: Local| -> Vec<(Local, u64)> {
                let mut m = vec![(r, 0)];
                m.extend(fwd.iter().enumerate().filter_map(|(i, f)| match f {
                    Some((d, o)) if *d == r => Some((Local::from_usize(i), *o)),
                    _ => None,
                }));
                m
            };
            let stay = members(&fwd, root);
            let go = members(&fwd, moving);
            let mut ok = true;
            'pairs: for &(a, ao) in &go {
                for &(b, bo) in &stay {
                    let (a0, a1) = (ao + shift, ao + shift + sizes[a.as_usize()]);
                    let (b0, b1) = (bo, bo + sizes[b.as_usize()]);
                    if a0 >= b1 || b0 >= a1 {
                        continue;
                    }
                    for l in [a, b] {
                        if flows[l.as_usize()].is_none() {
                            flows[l.as_usize()] = Some(self.flow(l));
                        }
                    }
                    let (fa, fb) = (flows[a.as_usize()].as_ref().unwrap(), flows[b.as_usize()].as_ref().unwrap());
                    let exempt = ((a == sub && b == sup) || (a == sup && b == sub)).then_some(at);
                    if fa.clashes(fb, exempt) {
                        if trace {
                            for (bi, (ra, rb)) in fa.points.iter().zip(&fb.points).enumerate() {
                                for (i, (x, y)) in ra.iter().zip(rb).enumerate() {
                                    if x.def && (y.used || y.held) || y.def && (x.used || x.held) {
                                        eprintln!(
                                            "  {a:?}/{b:?} clash at bb{bi}[{i}]: def {} {} used {} {} held {} {}",
                                            x.def, y.def, x.used, y.used, x.held, y.held
                                        );
                                    }
                                }
                            }
                        }
                        ok = false;
                        break 'pairs;
                    }
                }
            }
            if !ok {
                continue;
            }
            for (l, o) in go {
                fwd[l.as_usize()] = Some((root, o + shift));
            }
            self.joined[root.as_usize()] = true;
            self.joined[moving.as_usize()] = false;
            any = true;
        }
        if any {
            for l in 0..n {
                if self.joined[l] {
                    self.touched.borrow_mut()[l] = true;
                }
            }
            // (addresses that were noted before: by the new names)
            let id = self.id;
            for a in self.addrs.iter_mut().flatten() {
                let (base, rest) = match a.split_once('+') {
                    Some((b, r)) => (b, Some(r)),
                    None => (a.as_str(), None),
                };
                let Some(l) = base.strip_prefix(&format!("F{id}v")).and_then(|r| r.parse::<usize>().ok()) else {
                    continue;
                };
                let Some((root, off)) = fwd.get(l).copied().flatten() else { continue };
                let extra = match rest {
                    None => Some(0),
                    Some(r) => r.split('+').map(|t| t.parse::<u64>().ok()).sum::<Option<u64>>(),
                };
                *a = match extra {
                    Some(e) if off + e == 0 => format!("F{id}v{}", root.as_usize()),
                    Some(e) => format!("F{id}v{}+{}", root.as_usize(), off + e),
                    None => format!("F{id}v{}+{off}+{}", root.as_usize(), rest.unwrap_or("0")),
                };
            }
            self.fwd = fwd;
        }
    }

    /// Notes for each local of four bytes or more that a call writes its
    /// result to whether no reference to it (or to a local that shares
    /// its bytes) can exist at the call. If the function that is called is
    /// later pasted into this one, it can then write its result there
    /// itself (`inline::result_copy`).
    fn free_dests(&mut self) {
        let mir = self.mir;
        let n = mir.local_decls.len();
        let mut flows: Vec<Option<Flow>> = (0..n).map(|_| None).collect();
        for (bb, data) in mir.basic_blocks.iter_enumerated() {
            let TerminatorKind::Call { destination, .. } = &data.terminator().kind else { continue };
            let d = destination.local;
            if !destination.projection.is_empty() || self.layout(self.local_ty(d)).size.bytes() < 4 {
                continue;
            }
            let root = self.fwd[d.as_usize()].map_or(d, |f| f.0);
            let mut free = true;
            for l in 0..n {
                if l != root.as_usize() && self.fwd[l].map(|f| f.0) != Some(root) {
                    continue;
                }
                if flows[l].is_none() {
                    flows[l] = Some(self.flow(Local::from_usize(l)));
                }
                free &= !flows[l].as_ref().unwrap().points[bb.as_usize()][data.statements.len()].ptr;
            }
            let name = format!("F{}v{}", self.id, root.as_usize());
            let all = self.p.free_dests.entry(name).or_insert(true);
            *all &= free;
        }
    }

    /// Where a local is written and read, and where it holds a value that
    /// is read later (see `join_locals`).
    fn flow(&self, l: Local) -> Flow {
        struct Touch<'a> {
            l: Local,
            lazy: &'a [bool],
            roots: &'a [Vec<Local>],
            used: bool,
            def: bool,
            full: bool,
            borrow: bool,
            raw: bool,
            moved: bool,
            dead: bool,
            /// Something is read or written through a pointer.
            deref: bool,
        }
        impl<'tcx> Visitor<'tcx> for Touch<'_> {
            fn visit_place(&mut self, place: &Place<'tcx>, ctx: PlaceContext, loc: Location) {
                self.super_projection(place.as_ref(), ctx, loc);
                if matches!(ctx, PlaceContext::NonUse(_)) {
                    return;
                }
                self.deref |= place.is_indirect();
                let base = place.local;
                if base != self.l {
                    // (a view of the local is read where it is used)
                    if self.lazy.get(base.as_usize()) == Some(&true)
                        && self.roots.get(base.as_usize()).is_some_and(|r| r.contains(&self.l))
                    {
                        self.used = true;
                    }
                    return;
                }
                if place.projection.first() == Some(&ProjectionElem::Deref) {
                    self.used = true;
                    return;
                }
                match ctx {
                    PlaceContext::MutatingUse(MutatingUseContext::Borrow)
                    | PlaceContext::NonMutatingUse(
                        NonMutatingUseContext::SharedBorrow | NonMutatingUseContext::FakeBorrow,
                    ) => {
                        self.borrow = true;
                        self.used = true;
                    }
                    PlaceContext::MutatingUse(MutatingUseContext::RawBorrow)
                    | PlaceContext::NonMutatingUse(NonMutatingUseContext::RawBorrow) => {
                        self.borrow = true;
                        self.raw = true;
                        self.used = true;
                    }
                    PlaceContext::MutatingUse(
                        MutatingUseContext::Store
                        | MutatingUseContext::Call
                        | MutatingUseContext::AsmOutput
                        | MutatingUseContext::Yield,
                    ) => {
                        self.def = true;
                        self.full |= place.projection.is_empty();
                    }
                    PlaceContext::MutatingUse(_) => {
                        self.used = true;
                        self.def = true;
                    }
                    PlaceContext::NonMutatingUse(NonMutatingUseContext::Move) if place.projection.is_empty() => {
                        self.used = true;
                        self.moved = true;
                    }
                    _ => self.used = true,
                }
            }
            fn visit_local(&mut self, local: Local, ctx: PlaceContext, loc: Location) {
                match ctx {
                    PlaceContext::NonUse(rustc_middle::mir::visit::NonUseContext::StorageDead) => self.dead |= local == self.l,
                    PlaceContext::NonUse(_) => {}
                    _ => self.visit_place(&Place::from(local), ctx, loc),
                }
            }
        }
        let mir = self.mir;
        let blocks = &mir.basic_blocks;
        let preds = blocks.predecessors();
        let nb = blocks.len();
        // per block and statement (the terminator last): the facts
        #[derive(Clone, Copy, Default)]
        struct At {
            used: bool,
            def: bool,
            full: bool,
            borrow: bool,
            ends: bool,
            /// Something may be written, or read, through a pointer.
            indirect: bool,
            deref: bool,
        }
        let mut at: Vec<Vec<At>> = Vec::with_capacity(nb);
        let mut raw = false;
        let mut moved: Vec<Vec<bool>> = Vec::with_capacity(nb);
        for (bb, data) in blocks.iter_enumerated() {
            let mut row = Vec::with_capacity(data.statements.len() + 1);
            let mut mrow = Vec::with_capacity(data.statements.len() + 1);
            for i in 0..=data.statements.len() {
                let mut t = Touch {
                    l,
                    lazy: &self.lazy_cand,
                    roots: &self.lazy_roots,
                    used: false,
                    def: false,
                    full: false,
                    borrow: false,
                    raw: false,
                    moved: false,
                    dead: false,
                    deref: false,
                };
                let loc = Location { block: bb, statement_index: i };
                let indirect = match data.statements.get(i) {
                    Some(stmt) => {
                        t.visit_statement(stmt, loc);
                        match &stmt.kind {
                            StatementKind::Assign(b) => b.0.is_indirect(),
                            StatementKind::StorageLive(_) | StatementKind::StorageDead(_) | StatementKind::Nop => false,
                            _ => true,
                        }
                    }
                    None => {
                        let term = data.terminator();
                        t.visit_terminator(term, loc);
                        !matches!(
                            term.kind,
                            TerminatorKind::Goto { .. }
                                | TerminatorKind::SwitchInt { .. }
                                | TerminatorKind::Return
                                | TerminatorKind::Unreachable
                                | TerminatorKind::Assert { .. }
                        )
                    }
                };
                raw |= t.raw;
                mrow.push(t.moved);
                row.push(At {
                    used: t.used,
                    def: t.def,
                    full: t.full,
                    borrow: t.borrow,
                    ends: t.dead,
                    indirect,
                    deref: t.deref || indirect,
                });
            }
            at.push(row);
            moved.push(mrow);
        }
        // (a pointer made with `&raw` may be used after the value is moved)
        if !raw {
            for (row, mrow) in at.iter_mut().zip(&moved) {
                for (a, m) in row.iter_mut().zip(mrow) {
                    a.ends |= *m;
                }
            }
        }
        // where a reference may exist: forward from the borrows
        let mut bor_in = vec![false; nb];
        let bor_through = |b: usize, mut v: bool, each: &mut dyn FnMut(usize, bool)| -> bool {
            for (i, a) in at[b].iter().enumerate() {
                each(i, v);
                v = (v || a.borrow) && !a.ends;
            }
            v
        };
        loop {
            let mut changed = false;
            for b in blocks.indices() {
                let i = b.as_usize();
                let v = bor_in[i] || preds[b].iter().any(|p| bor_through(p.as_usize(), bor_in[p.as_usize()], &mut |_, _| {}));
                if v != bor_in[i] {
                    bor_in[i] = v;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut bor: Vec<Vec<(bool, bool)>> = Vec::with_capacity(nb);
        for b in 0..nb {
            let mut ins = vec![false; at[b].len()];
            bor_through(b, bor_in[b], &mut |i, v| ins[i] = v);
            let row = ins.iter().zip(&at[b]).map(|(&v, a)| (v, (v || a.borrow) && !a.ends)).collect();
            bor.push(row);
        }
        // through pointers
        for b in 0..nb {
            for (a, &(before, _)) in at[b].iter_mut().zip(&bor[b]) {
                if before {
                    a.used |= a.deref;
                    a.def |= a.indirect;
                }
            }
        }
        // a value was written: forward
        let mut init_in = vec![false; nb];
        init_in[0] = (1..=mir.arg_count).contains(&l.as_usize());
        let init_end = |b: usize, v: bool| v || at[b].iter().any(|a| a.def || a.borrow);
        loop {
            let mut changed = false;
            for b in blocks.indices() {
                let i = b.as_usize();
                let v = init_in[i] || preds[b].iter().any(|p| init_end(p.as_usize(), init_in[p.as_usize()]));
                if v != init_in[i] {
                    init_in[i] = v;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        // read later: backward
        let mut live_in = vec![false; nb];
        let live_start = |b: usize, mut v: bool| {
            for a in at[b].iter().rev() {
                v = a.used || (v && !a.full);
            }
            v
        };
        loop {
            let mut changed = false;
            for b in blocks.indices().rev() {
                let i = b.as_usize();
                let out = blocks[b].terminator().successors().any(|s| live_in[s.as_usize()]);
                let v = live_start(i, out);
                if v != live_in[i] {
                    live_in[i] = v;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut points = Vec::with_capacity(nb);
        for (b, data) in blocks.iter_enumerated() {
            let i = b.as_usize();
            let mut live = data.terminator().successors().any(|s| live_in[s.as_usize()]);
            let mut row = vec![Point::default(); at[i].len()];
            for (k, a) in at[i].iter().enumerate().rev() {
                row[k].live = live;
                live = a.used || (live && !a.full);
            }
            let mut init = init_in[i];
            for (k, a) in at[i].iter().enumerate() {
                init |= a.def || a.borrow;
                row[k].held = row[k].live && init;
                row[k].used = a.used;
                row[k].def = a.def;
                row[k].ptr = bor[i][k].0;
            }
            points.push(row);
        }
        Flow { points }
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
                // (a large view: the place itself)
                if dsize > 16 {
                    return Some(v);
                }
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
    ) -> Option<(usize, Option<usize>, BinOp, &'tcx Operand<'tcx>, &'tcx Operand<'tcx>, Target, Target)> {
        let data = &self.mir.basic_blocks[bb];
        let quiet = |s: &rustc_middle::mir::Statement<'tcx>| {
            matches!(s.kind, StatementKind::StorageDead(_) | StatementKind::StorageLive(_) | StatementKind::Nop)
        };
        let (last, stmt) = data.statements.iter().enumerate().rev().find(|(_, s)| !quiet(s))?;
        let StatementKind::Assign(b) = &stmt.kind else { return None };
        let u = &self.uses;
        let term_loc = Location { block: bb, statement_index: data.statements.len() };
        // A temporary that is assigned once and read once, at `at`.
        let once = |place: &Place<'tcx>, at: Location| {
            let t = place.local.as_usize();
            place.projection.is_empty()
                && u.defs[t] == 1
                && u.uses[t] == 1
                && u.plain[t] == 1
                && !u.borrowed[t]
                && u.plain_loc[t] == Some(at)
                && self.consts[t].is_none()
        };
        let reads = |o: &Operand<'tcx>, l: Local| {
            matches!(o, Operand::Copy(p) | Operand::Move(p) if p.projection.is_empty() && p.local == l)
        };
        // `!(a < b)`: the comparison in front of the negation, with the
        // targets the other way round.
        let (i, also, stmt, cond, negated) = match &b.1 {
            Rvalue::UnaryOp(UnOp::Not, inner)
                if once(&b.0, term_loc) && self.local_ty(b.0.local).is_bool() =>
            {
                let (k, cmp) = data.statements.iter().enumerate().take(last).rev().find(|(_, s)| !quiet(s))?;
                let StatementKind::Assign(cb) = &cmp.kind else { return None };
                if !reads(inner, cb.0.local) || !once(&cb.0, Location { block: bb, statement_index: last }) {
                    return None;
                }
                (k, Some(last), cmp, b.0.local, true)
            }
            _ if once(&b.0, term_loc) => (last, None, stmt, b.0.local, false),
            _ => return None,
        };
        let StatementKind::Assign(b) = &stmt.kind else { return None };
        let (_, rv) = &**b;
        let Rvalue::BinaryOp(op, ops) = rv else { return None };
        if !matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) {
            return None;
        }
        let is_t = |o: &Operand<'tcx>| reads(o, cond);
        let (t_true, t_false) = match &data.terminator().kind {
            TerminatorKind::SwitchInt { discr, targets } if is_t(discr) => (
                Target::Block(targets.target_for_value(1)),
                Target::Block(targets.target_for_value(0)),
            ),
            // (an overflow check that is switched off is no branch at all)
            TerminatorKind::Assert { cond, expected, target, msg, .. }
                if is_t(cond) && !self.overflow_check_off(msg) =>
            {
                if *expected {
                    (Target::Block(*target), Target::Panic)
                } else {
                    (Target::Panic, Target::Block(*target))
                }
            }
            _ => return None,
        };
        let (t_true, t_false) = if negated { (t_false, t_true) } else { (t_true, t_false) };
        Some((i, also, *op, &ops.0, &ops.1, t_true, t_false))
    }

    /// Whether the comparison `x op y` at statement `at` of block `bb` is
    /// known to hold: a block that every way to this one passes is only
    /// entered by the true branch of the same comparison of the same
    /// values, and nothing in between can change them
    /// (`if n < buf.len() { ...; buf[n] = c }`: the check of the index).
    ///
    /// "The same values": the same constants, or locals that are copies of
    /// the same place, made in the block of their comparison. "Nothing in
    /// between": only assignments to locals that no reference exists to
    /// and that the places do not name, no calls.
    fn implied(&self, bb: BasicBlock, at: usize, op: BinOp, x: &'tcx Operand<'tcx>, y: &'tcx Operand<'tcx>) -> bool {
        let mir = self.mir;
        let blocks = &mir.basic_blocks;
        let preds = blocks.predecessors();
        let doms = blocks.dominators();
        let u = &self.uses;
        // What an operand is: a constant, a place, or the length of a
        // slice; and the statement in its block that reads it into the
        // local of the operand.
        #[derive(PartialEq)]
        enum Key<'tcx> {
            Const,
            Place(Place<'tcx>),
            Len(Place<'tcx>),
        }
        let source = |o: &'tcx Operand<'tcx>, block: BasicBlock, upto: usize| -> Option<(Key<'tcx>, Option<usize>)> {
            match o {
                Operand::Constant(_) => Some((Key::Const, None)),
                Operand::Copy(pl) | Operand::Move(pl) if pl.projection.is_empty() => {
                    let l = pl.local.as_usize();
                    if u.borrowed[l] {
                        return None;
                    }
                    // a local that is assigned in several places: itself
                    // (nothing between the comparisons assigns it)
                    let &[d] = &u.def_locs[l][..] else { return Some((Key::Place(*pl), None)) };
                    if d.block != block {
                        return Some((Key::Place(*pl), None));
                    }
                    let stmt = blocks[d.block].statements.get(d.statement_index)?;
                    let StatementKind::Assign(b) = &stmt.kind else { return None };
                    if d.statement_index >= upto || u.defs[l] != 1 {
                        return None;
                    }
                    match &b.1 {
                        Rvalue::Use(Operand::Copy(src) | Operand::Move(src), _) => {
                            Some((Key::Place(*src), Some(d.statement_index)))
                        }
                        Rvalue::UnaryOp(UnOp::PtrMetadata, Operand::Copy(src) | Operand::Move(src)) => {
                            Some((Key::Len(*src), Some(d.statement_index)))
                        }
                        _ => None,
                    }
                }
                _ => None,
            }
        };
        let quiet = |s: &rustc_middle::mir::Statement<'tcx>| {
            matches!(s.kind, StatementKind::StorageLive(_) | StatementKind::StorageDead(_) | StatementKind::Nop)
        };
        // The block that the true branch of the first comparison enters:
        // up the dominators of this block.
        let mut q = bb;
        for _ in 0..8 {
            if let &[p] = &preds[q][..]
                && p != q
                && let Some((k, None, op2, x2, y2, Target::Block(t), _)) = self.fusable(p)
                && t == q
                && op2 == op
                && blocks[p].terminator().successors().filter(|s| *s == q).count() == 1
            {
                let mut copies: Vec<(BasicBlock, usize)> = Vec::new();
                let mut places: Vec<Place<'tcx>> = Vec::new();
                let mut direct: Vec<Local> = Vec::new();
                let mut same = |a: &'tcx Operand<'tcx>, b: &'tcx Operand<'tcx>| -> bool {
                    // the same local on both sides
                    if let (Operand::Copy(pa) | Operand::Move(pa), Operand::Copy(pb) | Operand::Move(pb)) = (a, b)
                        && pa.projection.is_empty()
                        && pb.projection.is_empty()
                        && pa.local == pb.local
                    {
                        direct.push(pa.local);
                        return !u.borrowed[pa.local.as_usize()];
                    }
                    let (Some((sa, da)), Some((sb, db))) = (source(a, bb, at), source(b, p, k)) else { return false };
                    copies.extend(da.map(|i| (bb, i)));
                    copies.extend(db.map(|i| (p, i)));
                    match (sa, sb) {
                        (Key::Const, Key::Const) => match (a, b) {
                            (Operand::Constant(c), Operand::Constant(d)) => c.const_ == d.const_,
                            _ => false,
                        },
                        (Key::Const, _) | (_, Key::Const) => false,
                        (ka, kb) => {
                            let same = ka == kb;
                            let (Key::Place(pl) | Key::Len(pl)) = ka else { unreachable!() };
                            places.push(pl);
                            same
                        }
                    }
                };
                if same(x, x2) && same(y, y2) {
                    // the locals the places name
                    let mut named = Mentions(Vec::new());
                    for pl in &places {
                        named.visit_place(
                            pl,
                            PlaceContext::NonMutatingUse(NonMutatingUseContext::Copy),
                            Location::START,
                        );
                    }
                    // (in the block of the first comparison a local that
                    // both sides read as it is may still get its value)
                    let sources = named.0.clone();
                    named.0.extend(direct.iter().copied());
                    let harmless_for = |names: &[Local], block: BasicBlock, i: usize, s: &rustc_middle::mir::Statement<'tcx>| {
                        quiet(s)
                            || copies.contains(&(block, i))
                            || matches!(&s.kind, StatementKind::Assign(b)
                                if !b.0.is_indirect()
                                    && !u.borrowed[b.0.local.as_usize()]
                                    && !names.contains(&b.0.local))
                    };
                    let harmless = |block: BasicBlock, i: usize, s: &rustc_middle::mir::Statement<'tcx>| {
                        harmless_for(&named.0, block, i, s)
                    };
                    // in the block of the first comparison: only the copies
                    let first = copies.iter().filter(|c| c.0 == p).map(|c| c.1).min().unwrap_or(k);
                    if !blocks[p].statements.iter().enumerate().take(k).skip(first).all(|(i, s)| {
                        harmless_for(&sources, p, i, s)
                    }) {
                        return false;
                    }
                    // the blocks between `q` and this one
                    let n = blocks.len();
                    let mut down = vec![false; n];
                    let mut work = vec![q];
                    while let Some(b) = work.pop() {
                        if std::mem::replace(&mut down[b.as_usize()], true) || b == bb {
                            continue;
                        }
                        work.extend(blocks[b].terminator().successors());
                    }
                    let mut up = vec![false; n];
                    let mut work = vec![bb];
                    while let Some(b) = work.pop() {
                        if std::mem::replace(&mut up[b.as_usize()], true) || b == q {
                            continue;
                        }
                        work.extend(preds[b].iter().copied());
                    }
                    return blocks.indices().filter(|b| down[b.as_usize()] && up[b.as_usize()]).all(|b| {
                        let data = &blocks[b];
                        let upto = if b == bb { at } else { data.statements.len() };
                        doms.dominates(q, b)
                            // (no way in from a block that is not checked)
                            && (b == q || preds[b].iter().all(|p| down[p.as_usize()] && up[p.as_usize()]))
                            && data.statements.iter().enumerate().take(upto).all(|(i, s)| harmless(b, i, s))
                            && (b == bb
                                || !matches!(
                                    data.terminator().kind,
                                    TerminatorKind::Call { .. }
                                        | TerminatorKind::Drop { .. }
                                        | TerminatorKind::TailCall { .. }
                                        | TerminatorKind::InlineAsm { .. }
                                ))
                    });
                }
            }
            // further up
            match doms.immediate_dominator(q) {
                Some(d) if d != q => q = d,
                _ => return false,
            }
        }
        false
    }

    /// The locals of two bytes (`usize`, `u16`) whose value is always below
    /// 256: a count or an index that starts at a small constant or a byte
    /// and is only stepped where it is known to be below a constant
    /// (`while n < 40 { ...; n += 1 }`, the index of an array). They keep
    /// their two bytes, but where they are read the second one is the
    /// constant 0, and so the stores to it go away as stores nobody reads.
    ///
    /// Every assignment to such a local is one of: a constant below 256, a
    /// value of one byte that is extended, or `n + k` in a place where
    /// `n < c` holds with `c + k <= 256`.
    fn narrow_locals(&self) -> Vec<bool> {
        let mir = self.mir;
        let blocks = &mir.basic_blocks;
        let u = &self.uses;
        let n = mir.local_decls.len();
        let mut out = vec![false; n];
        if std::env::var_os("RUST64_NO_NARROW").is_some() {
            return out;
        }
        let wide = std::env::var_os("RUST64_NO_NARROW_MORE").is_none();
        // (in rounds: a local may fit because another one does)
        for _ in 0..8 {
        let mut changed = false;
        // (the result too: it keeps its bytes for the caller)
        'locals: for l in std::iter::once(0).chain(mir.arg_count + 1..n) {
            let local = Local::from_usize(l);
            let ty = self.local_ty(local);
            if out[l] {
                continue;
            }
            if !matches!(ty.kind(), ty::Uint(_))
                || self.layout(ty).size.bytes() != 2
                || u.borrowed[l]
                || u.def_locs[l].is_empty()
                || u.defs[l] as usize != u.def_locs[l].len()
                || self.addrs[l].is_some()
                || self.lazy_cand[l]
                || self.fwd[l].is_some()
                || self.res_cand[l]
                || self.consts[l].is_some()
            {
                continue;
            }
            for &d in &u.def_locs[l] {
                // (a local that fits: one of these, or a byte that is
                // extended, which may be a view without storage)
                let fits_local = |q: Local| {
                    let i = q.as_usize();
                    out[i]
                        || (u.defs[i] == 1 && !u.borrowed[i] && {
                            let stmt = match &u.def_locs[i][..] {
                                &[loc] => blocks[loc.block].statements.get(loc.statement_index),
                                _ => None,
                            };
                            matches!(stmt.map(|s| &s.kind), Some(StatementKind::Assign(b))
                                if b.0.projection.is_empty()
                                    && matches!(&b.1, Rvalue::Cast(CastKind::IntToInt, src, _) if {
                                        let from = self.monomorphize(src.ty(&mir.local_decls, self.tcx()));
                                        matches!(from.kind(), ty::Uint(_)) && self.layout(from).size.bytes() == 1
                                    }))
                        })
                };
                let fits = |o: &Operand<'tcx>| {
                    self.const_bits(o).is_some_and(|v| v < 256)
                        || matches!(o, Operand::Copy(p) | Operand::Move(p)
                            if p.projection.is_empty() && fits_local(p.local))
                };
                let Some(stmt) = blocks[d.block].statements.get(d.statement_index) else {
                    // the result of a call: the smaller of two values of
                    // which one fits, or `(a + n) - a` where `n` fits
                    let TerminatorKind::Call { func, args, destination, .. } = &blocks[d.block].terminator().kind
                    else {
                        continue 'locals;
                    };
                    let name = func
                        .const_fn_def()
                        .filter(|(id, _)| wide && self.tcx().intrinsic(*id).is_some())
                        .map(|(id, _)| self.tcx().item_name(id));
                    let ok = destination.projection.is_empty()
                        && match name.as_ref().map(|n| n.as_str()) {
                            Some("integer_min") => args.iter().any(|a| fits(&a.node)),
                            Some("ptr_offset_from_unsigned") if args.len() == 2 => {
                                let ty = self.monomorphize(args[0].node.ty(&mir.local_decls, self.tcx()));
                                ty.builtin_deref(true).is_some_and(|t| self.layout(t).size.bytes() == 1)
                                    && self
                                        .sum_diff(&args[0].node, &args[1].node)
                                        .is_some_and(fits_local)
                            }
                            _ => false,
                        };
                    if !ok {
                        continue 'locals;
                    }
                    continue;
                };
                let StatementKind::Assign(b) = &stmt.kind else { continue 'locals };
                let ok = b.0.projection.is_empty()
                    && match &b.1 {
                        Rvalue::Use(k @ Operand::Constant(_), ..) => self.const_bits(k).is_some_and(|v| v < 256),
                        Rvalue::Use(k @ (Operand::Copy(_) | Operand::Move(_)), ..) => wide && fits(k),
                        Rvalue::Cast(CastKind::IntToInt, src, _) => {
                            let from = self.monomorphize(src.ty(&mir.local_decls, self.tcx()));
                            matches!(from.kind(), ty::Uint(_)) && self.layout(from).size.bytes() == 1
                        }
                        Rvalue::BinaryOp(BinOp::Add | BinOp::AddUnchecked, ops) => match (&ops.0, &ops.1) {
                            (Operand::Copy(x) | Operand::Move(x), k @ Operand::Constant(_))
                                if x.projection.is_empty() =>
                            {
                                self.const_bits(k).is_some_and(|k| {
                                    k < 256 && self.below(local, x.local, d, 256 - k, if wide { &out } else { &[] })
                                })
                            }
                            _ => false,
                        },
                        _ => false,
                    };
                if !ok {
                    continue 'locals;
                }
            }
            out[l] = true;
            changed = true;
        }
        if !changed {
            break;
        }
        }
        out
    }

    /// Whether `x`, which is `local` or a copy of it, is below a constant
    /// of at most `limit` at `at`, where `local` is assigned: a block that
    /// every way to `at` passes is only entered by the true branch of
    /// `local < c`, and `local` is not assigned from there to `at`. With a
    /// `limit` of 255, `c` may also be a local that fits a byte (`narrow`).
    fn below(&self, local: Local, x: Local, at: Location, limit: u128, narrow: &[bool]) -> bool {
        let mir = self.mir;
        let blocks = &mir.basic_blocks;
        let preds = blocks.predecessors();
        let doms = blocks.dominators();
        let u = &self.uses;
        let defs = &u.def_locs[local.as_usize()];
        // The statement that makes `t` a copy of `local`.
        let copy_at = |t: Local| -> Option<Location> {
            let i = t.as_usize();
            let &[loc] = &u.def_locs[i][..] else { return None };
            if u.defs[i] != 1 || u.borrowed[i] {
                return None;
            }
            let StatementKind::Assign(b) = &blocks[loc.block].statements.get(loc.statement_index)?.kind else {
                return None;
            };
            match &b.1 {
                Rvalue::Use(Operand::Copy(src) | Operand::Move(src), ..)
                    if b.0.projection.is_empty() && src.projection.is_empty() && src.local == local =>
                {
                    Some(loc)
                }
                _ => None,
            }
        };
        let via = if x == local {
            None
        } else {
            match copy_at(x) {
                Some(loc) => Some(loc),
                None => return false,
            }
        };
        let mut q = at.block;
        for _ in 0..8 {
            'here: {
                let &[p] = &preds[q][..] else { break 'here };
                if p == q || blocks[p].terminator().successors().filter(|s| *s == q).count() != 1 {
                    break 'here;
                }
                let Some((k, _, op, a, c, t_true, t_false)) = self.fusable(p) else { break 'here };
                let holds = match op {
                    BinOp::Lt => t_true == Target::Block(q),
                    BinOp::Ge => t_false == Target::Block(q),
                    _ => false,
                };
                let bound = self.const_bits(c).is_some_and(|c| c <= limit)
                    || (limit >= 255
                        && matches!(c, Operand::Copy(m) | Operand::Move(m)
                            if m.projection.is_empty() && narrow.get(m.local.as_usize()) == Some(&true)));
                if !holds || !bound {
                    break 'here;
                }
                // what is compared is the local as it is at the end of `p`
                let (Operand::Copy(a) | Operand::Move(a)) = a else { break 'here };
                if !a.projection.is_empty() {
                    break 'here;
                }
                let from = if a.local == local {
                    k
                } else {
                    match copy_at(a.local) {
                        Some(loc) if loc.block == p && loc.statement_index < k => loc.statement_index,
                        _ => break 'here,
                    }
                };
                if defs.iter().any(|e| e.block == p && e.statement_index >= from) {
                    break 'here;
                }
                // the copy that is stepped is made on this way
                if let Some(loc) = via {
                    let before = if loc.block == at.block {
                        loc.statement_index < at.statement_index
                    } else {
                        doms.dominates(loc.block, at.block)
                    };
                    if !before || !doms.dominates(q, loc.block) {
                        break 'here;
                    }
                }
                // the blocks on the ways from `q` to `at`
                let mut seen = vec![false; blocks.len()];
                let mut work: Vec<BasicBlock> = if at.block == q { Vec::new() } else { preds[at.block].to_vec() };
                while let Some(b) = work.pop() {
                    if std::mem::replace(&mut seen[b.as_usize()], true) || b == q {
                        continue;
                    }
                    work.extend(preds[b].iter().copied());
                }
                // (a way from `at` back to it that does not pass the check)
                if seen[at.block.as_usize()] {
                    break 'here;
                }
                if defs.iter().any(|e| {
                    *e != at
                        && (seen[e.block.as_usize()] || e.block == at.block && e.statement_index < at.statement_index)
                }) {
                    break 'here;
                }
                return true;
            }
            match doms.immediate_dominator(q) {
                Some(d) if d != q => q = d,
                _ => return false,
            }
        }
        false
    }

    /// A `match` on an enum with two variants that is stored without a tag
    /// A `match` on a byte whose arms only give one variable a constant
    /// byte each (`match c { b'/' => 2, b'#' => 3, .. , _ => 0 }`): a table
    /// with the values from the smallest to the largest case, and
    ///
    /// ```text
    ///         lda c
    ///         sec
    ///         sbc #first
    ///         cmp #count
    ///         bcs other
    ///         tax
    ///         lda table,x
    /// ```
    ///
    /// in place of a comparison per case. Taken where the table is not
    /// larger than the comparisons and arms it replaces (about 11 bytes per
    /// case); true if it was compiled that way.
    fn lookup_switch(
        &mut self,
        discr: &Operand<'tcx>,
        targets: &rustc_middle::mir::SwitchTargets,
        next: Option<BasicBlock>,
    ) -> bool {
        if std::env::var_os("RUST64_NO_LOOKUP").is_some() {
            return false;
        }
        let mir = self.mir;
        let ty = self.monomorphize(discr.ty(&mir.local_decls, self.tcx()));
        if self.layout(ty).size.bytes() != 1 || targets.iter().count() < 4 {
            return false;
        }
        // what an arm is: the variable, its value, and where it goes on
        let arm = |this: &Self, bb: BasicBlock| -> Option<(Local, u8, BasicBlock)> {
            let data = &mir.basic_blocks[bb];
            let TerminatorKind::Goto { target, .. } = data.terminator().kind else { return None };
            let mut found = None;
            for stmt in &data.statements {
                match &stmt.kind {
                    StatementKind::StorageLive(_) | StatementKind::StorageDead(_) | StatementKind::Nop => {}
                    StatementKind::Assign(b) if found.is_none() => {
                        let (place, Rvalue::Use(k @ Operand::Constant(_), ..)) = &**b else { return None };
                        let size = this.layout(this.local_ty(place.local)).size.bytes();
                        if !place.projection.is_empty() || size != 1 {
                            return None;
                        }
                        found = Some((place.local, this.const_bits(k)? as u8, target));
                    }
                    _ => return None,
                }
            }
            found
        };
        let other = &mir.basic_blocks[targets.otherwise()];
        let impossible = matches!(other.terminator().kind, TerminatorKind::Unreachable);
        let default = arm(self, targets.otherwise());
        if default.is_none() && !impossible {
            return false;
        }
        let mut cases: Vec<(u8, u8)> = Vec::new();
        let mut same: Option<(Local, BasicBlock)> = default.map(|(l, _, j)| (l, j));
        for (value, target) in targets.iter() {
            let Some((l, k, j)) = arm(self, target) else { return false };
            if value > 255 || same.is_some_and(|s| s != (l, j)) {
                return false;
            }
            same = Some((l, j));
            cases.push((value as u8, k));
        }
        let Some((dest, join)) = same else { return false };
        let (Some(&min), Some(&max)) = (cases.iter().map(|c| &c.0).min(), cases.iter().map(|c| &c.0).max()) else {
            return false;
        };
        let range = (max - min) as usize + 1;
        if range + 16 > 11 * cases.len() {
            return false;
        }
        let fill = default.map_or(0, |d| d.1);
        let mut table = vec![fill; range];
        for &(value, k) in &cases {
            table[(value - min) as usize] = k;
        }
        let name = format!("T{}", self.p.ntables);
        self.p.ntables += 1;
        let _ = writeln!(self.p.tables, "; the values of a match in {}\n{name}:", self.p.name(self.id));
        for row in table.chunks(16) {
            let bytes: Vec<String> = row.iter().map(|b| format!("${b:02x}")).collect();
            let _ = writeln!(self.p.tables, "        .byte {}", bytes.join(","));
        }
        let (v, _) = self.operand(discr);
        let (d, _, _) = self.place(Place::from(dest));
        let a = &mut self.p.asm;
        a.setup_val(&v, 0);
        a.setup(&d, 2);
        a.lda(&v, 0, 0);
        if min != 0 {
            a.ins("sec");
            a.ins(format!("sbc #${min:02x}"));
        }
        let (lother, lstore) = (a.new_label(), a.new_label());
        if !impossible || default.is_some() {
            if range < 256 {
                a.ins(format!("cmp #${range:02x}"));
                a.ins(format!("bcs {lother}"));
            }
        }
        a.ins("tax");
        a.ins(format!("lda {name},x"));
        a.ins(format!("jmp {lstore}"));
        a.label(&lother);
        a.ins(format!("lda #${fill:02x}"));
        a.label(&lstore);
        a.sta(&d, 0, 2);
        self.goto(join, next);
        true
    }

    /// of its own (`Option<&T>`, `Result<Dir, Error>` with the error in a
    /// value the data cannot have): the block ends with the discriminant
    /// and a switch on it, and the discriminant is used nowhere else. Then
    /// one comparison of the tag with the niche value decides: the index of
    /// the statement, the enum, the niche value, and the targets for the
    /// variant of the niche and for the other one.
    fn niche_switch(&self, bb: BasicBlock) -> Option<(usize, Place<'tcx>, u8, BasicBlock, BasicBlock)> {
        let data = &self.mir.basic_blocks[bb];
        let TerminatorKind::SwitchInt { discr, targets } = &data.terminator().kind else { return None };
        let (i, stmt) = data.statements.iter().enumerate().rev().find(|(_, s)| {
            !matches!(s.kind, StatementKind::StorageDead(_) | StatementKind::StorageLive(_) | StatementKind::Nop)
        })?;
        let StatementKind::Assign(b) = &stmt.kind else { return None };
        let (place, Rvalue::Discriminant(of)) = &**b else { return None };
        let t = place.local.as_usize();
        let u = &self.uses;
        let term_loc = Location { block: bb, statement_index: data.statements.len() };
        let reads = matches!(discr, Operand::Copy(p) | Operand::Move(p) if p.projection.is_empty() && p.local == place.local);
        if !reads
            || !place.projection.is_empty()
            || u.defs[t] != 1
            || u.uses[t] != 1
            || u.plain[t] != 1
            || u.borrowed[t]
            || u.plain_loc[t] != Some(term_loc)
            || self.consts[t].is_some()
            || self.lazy_cand[of.local.as_usize()]
        {
            return None;
        }
        let layout = self.layout(self.monomorphize(of.ty(self.mir, self.tcx()).ty));
        let Variants::Multiple { tag, tag_encoding, variants, .. } = &layout.variants else { return None };
        let TagEncoding::Niche { untagged_variant, niche_variants, niche_start } = tag_encoding else { return None };
        if tag.size(&self.p.cx).bytes() != 1 || niche_variants.start != niche_variants.last || variants.len() != 2 {
            return None;
        }
        let value = |v: VariantIdx| {
            layout.ty.discriminant_for_variant(self.tcx(), v).map_or(v.as_u32() as u128, |d| d.val)
        };
        Some((
            i,
            *of,
            *niche_start as u8,
            targets.target_for_value(value(niche_variants.start)),
            targets.target_for_value(value(*untagged_variant)),
        ))
    }

    /// An overflow check in code that is compiled without them: functions
    /// with `#[rustc_inherit_overflow_checks]` (the arithmetic of `core`,
    /// the counters of its iterators) carry the check in their MIR, and
    /// the crate that is compiled decides (as in rustc's own backends).
    fn overflow_check_off(&self, msg: &rustc_middle::mir::AssertMessage<'tcx>) -> bool {
        !self.tcx().sess.overflow_checks() && msg.is_optional_overflow_check()
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
    /// such a local, such a local plus a constant, or a reference to a
    /// place with a known address) and never borrowed or written through a
    /// projection. A pointer in a wrapper (`NonNull`) counts where the
    /// local is only used as a whole.
    fn addr_locals(&mut self) -> Vec<Option<String>> {
        struct Parts(Vec<bool>);
        impl<'tcx> Visitor<'tcx> for Parts {
            fn visit_place(&mut self, place: &Place<'tcx>, _: PlaceContext, _: Location) {
                if !place.projection.is_empty() {
                    self.0[place.local.as_usize()] = true;
                }
            }
        }
        let n = self.mir.local_decls.len();
        let mut parts = Parts(vec![false; n]);
        parts.visit_body(self.mir);
        let parts = parts.0;
        let extended = std::env::var_os("RUST64_NO_ADDR_SUMS").is_none();
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
        // (where the pointers to slices point to: those keep their storage,
        // but a thin pointer made of one has a known address again)
        let mut fat: Vec<Option<String>> = vec![None; n];
        // (a known address plus a local that is assigned once: the
        // difference to that address is the local again)
        let mut sums: Vec<Option<(Local, Local)>> = vec![None; n];
        for _ in 0..if extended { 12 } else { 6 } {
            let mut changed = false;
            for &(l, rv) in &cands {
                let i = l.as_usize();
                if assigns[i] != 1 || blocked[i] || addrs[i].is_some() || fat[i].is_some() || sums[i].is_some() {
                    continue;
                }
                let ty = self.local_ty(l);
                let layout = self.layout(ty);
                if extended && layout.size.bytes() == 4 && (ty.builtin_deref(true).is_some() || !parts[i]) {
                    let v = match rv {
                        Rvalue::Use(Operand::Copy(p) | Operand::Move(p), _)
                        | Rvalue::Cast(_, Operand::Copy(p) | Operand::Move(p), _)
                            if p.projection.is_empty() =>
                        {
                            let q = p.local.as_usize();
                            let pointer = self.local_ty(p.local).builtin_deref(true).is_some() || !parts[q];
                            fat[q].clone().or_else(|| addrs[q].clone()).filter(|_| pointer)
                        }
                        Rvalue::Ref(_, _, p) | Rvalue::RawPtr(_, p)
                            if p.projection.len() == 1 && p.projection[0] == ProjectionElem::Deref =>
                        {
                            fat[p.local.as_usize()].clone()
                        }
                        // (a pointer to a slice made of its address and length)
                        Rvalue::Aggregate(kind, fields) if matches!(**kind, AggregateKind::RawPtr(..)) => {
                            match fields.iter().next() {
                                Some(Operand::Copy(p) | Operand::Move(p)) if p.projection.is_empty() => {
                                    addrs[p.local.as_usize()].clone()
                                }
                                _ => None,
                            }
                        }
                        _ => None,
                    };
                    if v.is_some() {
                        fat[i] = v;
                        changed = true;
                    }
                    continue;
                }
                let wrapped = extended
                    && !parts[i]
                    && matches!(layout.backend_repr, rustc_abi::BackendRepr::Scalar(s) if matches!(s.primitive(), Primitive::Pointer(_)));
                if (ty.builtin_deref(true).is_none() && !wrapped) || layout.size.bytes() != 2 {
                    continue;
                }
                let v = match rv {
                    Rvalue::Use(op, _) | Rvalue::Cast(_, op, _) => match op {
                        Operand::Copy(p) | Operand::Move(p) if p.projection.is_empty() => {
                            let q = p.local.as_usize();
                            addrs[q].clone().or_else(|| fat[q].clone().filter(|_| matches!(rv, Rvalue::Cast(..))))
                        }
                        Operand::Constant(c) => {
                            let (v, l) = self.constant(c);
                            if l.size.bytes() == 2 { Self::imm_addr(&v) } else { None }
                        }
                        _ => None,
                    },
                    Rvalue::Ref(_, _, p) | Rvalue::RawPtr(_, p) => self.static_place_addr(*p, &addrs),
                    Rvalue::BinaryOp(BinOp::Offset, pair) if extended => match &**pair {
                        (Operand::Copy(p) | Operand::Move(p), k) if p.projection.is_empty() => {
                            let elem = self
                                .local_ty(p.local)
                                .builtin_deref(true)
                                .map(|t| self.layout(t).size.bytes());
                            match (addrs[p.local.as_usize()].clone(), self.const_bits(k), elem) {
                                (Some(e), Some(k), Some(elem)) if !e.starts_with('$') => {
                                    let k = (k as u64).wrapping_mul(elem) & 0xffff;
                                    Some(if k == 0 { e } else { format!("{e}+{k}") })
                                }
                                _ => None,
                            }
                        }
                        _ => None,
                    },
                    _ => None,
                };
                if v.is_some() {
                    addrs[i] = v;
                    changed = true;
                    continue;
                }
                if !extended {
                    continue;
                }
                let sum = match rv {
                    Rvalue::Use(Operand::Copy(p) | Operand::Move(p), _)
                    | Rvalue::Cast(_, Operand::Copy(p) | Operand::Move(p), _)
                        if p.projection.is_empty() =>
                    {
                        sums[p.local.as_usize()]
                    }
                    Rvalue::BinaryOp(BinOp::Offset, pair) => match &**pair {
                        (Operand::Copy(p) | Operand::Move(p), Operand::Copy(k) | Operand::Move(k))
                            if p.projection.is_empty() && k.projection.is_empty() =>
                        {
                            let q = k.local.as_usize();
                            let bytes = self
                                .local_ty(p.local)
                                .builtin_deref(true)
                                .is_some_and(|t| self.layout(t).size.bytes() == 1);
                            (bytes
                                && addrs[p.local.as_usize()].is_some()
                                && assigns[q] == 1
                                && !blocked[q]
                                && self.layout(self.local_ty(k.local)).size.bytes() == 2)
                                .then_some((p.local, k.local))
                        }
                        _ => None,
                    },
                    _ => None,
                };
                if sum.is_some() {
                    sums[i] = sum;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        self.sums = sums;
        // The same for pointers whose address is not known: what they are
        // made of. A pointer local that never changes (assigned once and
        // not borrowed, or an argument that is not assigned) is a root; a
        // copy, a cast, a reference to a field behind it, or it plus a
        // constant is that root plus a constant.
        if extended && std::env::var_os("RUST64_NO_PTR_SYMS").is_none() {
            let args = self.mir.arg_count;
            let kinds: Vec<u64> = (0..n)
                .map(|l| {
                    let stable = (assigns[l] == 1 && !blocked[l])
                        || ((1..=args).contains(&l) && assigns[l] == 0 && !self.uses.borrowed[l]);
                    let ty = self.local_ty(Local::from_usize(l));
                    let layout = self.layout(ty);
                    let pointer = |s: rustc_abi::Scalar| matches!(s.primitive(), Primitive::Pointer(_));
                    let ptr = ty.builtin_deref(true).is_some()
                        || (!parts[l]
                            && match layout.backend_repr {
                                rustc_abi::BackendRepr::Scalar(s) => pointer(s),
                                rustc_abi::BackendRepr::ScalarPair { a: s, .. } => pointer(s),
                                _ => false,
                            });
                    let size = layout.size.bytes();
                    if stable && ptr && (size == 2 || size == 4) { size } else { 0 }
                })
                .collect();
            let mut def: Vec<Option<&'tcx Rvalue<'tcx>>> = vec![None; n];
            for &(l, rv) in &cands {
                if assigns[l.as_usize()] == 1 {
                    def[l.as_usize()] = Some(rv);
                }
            }
            let field_off = |p: &Place<'tcx>| -> Option<u64> {
                let mut layout = self.layout(self.local_ty(p.local).builtin_deref(true)?);
                let mut off = 0;
                for elem in p.projection.iter().skip(1) {
                    match elem {
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
                        _ => return None,
                    }
                }
                Some(off)
            };
            // Err: not made of another pointer (a root); Ok(None): of one
            // that is not known (yet)
            let derive = |i: usize, sym: &[Option<(Local, u64)>]| -> Result<Option<(Local, u64)>, ()> {
                let Some(rv) = def[i] else { return Err(()) };
                match rv {
                    Rvalue::Use(Operand::Copy(p) | Operand::Move(p), _)
                    | Rvalue::Cast(_, Operand::Copy(p) | Operand::Move(p), _)
                        if p.projection.is_empty() && kinds[p.local.as_usize()] != 0 =>
                    {
                        Ok(sym[p.local.as_usize()])
                    }
                    Rvalue::Ref(_, _, p) | Rvalue::RawPtr(_, p)
                        if p.projection.first() == Some(&ProjectionElem::Deref) =>
                    {
                        let q = p.local.as_usize();
                        if kinds[q] != 0 && p.projection.len() == 1 {
                            Ok(sym[q])
                        } else if kinds[q] == 2 {
                            let off = field_off(p).ok_or(())?;
                            Ok(sym[q].map(|(r, o)| (r, o + off)))
                        } else {
                            Err(())
                        }
                    }
                    Rvalue::BinaryOp(BinOp::Offset, pair) => match &**pair {
                        (Operand::Copy(p) | Operand::Move(p), k)
                            if p.projection.is_empty() && kinds[p.local.as_usize()] == 2 =>
                        {
                            let k = self.const_bits(k).ok_or(())? as u64;
                            let elem = self.local_ty(p.local).builtin_deref(true).ok_or(())?;
                            let k = k.wrapping_mul(self.layout(elem).size.bytes()) & 0xffff;
                            Ok(sym[p.local.as_usize()].map(|(r, o)| (r, o + k)))
                        }
                        _ => Err(()),
                    },
                    Rvalue::Aggregate(kind, fields) if matches!(**kind, AggregateKind::RawPtr(..)) => {
                        match fields.iter().next() {
                            Some(Operand::Copy(p) | Operand::Move(p))
                                if p.projection.is_empty() && kinds[p.local.as_usize()] == 2 =>
                            {
                                Ok(sym[p.local.as_usize()])
                            }
                            _ => Err(()),
                        }
                    }
                    _ => Err(()),
                }
            };
            let mut sym: Vec<Option<(Local, u64)>> = vec![None; n];
            loop {
                let mut changed = false;
                for i in 0..n {
                    if kinds[i] == 0 || sym[i].is_some() {
                        continue;
                    }
                    match derive(i, &sym) {
                        Ok(Some(v)) => {
                            sym[i] = Some(v);
                            changed = true;
                        }
                        Err(()) if kinds[i] == 2 => {
                            sym[i] = Some((Local::from_usize(i), 0));
                            changed = true;
                        }
                        _ => {}
                    }
                }
                if !changed {
                    // (made of a pointer that stays unknown: a root too)
                    match (0..n).find(|&i| kinds[i] == 2 && sym[i].is_none()) {
                        Some(i) => sym[i] = Some((Local::from_usize(i), 0)),
                        None => break,
                    }
                }
            }
            let mut sum: Vec<Option<(Local, u64, Local)>> = vec![None; n];
            loop {
                let mut changed = false;
                for &(l, rv) in &cands {
                    let i = l.as_usize();
                    if kinds[i] != 2 || sum[i].is_some() || def[i].is_none() {
                        continue;
                    }
                    let v = match rv {
                        Rvalue::Use(Operand::Copy(p) | Operand::Move(p), _)
                        | Rvalue::Cast(_, Operand::Copy(p) | Operand::Move(p), _)
                            if p.projection.is_empty() =>
                        {
                            sum[p.local.as_usize()]
                        }
                        Rvalue::BinaryOp(BinOp::Offset, pair) => match &**pair {
                            (Operand::Copy(p) | Operand::Move(p), Operand::Copy(k) | Operand::Move(k))
                                if p.projection.is_empty() && k.projection.is_empty() =>
                            {
                                let (q, c) = (p.local.as_usize(), k.local.as_usize());
                                let bytes = self
                                    .local_ty(p.local)
                                    .builtin_deref(true)
                                    .is_some_and(|t| self.layout(t).size.bytes() == 1);
                                match sym[q] {
                                    Some((r, o))
                                        if bytes
                                            && kinds[q] == 2
                                            && assigns[c] == 1
                                            && !blocked[c]
                                            && self.layout(self.local_ty(k.local)).size.bytes() == 2 =>
                                    {
                                        Some((r, o, k.local))
                                    }
                                    _ => None,
                                }
                            }
                            _ => None,
                        },
                        _ => None,
                    };
                    if v.is_some() {
                        sum[i] = v;
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
            }
            for i in 0..n {
                if kinds[i] != 2 {
                    sym[i] = None;
                }
            }
            self.syms = sym;
            self.sym_sums = sum;
        }
        addrs
    }

    /// For the difference of two pointers to bytes, `a - b`: the local `n`
    /// if `a` is `b + n` (`sums`).
    fn sum_diff(&self, a: &Operand<'tcx>, b: &Operand<'tcx>) -> Option<Local> {
        let (Operand::Copy(a) | Operand::Move(a), Operand::Copy(b) | Operand::Move(b)) = (a, b) else {
            return None;
        };
        if !a.projection.is_empty() || !b.projection.is_empty() {
            return None;
        }
        if let Some(&Some((p, n))) = self.sums.get(a.local.as_usize())
            && let Some(base) = self.addrs[p.as_usize()].as_ref()
            && self.addrs[b.local.as_usize()].as_ref() == Some(base)
        {
            return Some(n);
        }
        // (the same root and constant)
        let &(r, o, n) = self.sym_sums.get(a.local.as_usize())?.as_ref()?;
        (self.syms.get(b.local.as_usize()) == Some(&Some((r, o)))).then_some(n)
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
        // The offset behind a pointer goes into Y. If it does not fit (a
        // field or a constant index far into a large array), the pointer
        // is moved first.
        if let Addr::Ind { ptr, off } = &addr
            && *off > 0
            && *off + layout.size.bytes() > 256
        {
            let t = self.temp(2);
            let (ptr, off) = (ptr.clone(), *off);
            let a = self.a();
            a.ins("clc");
            a.ins(format!("lda {ptr}"));
            a.ins(format!("adc #${:02x}", off & 0xff));
            a.ins(format!("sta {t}"));
            a.ins(format!("lda {ptr}+1"));
            a.ins(format!("adc #${:02x}", (off >> 8) & 0xff));
            a.ins(format!("sta {t}+1"));
            addr = Addr::Ind { ptr: t, off: 0 };
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
                {
                    match self.lazy_val(p.local) {
                        Some(Val::Imm(b)) => {
                            let size = l.size.bytes() as usize;
                            let part = b.into_iter().skip(off as usize).take(size).collect();
                            return (Val::Imm(part), l);
                        }
                        Some(v @ Val::Mem(_)) => return (v.offset(off), l),
                        None => {}
                    }
                }
                if p.projection.is_empty() {
                    if let Some(e) = self.addrs[p.local.as_usize()].clone() {
                        let v = Val::Imm(vec![format!("#<({e})"), format!("#>({e})")]);
                        return (v, self.layout(self.local_ty(p.local)));
                    }
                    // (a counter that stays below 256: its first byte and 0)
                    if self.narrow.get(p.local.as_usize()) == Some(&true) {
                        let v = Val::Imm(vec![self.local_addr(p.local), "#$00".to_owned()]);
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

    /// Whether an operand names a byte of a frame (`F<id>v<n>`, `F<id>t`).
    ///
    /// A 16-bit `x += k` on a frame variable is emitted as a plain addition:
    /// the data-flow pass may move it to the end of a loop (`opt.rs`,
    /// `sink_add`) and makes `inc x` / `bne` / `inc x+1` of it afterwards.
    fn in_frame(e: &str) -> bool {
        e.strip_prefix('F').is_some_and(|r| {
            let digits = r.bytes().take_while(u8::is_ascii_digit).count();
            digits > 0 && matches!(r.as_bytes().get(digits), Some(b'v' | b't'))
        })
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
        if add && n == 2 && Self::in_frame(d) {
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

    /// Shifts the `n` bytes at `t` by the constant `k`: first by whole
    /// bytes (moves), then bit by bit over the bytes that still change.
    fn shift_const(a: &mut Asm, t: &str, n: u64, k: u64, left: bool, signed: bool) {
        let at = |i: u64| if i == 0 { t.to_owned() } else { format!("{t}+{i}") };
        let (q, bits) = ((k / 8).min(n), k % 8);
        // the bytes that the bits are shifted through
        let m = n - q;
        if q > 0 && left {
            for i in (q..n).rev() {
                a.ins(format!("lda {}", at(i - q)));
                a.ins(format!("sta {}", at(i)));
            }
            a.ins("lda #$00");
            for i in 0..q {
                a.ins(format!("sta {}", at(i)));
            }
        } else if q > 0 {
            for i in 0..m {
                a.ins(format!("lda {}", at(i + q)));
                a.ins(format!("sta {}", at(i)));
            }
            if signed {
                // the sign: $ff or $00 (A is the old top byte, or it is
                // still in its place)
                if m == 0 {
                    a.ins(format!("lda {}", at(n - 1)));
                }
                a.ins("asl");
                a.ins("lda #$00");
                a.ins("adc #$ff");
                a.ins("eor #$ff");
            } else {
                a.ins("lda #$00");
            }
            for i in m..n {
                a.ins(format!("sta {}", at(i)));
            }
        }
        if m == 0 {
            return;
        }
        for _ in 0..bits {
            if left {
                a.ins(format!("asl {}", at(q)));
                for i in q + 1..n {
                    a.ins(format!("rol {}", at(i)));
                }
            } else {
                if signed {
                    a.ins(format!("lda {}", at(m - 1)));
                    a.ins("cmp #$80");
                    a.ins(format!("ror {}", at(m - 1)));
                } else {
                    a.ins(format!("lsr {}", at(m - 1)));
                }
                for i in (0..m - 1).rev() {
                    a.ins(format!("ror {}", at(i)));
                }
            }
        }
    }

    /// `dst = v << k` or `v >> k` for one byte: in the accumulator.
    fn shift_byte(a: &mut Asm, dst: &Addr, v: &Val, k: u64, left: bool, signed: bool) {
        a.setup_val(v, 0);
        a.setup(dst, 2);
        a.lda(v, 0, 0);
        for _ in 0..k {
            if left {
                a.ins("asl");
            } else if signed {
                a.ins("cmp #$80");
                a.ins("ror");
            } else {
                a.ins("lsr");
            }
        }
        a.sta(dst, 0, 2);
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
        // A known size: a loop in place.
        if let Some(c) = Self::imm_value(count, csize.min(8)) {
            let n = c.wrapping_mul(elem) & 0xffff;
            if n == 0 {
                return;
            }
            if n <= 256 {
                let s = self.deref_val(src);
                let d = self.deref_val(dst);
                if self.a().copy_block(&d, &s, n) {
                    return;
                }
            }
        }
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
                if es == 1 && (2..256).contains(&count) {
                    // a few bytes: a loop in place
                    self.a().fill_block(&dst, &v, count);
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

    /// For the division or remainder `x op y` of the statement that is
    /// being compiled: where the other of the two goes, if the code asks
    /// for it next (`d % g` and then `d / g`, the digits of a number). That
    /// statement is marked as done; the caller stores its result.
    ///
    /// "Next" is: further on in the same block, or in the blocks that
    /// follow through assertions and jumps and have no other way in (the
    /// checks for a zero divisor and for array bounds are assertions), with
    /// only assignments in between that neither write the operands nor
    /// name the local the result goes to. The operands and that local are
    /// plain locals that no reference is made to (or constants).
    fn div_partner(&mut self, op: BinOp, x: &Operand<'tcx>, y: &Operand<'tcx>) -> Option<Place<'tcx>> {
        let (mut bb, at) = self.cur?;
        let mir = self.mir;
        // (not a value that is computed where it is used)
        match &mir.basic_blocks[bb].statements.get(at)?.kind {
            StatementKind::Assign(b) => match &b.1 {
                Rvalue::BinaryOp(o, ops) if *o == op && std::ptr::eq(&ops.0, x) && std::ptr::eq(&ops.1, y) => {
                    if self.operand_local(x) == Some(b.0.local) || self.operand_local(y) == Some(b.0.local) {
                        return None;
                    }
                }
                _ => return None,
            },
            _ => return None,
        }
        let wanted = if op == BinOp::Div { BinOp::Rem } else { BinOp::Div };
        self.is_borrowed(Local::from_usize(0));
        let plain = |this: &Self, l: Local| {
            let i = l.as_usize();
            !this.borrowed.as_ref().unwrap()[i] && !this.lazy_cand[i] && this.addrs[i].is_none()
        };
        let operands = [self.operand_local(x), self.operand_local(y)];
        for (o, l) in [x, y].into_iter().zip(operands) {
            match (o, l) {
                (Operand::Constant(_), _) => {}
                (_, Some(l)) if plain(self, l) => {}
                _ => return None,
            }
        }
        let same = |this: &Self, a: &Operand<'tcx>, b: &Operand<'tcx>| match (a, b) {
            (Operand::Constant(c), Operand::Constant(d)) => c.const_ == d.const_,
            (Operand::Constant(_), _) | (_, Operand::Constant(_)) => false,
            _ => this.operand_local(a).is_some() && this.operand_local(a) == this.operand_local(b),
        };
        let mut named = Mentions(Vec::new());
        let mut from = at + 1;
        for _ in 0..6 {
            let data = &mir.basic_blocks[bb];
            for (i, stmt) in data.statements.iter().enumerate().skip(from) {
                let loc = Location { block: bb, statement_index: i };
                match &stmt.kind {
                    StatementKind::StorageLive(_) | StatementKind::StorageDead(_) | StatementKind::Nop => {}
                    StatementKind::Assign(b) => {
                        let (place, rv) = &**b;
                        if let Rvalue::BinaryOp(o, ops) = rv
                            && *o == wanted
                            && same(self, &ops.0, x)
                            && same(self, &ops.1, y)
                        {
                            if place.projection.is_empty()
                                && plain(self, place.local)
                                && !named.0.contains(&place.local)
                            {
                                self.done.insert((bb, i));
                                return Some(*place);
                            }
                            return None;
                        }
                        if operands.contains(&Some(place.local)) {
                            return None;
                        }
                        named.visit_statement(stmt, loc);
                    }
                    _ => return None,
                }
            }
            let term = data.terminator();
            let target = match &term.kind {
                TerminatorKind::Goto { target, .. } | TerminatorKind::Assert { target, .. } => *target,
                _ => return None,
            };
            if mir.basic_blocks.predecessors()[target].len() != 1 {
                return None;
            }
            // (what an assertion names for its message is not a use)
            if let TerminatorKind::Assert { cond, .. } = &term.kind {
                named.visit_operand(cond, Location { block: bb, statement_index: data.statements.len() });
            }
            bb = target;
            from = 0;
        }
        None
    }

    /// For an operand that is a 16-bit integer cast to 32 bits: the 16-bit
    /// operand and whether it is signed. The cast is in the block of the
    /// statement that is being compiled, in front of it, and its operand is
    /// a constant or a local that is not written in between (and that no
    /// reference is made to), so it has the same value here.
    fn widened(&mut self, o: &Operand<'tcx>) -> Option<(&'tcx Operand<'tcx>, bool)> {
        let l = match o {
            Operand::Copy(p) | Operand::Move(p) if p.projection.is_empty() => p.local,
            _ => return None,
        };
        let (bb, at) = self.cur?;
        let &[def] = &self.uses.def_locs[l.as_usize()][..] else { return None };
        if def.block != bb || def.statement_index >= at {
            return None;
        }
        let mir = self.mir;
        let stmts = &mir.basic_blocks[bb].statements;
        let StatementKind::Assign(b) = &stmts[def.statement_index].kind else { return None };
        let Rvalue::Cast(CastKind::IntToInt, src, _) = &b.1 else { return None };
        let from = self.monomorphize(src.ty(&mir.local_decls, self.tcx()));
        if self.layout(self.local_ty(l)).size.bytes() != 4 || !from.is_integral() || self.layout(from).size.bytes() != 2 {
            return None;
        }
        match src {
            Operand::Constant(_) => {}
            Operand::Copy(p) | Operand::Move(p) if p.projection.is_empty() => {
                if self.is_borrowed(p.local) {
                    return None;
                }
                for stmt in &stmts[def.statement_index + 1..at] {
                    match &stmt.kind {
                        StatementKind::Assign(b) if b.0.local == p.local => return None,
                        StatementKind::Assign(_)
                        | StatementKind::StorageLive(_)
                        | StatementKind::StorageDead(_)
                        | StatementKind::Nop => {}
                        _ => return None,
                    }
                }
            }
            _ => return None,
        }
        Some((src, Self::is_signed(from)))
    }

    /// Whether a reference or a raw pointer is made to the local anywhere
    /// in the function.
    fn is_borrowed(&mut self, l: Local) -> bool {
        if self.borrowed.is_none() {
            let mir = self.mir;
            let mut borrowed = vec![false; mir.local_decls.len()];
            for data in mir.basic_blocks.iter() {
                for stmt in &data.statements {
                    if let StatementKind::Assign(b) = &stmt.kind
                        && let Rvalue::Ref(_, _, place) | Rvalue::RawPtr(_, place) = &b.1
                    {
                        borrowed[place.local.as_usize()] = true;
                    }
                }
            }
            self.borrowed = Some(borrowed);
        }
        self.borrowed.as_ref().unwrap()[l.as_usize()]
    }

    /// The local an operand is, if it is a whole local; for a temporary
    /// that is only a copy of a local (and not stored: `lazy_locals`), that
    /// local.
    fn operand_local(&self, o: &Operand<'tcx>) -> Option<Local> {
        let mut l = match o {
            Operand::Copy(p) | Operand::Move(p) if p.projection.is_empty() => p.local,
            _ => return None,
        };
        for _ in 0..4 {
            let i = l.as_usize();
            let &[def] = &self.uses.def_locs[i][..] else { break };
            let Some(stmt) = self.mir.basic_blocks[def.block].statements.get(def.statement_index) else { break };
            match &stmt.kind {
                StatementKind::Assign(b) if self.lazy_cand[i] => match &b.1 {
                    Rvalue::Use(Operand::Copy(p) | Operand::Move(p), _) if p.projection.is_empty() => l = p.local,
                    _ => break,
                },
                _ => break,
            }
        }
        Some(l)
    }

    fn binop(
        &mut self,
        op: BinOp,
        x: &Operand<'tcx>,
        y: &Operand<'tcx>,
        dst: &Addr,
        dl: TyAndLayout<'tcx>,
    ) {
        // A 16-bit value widened for a 32-bit product with a constant that
        // fits 16 bits (`f[b] as u32 * 10000`): the same, where the
        // constant is not one for a few shifts and additions.
        if matches!(op, BinOp::Mul | BinOp::MulUnchecked) && std::env::var_os("RUST64_NO_MUL16K").is_none() {
            let found = match (self.widened(x), self.widened(y)) {
                (Some((s, signed)), None) => self.const_bits(y).map(|c| (s, signed, c)),
                (None, Some((s, signed))) => self.const_bits(x).map(|c| (s, signed, c)),
                _ => None,
            };
            if let Some((s, signed, c)) = found
                && c.count_ones() > 3
                && c < if signed { 0x8000 } else { 0x1_0000 }
            {
                let (va, _) = self.operand(s);
                let a = &mut self.p.asm;
                a.copy(&Addr::Abs("rt_a".into()), &va, 2);
                a.copy(&Addr::Abs("rt_b".into()), &Val::imm_u64(c as u64, 2), 2);
                a.ins(if signed { "jsr rt_mul16s" } else { "jsr rt_mul16u" });
                a.copy(dst, &Val::Mem(Addr::Abs("rt_r".into())), 4);
                return;
            }
        }
        // Two 16-bit values widened for a 32-bit product (fixed point):
        // the routine takes them as they are.
        if matches!(op, BinOp::Mul | BinOp::MulUnchecked)
            && let (Some((sx, signed)), Some((sy, signed_y))) = (self.widened(x), self.widened(y))
            && signed == signed_y
        {
            let (va, _) = self.operand(sx);
            let (vb, _) = self.operand(sy);
            let a = &mut self.p.asm;
            a.copy(&Addr::Abs("rt_a".into()), &va, 2);
            a.copy(&Addr::Abs("rt_b".into()), &vb, 2);
            a.ins(if signed { "jsr rt_mul16s" } else { "jsr rt_mul16u" });
            a.copy(dst, &Val::Mem(Addr::Abs("rt_r".into())), 4);
            return;
        }
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
            if op != Rem && n == 1 {
                Self::shift_byte(&mut self.p.asm, dst, &va, k, op != Div, false);
                return;
            }
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
                    && !(add && n == 2 && Self::in_frame(d))
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
                    && n == 1
                {
                    let left = matches!(op, Shl | ShlUnchecked);
                    Self::shift_byte(&mut self.p.asm, dst, &va, k & 7, left, signed);
                    return;
                }
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
                // A division gives the quotient and the remainder: if the
                // other one is asked for next, it is stored now.
                let other = match op {
                    Div | Rem if !inline => self.div_partner(op, x, y).map(|place| self.place(place).0),
                    _ => None,
                };
                let a = &mut self.p.asm;
                if !inline {
                    a.copy(&Addr::Abs("rt_a".into()), &va, n);
                    a.copy(&Addr::Abs("rt_b".into()), &vb, n);
                    // One, two and four bytes have their own
                    // multiplication and unsigned division.
                    let (routine, result) = match op {
                        Div if !signed && n == 1 => ("rt_udiv8", "rt_a"),
                        Rem if !signed && n == 1 => ("rt_udiv8", "rt_r"),
                        Div if !signed && n == 2 => ("rt_udiv16", "rt_a"),
                        Rem if !signed && n == 2 => ("rt_udiv16", "rt_r"),
                        Div if !signed && n == 4 => ("rt_udiv32", "rt_a"),
                        Rem if !signed && n == 4 => ("rt_udiv32", "rt_r"),
                        Div => (if signed { "rt_sdiv" } else { "rt_udiv" }, "rt_a"),
                        Rem => (if signed { "rt_sdiv" } else { "rt_udiv" }, "rt_r"),
                        _ if n == 1 => ("rt_mul8", "rt_r"),
                        _ if n == 2 => ("rt_mul16", "rt_r"),
                        _ if n == 4 => ("rt_mul32", "rt_r"),
                        _ => ("rt_mul", "rt_r"),
                    };
                    if matches!(routine, "rt_mul" | "rt_udiv" | "rt_sdiv") {
                        a.ins(format!("lda #{n}"));
                        a.ins("sta rt_n");
                    }
                    a.ins(format!("jsr {routine}"));
                    a.copy(dst, &Val::Mem(Addr::Abs(result.into())), n);
                    if let Some(d) = &other {
                        let rest = if result == "rt_a" { "rt_r" } else { "rt_a" };
                        a.copy(d, &Val::Mem(Addr::Abs(rest.into())), n);
                    }
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
                        && !Self::in_frame(d)
                    {
                        let d = d.clone();
                        Self::inc_dec(&mut self.p.asm, &d, 2, true);
                        return;
                    }
                    // (a known address: the assembler adds)
                    if let Some(e) = Self::imm_addr(&va) {
                        let v = Val::Imm(vec![format!("#<({e}+{k})"), format!("#>({e}+{k})")]);
                        self.a().copy(dst, &v, 2);
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

    /// Returns: a result of up to three bytes in A, X and Y.
    fn ret(&mut self) {
        let result = Local::from_usize(0);
        let size = self.layout(self.local_ty(result)).size.bytes();
        if (1..=3).contains(&size) {
            let at = self.local_addr(result);
            for (i, load) in ["lda", "ldx", "ldy"].iter().enumerate().take(size as usize) {
                self.a().ins(format!("{load} {at}+{i}"));
            }
        }
        self.a().ins("rts");
    }

    /// A call of function `id` with the argument values `args` (value and
    /// size, in the order of the callee's argument locals).
    ///
    /// The first three bytes of the arguments go into A, X and Y, the rest
    /// into the callee's argument locals. The registers are loaded last,
    /// from where the values are (a value behind a pointer is copied to a
    /// temporary first). The result comes back in A, X and Y if it has up
    /// to three bytes, else in the callee's result local.
    fn call_fn(&mut self, id: usize, args: &[(Val, u64)], dest: Option<Place<'tcx>>) {
        self.p.calls[self.id].push(id);
        // A result that is read where the function leaves it.
        if let Some(d) = dest
            && d.projection.is_empty()
            && self.res_cand.get(d.local.as_usize()) == Some(&true)
        {
            self.call_fn(id, args, None);
            self.p.calls[self.id].pop();
            self.res_at[d.local.as_usize()] = Some(format!("L{id}_0"));
            return;
        }
        // where the result goes: found before the call, as the registers
        // hold the result after it
        let dest = dest.map(|d| {
            let (a, l, _) = self.place(d);
            (a, l.size.bytes())
        });
        let mut regs: Vec<String> = Vec::new();
        let mut total = 0;
        for (i, (v, n)) in args.iter().enumerate() {
            let in_regs = (3 - regs.len() as u64).min(*n);
            let mut v = v.clone();
            if in_regs > 0 && v.byte(0).is_none() {
                let t = Addr::Abs(self.temp(*n));
                self.a().copy(&t, &v, *n);
                v = Val::Mem(t);
            }
            for j in 0..in_regs {
                regs.push(v.byte(j).unwrap());
            }
            if *n > in_regs {
                let slot = Addr::Abs(format!("L{id}_{}", i + 1)).offset(in_regs);
                self.a().copy(&slot, &v.offset(in_regs), n - in_regs);
            }
            total += n;
        }
        self.p.arg_bytes.insert(id, total);
        for (load, b) in ["lda", "ldx", "ldy"].iter().zip(&regs) {
            self.a().ins(format!("{load} {b}"));
        }
        self.a().ins(format!("jsr F{id}"));
        let Some((d, size)) = dest else { return };
        if size > 3 {
            self.a().copy(&d, &Val::Mem(Addr::Abs(format!("L{id}_0"))), size);
        } else if size > 0 {
            let (at, tmp) = self.abs_dst(&d, size);
            for (i, store) in ["sta", "stx", "sty"].iter().enumerate().take(size as usize) {
                self.a().ins(format!("{store} {at}+{i}"));
            }
            self.flush(&d, tmp, size);
        }
    }

    fn terminator(&mut self, kind: &TerminatorKind<'tcx>, next: Option<BasicBlock>) {
        match kind {
            TerminatorKind::Goto { target, .. } => self.goto(*target, next),
            TerminatorKind::FalseEdge { real_target, .. }
            | TerminatorKind::FalseUnwind { real_target, .. } => self.goto(*real_target, next),
            TerminatorKind::Return => self.ret(),
            TerminatorKind::Unreachable
            | TerminatorKind::UnwindResume
            | TerminatorKind::UnwindTerminate(_) => self.a().ins("jmp rt_panic"),
            TerminatorKind::SwitchInt { discr, targets } => {
                if let Some(c) = self.const_bits(discr) {
                    self.goto(targets.target_for_value(c), next);
                    return;
                }
                if self.lookup_switch(discr, targets, next) {
                    return;
                }
                let (v, l) = self.operand(discr);
                let n = l.size.bytes();
                self.a().setup_val(&v, 0);
                // "Anything else cannot be" (the match of an enum): the
                // last value needs no test, it is what is left.
                let other = &self.mir.basic_blocks[targets.otherwise()];
                let impossible = matches!(other.terminator().kind, TerminatorKind::Unreachable)
                    && other.statements.iter().all(|s| {
                        matches!(s.kind, StatementKind::StorageLive(_) | StatementKind::StorageDead(_) | StatementKind::Nop)
                    });
                let count = targets.iter().count();
                let (tested, rest) = match targets.iter().last() {
                    Some((_, last)) if impossible => (count - 1, last),
                    _ => (count, targets.otherwise()),
                };
                // The bytes of the value that are constants (the upper
                // byte of a tag that was extended): nothing to compare.
                let known: Vec<Option<u8>> = (0..n)
                    .map(|i| {
                        let b = v.byte(i)?;
                        let b = b.strip_prefix('#')?;
                        match b.strip_prefix('$') {
                            Some(h) => u8::from_str_radix(h, 16).ok(),
                            None => b.parse().ok(),
                        }
                    })
                    .collect();
                for (value, target) in targets.iter().take(tested) {
                    // (a value the bytes cannot have)
                    if (0..n).any(|i| known[i as usize].is_some_and(|k| k != (value >> (8 * i)) as u8)) {
                        continue;
                    }
                    let tl = self.block(target);
                    let a = &mut self.p.asm;
                    let lnext = a.new_label();
                    for i in 0..n {
                        if known[i as usize].is_some() {
                            continue;
                        }
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
                self.goto(rest, next);
            }
            TerminatorKind::Assert { target, msg, .. } if self.overflow_check_off(msg) => {
                self.goto(*target, next)
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
                    let t = self.temp(2);
                    self.a().addr_to(&a, &t);
                    self.call_fn(id, &[(Val::Mem(Addr::Abs(t)), 2)], None);
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
                self.ret();
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
        // The block starts in front of the copies of its operands: what is
        // in the registers there is not known to the block (the passes
        // over the assembly count on that), but a value that is copied to
        // an operand and loaded again is.
        let id = self.id;
        let n = self.a().new_label();
        let n = n.trim_start_matches("_L").to_owned();
        self.a().label(&format!("F{id}_i{n}"));
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
        let a = &mut self.p.asm;
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
        let unpassed = unpassed_args(self.p, inst);
        let mut vals = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            // (a reference to nothing that the function does not look at)
            if unpassed.get(i) == Some(&true) {
                vals.push((Val::Imm(Vec::new()), 0));
                continue;
            }
            let (v, l) = self.operand(arg);
            vals.push((v, l.size.bytes()));
        }
        self.call_fn(id, &vals, Some(dest));
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
                // A known size below 256: a loop in place.
                if let Some(c) = Self::imm_value(&count, cl.size.bytes().min(8)) {
                    let n = c.wrapping_mul(elem) & 0xffff;
                    if n == 0 {
                        return true;
                    }
                    if n < 256 {
                        let d = self.deref_val(&dst);
                        self.a().fill_block(&d, &val, n);
                        return true;
                    }
                }
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
                // Up to 128 bytes: a loop here, which counts the result up
                // when it runs to its end.
                if (1..=128).contains(&size) && std::env::var_os("RUST64_NO_EQ_LOOP").is_none() {
                    let side = |a: &mut Asm, x: &Addr, zp: &str| {
                        // (an address in brackets would read as a pointer)
                        let plain = match x {
                            Addr::Abs(e) if !e.contains('$') => {
                                let inner = e.strip_prefix('(').and_then(|e| e.strip_suffix(')'));
                                match inner {
                                    Some(i) if !i.contains(['(', ')']) => Some(i),
                                    None if !e.starts_with('(') => Some(e.as_str()),
                                    _ => None,
                                }
                            }
                            _ => None,
                        };
                        match plain {
                            Some(e) => format!("{e},y"),
                            None => {
                                a.addr_to(x, zp);
                                format!("({zp}),y")
                            }
                        }
                    };
                    let (x, y) = (side(a, &da, "zp_p0"), side(a, &db, "zp_p1"));
                    let (next, end) = (a.new_label(), a.new_label());
                    a.ins("ldx #0");
                    a.ins(format!("ldy #{}", size - 1));
                    a.label(&next);
                    a.ins(format!("lda {x}"));
                    a.ins(format!("cmp {y}"));
                    a.ins(format!("bne {end}"));
                    a.ins("dey");
                    a.ins(format!("bpl {next}"));
                    a.ins("inx");
                    a.label(&end);
                    if let Addr::Abs(to) = &d {
                        a.ins(format!("stx {to}"));
                    } else {
                        a.ins("stx rt_r");
                        a.copy(&d, &Val::Mem(Addr::Abs("rt_r".into())), 1);
                    }
                    return true;
                }
                a.addr_to(&da, "zp_p0");
                a.addr_to(&db, "zp_p1");
                a.copy(&Addr::Abs("rt_n".into()), &Val::imm_u64(size, 2), 2);
                a.ins("jsr rt_memcmp");
                // A is 0 for equal, else 1 or $ff: the carry of `cmp #1`
                // is the result turned around
                a.ins("cmp #1");
                a.ins("lda #0");
                a.ins("rol");
                a.ins("eor #1");
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
                // (a + n) - a
                let bytes = |o: &Operand<'tcx>| {
                    let ty = self.monomorphize(o.ty(&self.mir.local_decls, self.tcx()));
                    ty.builtin_deref(true).is_some_and(|t| self.layout(t).size.bytes() == 1)
                };
                if bytes(&args[0])
                    && let Some(n) = self.sum_diff(&args[0], &args[1])
                {
                    let (v, _) = self.operand(&Operand::Copy(Place::from(n)));
                    let (d, _, _) = self.place(dest);
                    self.a().copy(&d, &v, 2);
                    return true;
                }
                let (va, la) = self.operand(&args[0]);
                let (vb, _) = self.operand(&args[1]);
                let elem = self.layout(la.ty.builtin_deref(true).unwrap()).size.bytes();
                let signed = name.as_str() == "ptr_offset_from";
                // two addresses in the same object: a constant
                if let (Some(x), Some(y)) = (Self::imm_addr(&va), Self::imm_addr(&vb))
                    && let ((bx, ox), (by, oy)) = (Self::split_offset(&x), Self::split_offset(&y))
                    && bx == by
                    && !bx.starts_with('$')
                    && ox >= oy
                    && elem > 0
                    && (ox - oy) % elem == 0
                {
                    let (d, _, _) = self.place(dest);
                    self.a().copy(&d, &Val::imm_u64((ox - oy) / elem, 2), 2);
                    return true;
                }
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
            "integer_min" | "integer_max" => {
                let (va, l) = self.operand(&args[0]);
                let (vb, _) = self.operand(&args[1]);
                let n = l.size.bytes();
                let signed = Self::is_signed(l.ty);
                let (d, _, _) = self.place(dest);
                self.compare(&va, &vb, n, signed); // C clear: a < b
                let a = &mut self.p.asm;
                let (lb, lend) = (a.new_label(), a.new_label());
                a.ins(format!("{} {lb}", if name.as_str() == "integer_min" { "bcs" } else { "bcc" }));
                a.copy(&d, &va, n);
                a.ins(format!("jmp {lend}"));
                a.label(&lb);
                a.copy(&d, &vb, n);
                a.label(&lend);
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
                let (tv, tl) = self.operand(&args[0]);
                let mut vals = Vec::with_capacity(tl.fields.count());
                for i in 0..tl.fields.count() {
                    let off = tl.fields.offset(i).bytes();
                    vals.push((tv.offset(off), tl.field(&self.p.cx, i).size.bytes()));
                }
                self.call_fn(id, &vals, Some(dest));
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
