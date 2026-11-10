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

use rustc_abi::{HasDataLayout, Primitive, TagEncoding, TargetDataLayout, VariantIdx, Variants};
use rustc_data_structures::fx::FxIndexSet;
use rustc_hir::def_id::DefId;
use rustc_middle::mir::interpret::{AllocId, Allocation, GlobalAlloc, Scalar};
use rustc_middle::mono::MonoItem;
use rustc_middle::mir::{
    AggregateKind, BasicBlock, BinOp, Body, CastKind, ConstOperand, ConstValue, Local,
    NonDivergingIntrinsic, Operand, Place, ProjectionElem, Rvalue, StatementKind, TerminatorKind,
    UnOp,
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

/// The program being built: reachable functions, data, the call graph.
struct Program<'tcx> {
    tcx: TyCtxt<'tcx>,
    cx: Cx<'tcx>,
    funcs: FxIndexSet<Instance<'tcx>>,
    queue: VecDeque<usize>,
    allocs: FxIndexSet<AllocId>,
    statics: FxIndexSet<DefId>,
    /// Call graph: callees of each function (by index in `funcs`).
    calls: Vec<Vec<usize>>,
    asm: Asm,
    /// Frame symbol definitions, emitted after the `__bss` label.
    defs: String,
    bss: u64,
}

impl<'tcx> Program<'tcx> {
    fn func_id(&mut self, inst: Instance<'tcx>) -> usize {
        let (id, new) = self.funcs.insert_full(inst);
        if new {
            self.queue.push_back(id);
            self.calls.push(Vec::new());
        }
        id
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
            GlobalAlloc::Function { instance } => format!("F{}", self.func_id(instance)),
            GlobalAlloc::VTable(..) => {
                self.tcx.dcx().span_fatal(span, "rust64: trait objects (vtables) are not supported yet")
            }
            GlobalAlloc::TypeId { .. } => {
                self.tcx.dcx().span_fatal(span, "rust64: TypeId is not supported")
            }
        }
    }

    fn name(&self, id: usize) -> String {
        let inst = self.funcs[id];
        rustc_middle::ty::print::with_no_trimmed_paths!(format!("{inst}"))
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
        asm: Asm::default(),
        defs: String::new(),
        bss: 0,
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
            "rust64: no entry point; declare `#[unsafe(no_mangle)] pub extern \"C\" fn main()` \
             (or use `#[c64::main]`) in a `#![no_main]` crate",
        )
    };
    let main_id = p.func_id(main);

    while let Some(id) = p.queue.pop_front() {
        compile_fn(&mut p, id);
    }

    check_recursion(&p);

    // Data: constant allocations and statics. Emitting one can discover
    // more (pointers in data), so loop until both lists are done.
    let mut data = Asm::default();
    let (mut na, mut ns) = (0, 0);
    while na < p.allocs.len() || ns < p.statics.len() {
        while na < p.allocs.len() {
            let id = p.allocs[na];
            let alloc = tcx.global_alloc(id).unwrap_memory();
            emit_alloc(&mut p, &mut data, &format!("A{na}"), alloc.inner());
            na += 1;
        }
        while ns < p.statics.len() {
            let def_id = p.statics[ns];
            let name = rustc_middle::ty::print::with_no_trimmed_paths!(tcx.def_path_str(def_id));
            data.comment(format!("static {name}"));
            match tcx.eval_static_initializer(def_id) {
                Ok(alloc) => emit_alloc(&mut p, &mut data, &format!("S{ns}"), alloc.inner()),
                Err(_) => tcx.dcx().fatal(format!("rust64: cannot evaluate static {name}")),
            }
            ns += 1;
        }
    }

    let mut out = String::new();
    let _ = writeln!(out, "; generated by rust64\n.basicupstart rt_start\n");
    out.push_str(RUNTIME);
    let _ = writeln!(out, "\nrt_main = F{main_id}");
    let _ = writeln!(out, "rt_loc: .res 16                  ; dummy core::panic::Location\n");
    out.push_str(&p.asm.text);
    out.push_str("\n; ---- data ----\n");
    out.push_str(&data.text);
    out.push_str("\n; ---- bss (not in the file) ----\n__bss:\n");
    out.push_str(&p.defs);
    let _ = writeln!(
        out,
        "\n.assert __bss + {} <= $a000, \"program and its data do not fit below $a000\"",
        p.bss
    );
    out
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

/// Translation of one function.
struct FnCx<'a, 'tcx> {
    p: &'a mut Program<'tcx>,
    id: usize,
    inst: Instance<'tcx>,
    mir: &'tcx Body<'tcx>,
    /// Frame offset of each local.
    local_off: Vec<u64>,
    /// Start of the temporaries, current and maximum frame size.
    temp_base: u64,
    temp_cur: u64,
    frame_size: u64,
    span: Span,
    /// Locals known to hold a constant.
    consts: Vec<Option<u128>>,
}

fn compile_fn(p: &mut Program<'_>, id: usize) {
    let tcx = p.tcx;
    let inst = p.funcs[id];
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
        inst,
        mir,
        local_off: Vec::new(),
        temp_base: 0,
        temp_cur: 0,
        frame_size: 0,
        span: mir.span,
        consts: const_locals(tcx, inst, mir, 3),
    };
    f.layout_frame();
    f.body();
    f.finish();
}

impl<'a, 'tcx> FnCx<'a, 'tcx> {
    fn tcx(&self) -> TyCtxt<'tcx> {
        self.p.tcx
    }

    fn a(&mut self) -> &mut Asm {
        &mut self.p.asm
    }

    fn fatal(&self, msg: impl Into<String>) -> ! {
        self.tcx().dcx().span_fatal(self.span, format!("rust64: {}", msg.into()))
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

    fn local_addr(&self, l: Local) -> String {
        format!("{}+{}", self.frame(), self.local_off[l.as_usize()])
    }

    fn block(&self, bb: BasicBlock) -> String {
        format!("F{}_b{}", self.id, bb.as_usize())
    }

    fn layout_frame(&mut self) {
        let mut off = 0;
        for (l, _) in self.mir.local_decls.iter_enumerated() {
            self.local_off.push(off);
            off += self.layout(self.local_ty(l)).size.bytes();
        }
        self.temp_base = off;
        self.temp_cur = off;
        self.frame_size = off;
    }

    /// A temporary of `size` bytes, valid until the end of the statement.
    fn temp(&mut self, size: u64) -> String {
        let t = format!("{}+{}", self.frame(), self.temp_cur);
        self.temp_cur += size;
        self.frame_size = self.frame_size.max(self.temp_cur);
        t
    }

    fn finish(&mut self) {
        let id = self.id;
        let frame = self.frame();
        let mut defs = format!("{frame} = __bss + {}\n", self.p.bss);
        for l in 0..=self.mir.arg_count {
            let _ = writeln!(defs, "L{id}_{l} = {frame}+{}", self.local_off[l]);
        }
        self.p.bss += self.frame_size;
        self.p.defs.push_str(&defs);
    }

    fn body(&mut self) {
        let name = self.p.name(self.id);
        self.a().raw("");
        self.a().comment(format!("fn {name}"));
        let label = format!("F{}", self.id);
        self.a().label(&label);
        let n_blocks = self.mir.basic_blocks.len();
        let reachable = self.reachable_blocks();
        for (bb, data) in self.mir.basic_blocks.iter_enumerated() {
            if data.is_cleanup || !reachable[bb.as_usize()] {
                continue; // panic=abort: no unwinding; dead code
            }
            let l = self.block(bb);
            self.a().label(&l);
            for stmt in &data.statements {
                self.span = stmt.source_info.span;
                self.temp_cur = self.temp_base;
                self.statement(&stmt.kind);
            }
            let term = data.terminator();
            self.span = term.source_info.span;
            self.temp_cur = self.temp_base;
            let next = if bb.as_usize() + 1 < n_blocks { Some(BasicBlock::from_usize(bb.as_usize() + 1)) } else { None };
            self.terminator(&term.kind, next);
        }
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

    // ---- places and operands ----

    /// The location of a place, its layout and (behind a fat pointer) the
    /// pointer metadata.
    fn place(&mut self, place: Place<'tcx>) -> (Addr, TyAndLayout<'tcx>, Option<Val>) {
        let mut addr = Addr::Abs(self.local_addr(place.local));
        let mut layout = self.layout(self.local_ty(place.local));
        let mut meta = None;
        for elem in place.projection.iter() {
            match elem {
                ProjectionElem::Deref => {
                    let Some(pointee) = layout.ty.builtin_deref(true) else {
                        self.fatal(format!("deref of `{}`", layout.ty))
                    };
                    let psize = layout.size.bytes();
                    let ptr = match &addr {
                        Addr::Abs(e) => e.clone(),
                        Addr::Ind { .. } => {
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
                    let idx = Val::Mem(Addr::Abs(self.local_addr(l)));
                    let isize = self.layout(self.local_ty(l)).size.bytes();
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
            let a = &mut self.p.asm;
            a.copy(&Addr::Abs("rt_a".into()), v, vsize.min(2));
            if vsize < 2 {
                a.ins("lda #0");
                a.ins("sta rt_a+1");
            }
            a.copy(&Addr::Abs("rt_b".into()), &Val::imm_u64(k, 2), 2);
            a.ins("lda #2");
            a.ins("sta rt_n");
            a.ins("jsr rt_mul");
            a.copy(&tv, &Val::Mem(Addr::Abs("rt_r".into())), 2);
        }
        t
    }

    fn operand(&mut self, op: &Operand<'tcx>) -> (Val, TyAndLayout<'tcx>) {
        match op {
            Operand::Copy(p) | Operand::Move(p) => {
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
                    self.fatal(format!("unexpected pointer constant {b:?}"))
                }
            }
            Val::Mem(Addr::Abs(e)) => Addr::Ind { ptr: e.clone(), off: 0 },
            Val::Mem(a @ Addr::Ind { .. }) => {
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
                let (a, _, _) = self.place(*p);
                self.a().copy(&dst, &Val::Mem(a), size);
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
            Addr::Ind { .. } => {
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
                let id = self.p.func_id(inst);
                self.p.calls[self.id].push(id);
                self.a().copy(dst, &Val::Imm(Self::ptr_bytes(&format!("F{id}"), 0)), 2);
            }
            CastKind::PointerCoercion(PointerCoercion::ClosureFnPointer(_), _) => {
                let (_, sl) = self.operand(op);
                let ty::Closure(def_id, args) = *sl.ty.kind() else {
                    self.fatal("closure fn pointer from a non-closure")
                };
                let inst =
                    Instance::resolve_closure(self.tcx(), def_id, args, ty::ClosureKind::FnOnce);
                let id = self.p.func_id(inst);
                self.p.calls[self.id].push(id);
                self.a().copy(dst, &Val::Imm(Self::ptr_bytes(&format!("F{id}"), 0)), 2);
            }
            CastKind::PointerCoercion(PointerCoercion::Unsize, _) => {
                let (v, sl) = self.operand(op);
                self.a().copy(dst, &v, 2);
                if dsize > 2 {
                    let src_pointee = sl.ty.builtin_deref(true).unwrap();
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
        let (va, la) = self.operand(x);
        let (vb, lb) = self.operand(y);
        let n = la.size.bytes();
        let signed = Self::is_signed(la.ty);
        use BinOp::*;
        match op {
            Add | AddUnchecked | AddWithOverflow | Sub | SubUnchecked | SubWithOverflow => {
                let add = matches!(op, Add | AddUnchecked | AddWithOverflow);
                let a = &mut self.p.asm;
                a.setup_val(&va, 0);
                a.setup_val(&vb, 1);
                a.setup(dst, 2);
                a.ins(if add { "clc" } else { "sec" });
                for i in 0..n {
                    a.lda(&va, i, 0);
                    a.op_byte(if add { "adc" } else { "sbc" }, &vb, i, 1);
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
                    a.lda(&va, i, 0);
                    a.op_byte(ins, &vb, i, 1);
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
                let t = self.temp(n);
                let ta = Addr::Abs(t.clone());
                self.a().copy(&ta, &va, n);
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
                let a = &mut self.p.asm;
                a.copy(&Addr::Abs("rt_a".into()), &va, n);
                a.copy(&Addr::Abs("rt_b".into()), &vb, n);
                a.ins(format!("lda #{n}"));
                a.ins("sta rt_n");
                let (routine, result) = match op {
                    Div => (if signed { "rt_sdiv" } else { "rt_udiv" }, "rt_a"),
                    Rem => (if signed { "rt_sdiv" } else { "rt_udiv" }, "rt_r"),
                    _ => ("rt_mul", "rt_r"),
                };
                a.ins(format!("jsr {routine}"));
                a.copy(dst, &Val::Mem(Addr::Abs(result.into())), n);
                if op == MulWithOverflow {
                    // TODO: detect overflow (needs the full-width product).
                    let fo = dl.fields.offset(1).bytes();
                    a.copy(&dst.offset(fo), &Val::imm_bytes(&[0]), 1);
                }
            }
            Offset => {
                let pointee = la.ty.builtin_deref(true).unwrap();
                let elem = self.layout(pointee).size.bytes();
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
        let a = &mut self.p.asm;
        a.setup_val(p, 0);
        a.setup_val(q, 1);
        a.ins("sec");
        for i in 0..n {
            if signed && i == n - 1 {
                a.lda(q, i, 1);
                a.ins("eor #$80");
                a.ins("sta rt_t");
                a.lda(p, i, 0);
                a.ins("eor #$80");
                a.ins("sbc rt_t");
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
                        a.ins(format!("cmp #${:02x}", (value >> (8 * i)) as u8));
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
                    let id = self.p.func_id(inst);
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
            other => self.fatal(format!("{other:?} is not supported")),
        }
    }

    fn call(&mut self, func: &Operand<'tcx>, args: &[Operand<'tcx>], dest: Place<'tcx>) {
        let tcx = self.tcx();
        let fty = self.monomorphize(func.ty(self.mir, tcx));
        let ty::FnDef(def_id, fargs) = *fty.kind() else {
            self.fatal("calls through function pointers are not supported yet")
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
            _ => {}
        }
        let id = self.p.func_id(inst);
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

    /// Translates an intrinsic call; false: use its fallback body.
    fn intrinsic(&mut self, inst: Instance<'tcx>, args: &[Operand<'tcx>], dest: Place<'tcx>) -> bool {
        let tcx = self.tcx();
        let name = tcx.item_name(inst.def_id());
        match name.as_str() {
            "volatile_load" | "unaligned_volatile_load" | "read_via_copy" => {
                let (p, _) = self.operand(&args[0]);
                let src = self.deref_val(&p);
                let (d, dl, _) = self.place(dest);
                self.a().copy(&d, &Val::Mem(src), dl.size.bytes());
            }
            "volatile_store" | "unaligned_volatile_store" | "nontemporal_store"
            | "write_via_move" => {
                let (p, _) = self.operand(&args[0]);
                let (v, vl) = self.operand(&args[1]);
                let dst = self.deref_val(&p);
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
                let id = self.p.func_id(callee);
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
