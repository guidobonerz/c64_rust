//! Data-flow optimization of the assembly of one function.
//!
//! The code generator keeps every value in memory and emits each statement
//! on its own; this pass looks at the function as a whole. It builds the
//! control-flow graph of the function's assembly text and runs two analyses
//! over it until nothing changes:
//!
//! * **Forward: what is known about the values.** For A, X, Y and every
//!   byte of the function's own frame that is only used as a plain operand
//!   ("tracked"; nothing else can change such a byte): a value number
//!   (locations with the same number hold the same value; constants have
//!   numbers of their own) and the range the value is in, plus what is
//!   known about the carry, zero and negative flags. A conditional branch
//!   narrows the ranges on its two edges (`cmp #25` / `bcc`: below 25 where
//!   the branch is taken). With that,
//!   - a read of a byte that holds a constant becomes an immediate, a read
//!     of a copy reads the original,
//!   - a load of a value another register holds becomes a transfer
//!     (`sta t` / `ldx t` -> `tax`), a load of the value the register
//!     already holds and a store of the value the byte already holds go,
//!   - a branch whose flag is known goes or becomes unconditional (bounds
//!     checks of masked indexes and of loop counters), and code that cannot
//!     be reached any more is removed.
//! * **Backward: what is still needed.** Liveness of the registers, the
//!   flags and the tracked bytes: instructions without side effects whose
//!   results nobody reads are removed (stores to dead frame bytes, loads,
//!   comparisons of folded branches). Two bytes that are never needed at
//!   the same time and are connected by a copy become one byte, and the
//!   copy goes (`Opt::merge_copies`).
//!
//! The scratch pointers of the code generator (`zp_p0` to `zp_p2`) are
//! tracked like frame bytes: a call may read and change them, and nothing
//! relies on them after the function. With them, an access through a
//! pointer that was computed as `address + index` or `pointer + index`
//! becomes what a 6502 programmer writes (`Opt::plan`):
//!
//! ```text
//!         clc                     ldx i
//!         lda #<table             lda table,x
//!         adc i
//!         sta p                   and for a pointer q in the frame:
//!         lda #>table
//!         adc #0                  ldy i
//!         sta p+1                 lda (q),y
//!         ...
//!         lda (p),y
//! ```
//!
//! When all that is done, a frame byte that a loop only loads, stores and
//! steps (a counter) moves into X or Y for the time of the loop, if the
//! loop leaves that register alone (`Opt::promote`, `Opt::sink`), the
//! step of an iterator moves to the end of its loop and is done in place
//! (`Opt::sink_add`, `Opt::in_place`), a computation that is the same in
//! every round moves in front of the loop (`Opt::hoist`, and for a bounds
//! check `Opt::hoist_check`), the 16-bit test of a loop over a slice of
//! known length becomes an 8-bit one (`Opt::narrow_test`) or the loop gets
//! an index in Y in place of the pointer (`Opt::index_loop`), and the test of
//! a loop moves to its end (`Opt::rotate`).
//!
//! Between the functions of the program the first three bytes of the
//! arguments are in A, X and Y, and so is a result of up to three bytes
//! (`FnCx::call_fn`); the flags carry nothing. At the end of a function
//! only its result is live. (See `FnCx::inline_asm` for the contract of
//! `asm!` blocks.)
//!
//! The pass gives up on a function it does not fully understand: unknown
//! mnemonics, directives between the instructions, decimal mode, labels
//! from elsewhere, a subroutine inside the function, code that modifies
//! itself or takes the address of one of its labels.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use crate::asm::{FrameLayout, sym_off};

const RA: u8 = 1;
const RX: u8 = 2;
const RY: u8 = 4;
const FN: u8 = 8;
const FZ: u8 = 16;
const FC: u8 = 32;
const FV: u8 = 64;
const NZ: u8 = FN | FZ;
const FLAGS: u8 = FN | FZ | FC | FV;
const ALL: u8 = 0x7f;

/// Value numbers: constants are their value, then come the symbolic
/// immediates (`#<(label)`), the values at the function's entry, the
/// values instructions produce and the values that differ between the
/// edges into a block.
const SYM: u64 = 256;
const ENTRY: u64 = 1 << 20;
const FRESH: u64 = 1 << 24;
const PHI: u64 = 1 << 40;

pub const MNEMONICS: [&str; 54] = [
    "lda", "ldx", "ldy", "sta", "stx", "sty", "tax", "tay", "txa", "tya", "tsx", "txs", "adc",
    "sbc", "and", "ora", "eor", "cmp", "cpx", "cpy", "bit", "asl", "lsr", "rol", "ror", "inc",
    "dec", "inx", "dex", "iny", "dey", "clc", "sec", "clv", "cld", "cli", "sei", "pha", "pla",
    "php", "plp", "nop", "jmp", "jsr", "rts", "rti", "bcc", "bcs", "beq", "bne", "bpl", "bmi",
    "bvc", "bvs",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// No operand, or the accumulator.
    Imp,
    Imm,
    Mem,
    MemX,
    MemY,
    IndY,
    IndX,
    Ind,
}

#[derive(Clone, PartialEq, Debug)]
struct Loc {
    vn: u64,
    lo: u8,
    hi: u8,
}

fn full(vn: u64) -> Loc {
    Loc { vn, lo: 0, hi: 255 }
}

fn konst(v: u8) -> Loc {
    Loc { vn: v as u64, lo: v, hi: v }
}

/// A value in `lo..=hi`: a constant if that is one value.
fn ranged(vn: u64, lo: u8, hi: u8) -> Loc {
    if lo == hi { konst(lo) } else { Loc { vn, lo, hi } }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tri {
    F,
    T,
    U,
}

fn tri(b: bool) -> Tri {
    if b { Tri::T } else { Tri::F }
}

/// The two sides of the comparison a flag comes from.
#[derive(Clone, PartialEq, Debug)]
struct Cmp {
    l: Loc,
    r: Loc,
}

/// The carry that went into an addition.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Cin {
    Zero,
    One,
    /// The carry out of the addition that made this value.
    Of(u64),
}

/// How a value was made: `x + y + carry`, and whether that carried.
#[derive(Clone, Copy)]
struct AddDef {
    x: u64,
    y: u64,
    cin: Cin,
    cout: Tri,
}

/// Where the index of an access is.
#[derive(Clone, PartialEq)]
enum Src {
    A,
    Cell(usize),
}

/// A better way to make an access through a scratch pointer.
#[derive(Clone)]
enum Plan {
    /// `operand` (`address,x`) with the index in X.
    Abs { operand: String, idx: Src },
    /// Through the pointer at frame bytes `cell` and `cell + 1` (copied to
    /// the scratch pointer `zp`) with the index in Y.
    Ptr { cell: usize, zp: usize, idx: Src },
    /// `operand`: the pointer and Y are known, so the address is.
    Direct { operand: String },
}

#[derive(Clone, PartialEq, Debug)]
enum ZSrc {
    None,
    /// N and Z are those of the value with this number.
    Val(u64),
    /// Z: the two sides are equal.
    Cmp(Cmp),
}

/// What is known at one point: `l[0..3]` are A, X, Y, `l[3 + c]` is frame
/// byte `c`.
#[derive(Clone, PartialEq)]
struct St {
    l: Vec<Loc>,
    c: Tri,
    z: Tri,
    n: Tri,
    /// C: left >= right.
    csrc: Option<Cmp>,
    zsrc: ZSrc,
    /// C: the carry out of the addition that made this value.
    cadd: Option<u64>,
}

struct Ins {
    op: &'static str,
    arg: String,
    mode: Mode,
    /// The tracked frame byte a plain operand names.
    cell: Option<usize>,
    /// The value of an immediate operand.
    ival: Option<Loc>,
    gone: bool,
    /// The instruction does not change the value of its register: it can
    /// go when these flags are not needed after it.
    redundant: Option<u8>,
    /// A conditional branch that is always taken.
    always: bool,
    /// A comparison whose carry is known.
    carry: Option<bool>,
    /// The scratch pointer of an `(zp_pN),y` operand (its low byte).
    ptr: Option<usize>,
    plan: Option<Plan>,
    /// The instruction is from an `asm!` block.
    in_asm: bool,
    /// `adc #1` or `sbc #1` of a value that this index register holds too
    /// (`RX` or `RY`), and whether it leaves the carry as it was.
    step_reg: Option<(u8, bool)>,
    /// A branch at the end of a block that is entered from the block in
    /// front of it: whether it is taken when the block is entered that way.
    first: Option<bool>,
    /// An `adc` of a constant: the frame bytes that hold the same value as
    /// A in front of it, and whether the carry is known to be clear there.
    add_same: Vec<usize>,
    add_clear: bool,
    /// For an access `(zp_pN),y`: the frame bytes that hold the same
    /// pointer as the scratch pointer (their low bytes), and Y if it is
    /// known (see `forward`; for `index_loop`).
    via: Vec<usize>,
    via_y: Option<u8>,
}

enum Line {
    Label(String),
    Ins(Ins),
    Other(String),
}

#[derive(Clone, Copy, PartialEq)]
enum Tgt {
    Block(usize),
    External,
}

#[derive(Clone, Copy)]
enum Term {
    Fall(Tgt),
    Cond(Tgt, Tgt),
    Jmp(Tgt),
    Ret,
}

struct Block {
    start: usize,
    end: usize,
    term: Term,
}

struct Live {
    regs: u8,
    cells: Vec<bool>,
}

struct Opt<'a> {
    /// Frame bytes with a known value at the entry of the function.
    entry_consts: Vec<(usize, Loc)>,
    /// The stores to argument slots of other functions that the last
    /// `forward` saw: name, offset, and the value if it is a constant.
    stores: Vec<(String, u64, Option<u8>)>,
    id: usize,
    layout: &'a FrameLayout,
    lines: Vec<Line>,
    /// The bytes of the frame; the tracked bytes are these and the six
    /// bytes of the scratch pointers after them.
    nframe: usize,
    ncells: usize,
    escaped: Vec<bool>,
    /// The additions of the function as it is now (see `forward`).
    adds: HashMap<u64, AddDef>,
    /// `adds` is complete (after the rewrite sweep of `forward`).
    adds_valid: bool,
    /// What is known where a block is entered from the one in front of it,
    /// by the line the block starts at (see `forward`; for `narrow_test`).
    heads: HashMap<usize, St>,
    /// Liveness as if no redundant load were removed (see `backward`).
    keep_redundant: bool,
    /// The first rounds (see `forward`).
    early: bool,
    /// The result: live at the exits.
    ret: usize,
    /// The result and the arguments: other functions use these bytes.
    pinned: usize,
    syms: Vec<String>,
    sym_ids: HashMap<String, u64>,
    is_io: &'a dyn Fn(&str) -> bool,
    call_regs: &'a dyn Fn(&str) -> Option<u8>,
    is_const: &'a dyn Fn(&str) -> bool,
}

fn is_branch(op: &str) -> bool {
    matches!(op, "bcc" | "bcs" | "beq" | "bne" | "bpl" | "bmi" | "bvc" | "bvs")
}

fn parse_mode(arg: &str) -> Mode {
    let lower = arg.to_ascii_lowercase();
    if arg.is_empty() || lower == "a" {
        Mode::Imp
    } else if arg.starts_with('#') {
        Mode::Imm
    } else if arg.starts_with('(') {
        let compact: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.ends_with("),y") {
            Mode::IndY
        } else if compact.ends_with(",x)") {
            Mode::IndX
        } else {
            Mode::Ind
        }
    } else if let Some((_, idx)) = lower.rsplit_once(',') {
        if idx.trim() == "x" { Mode::MemX } else { Mode::MemY }
    } else {
        Mode::Mem
    }
}

/// Optimizes the text of function `id`. `layout` names the bytes of its
/// frame; `is_io` tells whether a symbol may be a hardware register (a read
/// of it is never removed); `is_const` whether a symbol is data that the
/// program never changes; `call_args` gives the number of argument bytes
/// that a routine the code generator calls takes in A, X and Y (`None`: not
/// known, all of them), and `call_reads` which of them it reads at all (a
/// mask as `entry` gets it; `None`: not known).
///
/// `entry` gets the registers that the function reads before it writes
/// them (bit 0: A, 1: X, 2: Y), if the function could be followed: what a
/// caller has to load.
/// A known address plus a constant, which the assembler can add:
///
/// ```text
///     clc                      lda #<(X+4)
///     lda #<(X)                sta p
///     adc #$04                 lda #>(X+4)
///     sta p                    sta p+1
///     lda #>(X)                clc
///     adc #$00
///     sta p+1
/// ```
///
/// (the end of an array for an iterator over it). The carry is clear after
/// both: an address does not go past the end of the memory.
fn fold_addr(text: &str) -> String {
    if std::env::var_os("RUST64_NO_FOLD").is_some() {
        return text.to_owned();
    }
    let lines: Vec<&str> = text.lines().collect();
    let ins = |i: usize| -> Option<(&str, &str)> {
        let l = lines.get(i)?;
        if !l.starts_with([' ', '\t']) {
            return None;
        }
        let t = l.split(';').next().unwrap_or("").trim();
        Some(t.split_once(' ').map_or((t, ""), |(op, arg)| (op, arg.trim())))
    };
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < lines.len() {
        if let (Some(("clc", "")), Some(("lda", lo)), Some(("adc", k)), Some(("sta", d0))) =
            (ins(i), ins(i + 1), ins(i + 2), ins(i + 3))
            && let (Some(("lda", hi)), Some(("adc", zero)), Some(("sta", d1))) = (ins(i + 4), ins(i + 5), ins(i + 6))
            && let Some(e) = lo.strip_prefix("#<(").and_then(|e| e.strip_suffix(')'))
            && hi.strip_prefix("#>(").and_then(|e| e.strip_suffix(')')) == Some(e)
            && let Some(k) = k.strip_prefix("#$").and_then(|k| u8::from_str_radix(k, 16).ok())
            && matches!(zero, "#$00" | "#0")
            && !d0.starts_with(['#', '('])
            && !d0.contains(',')
            && (d1.strip_suffix("+1") == Some(d0) || d1.strip_suffix("+0+1") == d0.strip_suffix("+0"))
        {
            let pad = &lines[i][..lines[i].len() - lines[i].trim_start().len()];
            for l in [
                format!("lda #<({e}+{k})"),
                format!("sta {d0}"),
                format!("lda #>({e}+{k})"),
                format!("sta {d1}"),
                "clc".to_owned(),
            ] {
                out.push_str(pad);
                out.push_str(&l);
                out.push('\n');
            }
            i += 7;
            continue;
        }
        out.push_str(lines[i]);
        out.push('\n');
        i += 1;
    }
    out
}

#[allow(clippy::too_many_arguments)]
pub fn optimize(
    text: &str,
    id: usize,
    layout: &FrameLayout,
    is_io: &dyn Fn(&str) -> bool,
    is_const: &dyn Fn(&str) -> bool,
    call_args: &dyn Fn(&str) -> Option<u64>,
    call_reads: &dyn Fn(&str) -> Option<u8>,
    entry: &mut Option<u8>,
    entry_consts: &[(usize, u8)],
    stores: &mut Vec<(String, u64, Option<u8>)>,
) -> String {
    stores.clear();
    let call_regs = |sym: &str| {
        call_args(sym).map(|n| {
            let taken = match n {
                0 => 0,
                1 => RA,
                2 => RA | RX,
                _ => RA | RX | RY,
            };
            taken & call_reads(sym).unwrap_or(RA | RX | RY)
        })
    };
    *entry = None;
    let text = &fold_addr(text);
    let nframe = layout.values().map(|&(o, n)| o + n).max().unwrap_or(0) as usize;
    let ncells = nframe + 2 * crate::asm::ZP.len();
    let ret = layout.get(&format!("L{id}_0")).map_or(0, |&(o, n)| o + n) as usize;
    let args = format!("L{id}_");
    let pinned =
        layout.iter().filter(|(n, _)| n.starts_with(&args)).map(|(_, &(o, n))| o + n).max().unwrap_or(0) as usize;
    let mut o = Opt {
        id,
        layout,
        lines: Vec::new(),
        nframe,
        ncells,
        escaped: vec![false; ncells],
        adds: HashMap::new(),
        adds_valid: false,
        heads: HashMap::new(),
        keep_redundant: false,
        early: true,
        ret,
        pinned,
        syms: Vec::new(),
        sym_ids: HashMap::new(),
        is_io,
        call_regs: &call_regs,
        is_const,
        entry_consts: Vec::new(),
        stores: Vec::new(),
    };
    let trace = std::env::var_os("RUST64_OPT_TRACE").is_some();
    if !o.parse(text) {
        if trace {
            eprintln!("opt F{id}: not parsed");
        }
        return text.to_owned();
    }
    if trace {
        eprintln!("opt F{id}: {} lines, {} cells, escaped {}", o.lines.len(), o.ncells, o.escaped.iter().filter(|e| **e).count());
    }
    for &(c, v) in entry_consts {
        if c < o.nframe && !o.escaped[c] {
            let v = o.imm(&format!("#${v:02x}"));
            o.entry_consts.push((c, v));
        }
    }
    for round in 0..400 {
        o.early = round < 2;
        let Some(blocks) = o.blocks() else { return text.to_owned() };
        let mut changed = o.forward(&blocks);
        // The branches may have changed: the graph again.
        let Some(blocks) = o.blocks() else { return text.to_owned() };
        changed |= o.backward(&blocks);
        // (the last steps only come after the first rounds)
        if !changed && !o.early {
            break;
        }
    }
    if let Some(blocks) = o.blocks()
        && !blocks.is_empty()
    {
        let live_in = o.liveness(&blocks);
        *entry = Some(live_in[0].regs & (RA | RX | RY));
    }
    *stores = std::mem::take(&mut o.stores);
    o.emit()
}

impl Opt<'_> {
    /// The frame byte a plain operand names.
    fn own(&self, arg: &str) -> Option<usize> {
        let (base, off) = sym_off(arg)?;
        if let Some(&(o, _)) = self.layout.get(base) {
            return Some((o + off) as usize);
        }
        let p = crate::asm::ZP.iter().position(|z| *z == base)?;
        (off < 2).then_some(self.nframe + 2 * p + off as usize)
    }

    /// The scratch pointer of an operand `(zp_pN),y`: its low byte.
    fn scratch_ptr(&self, arg: &str) -> Option<usize> {
        let compact: String = arg.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
        let name = compact.strip_prefix('(')?.strip_suffix("),y")?;
        let p = crate::asm::ZP.iter().position(|z| *z == name)?;
        Some(self.nframe + 2 * p)
    }

    fn make_ins(&self, op: &'static str, arg: String) -> Ins {
        let mode = parse_mode(&arg);
        let cell = if mode == Mode::Mem { self.own(&arg).filter(|&c| !self.escaped[c]) } else { None };
        let ptr = if mode == Mode::IndY { self.scratch_ptr(&arg).filter(|&c| !self.escaped[c]) } else { None };
        Ins {
            op,
            arg,
            mode,
            cell,
            ival: None,
            gone: false,
            redundant: None,
            always: false,
            carry: None,
            ptr,
            plan: None,
            in_asm: false,
            step_reg: None,
            first: None,
            add_same: Vec::new(),
            add_clear: false,
            via: Vec::new(),
            via_y: None,
        }
    }

    /// The label at the start (`i`) or the end (`e`) of an `asm!` block.
    fn asm_label(&self, name: &str, kind: u8) -> bool {
        name.strip_prefix(&format!("F{}_", self.id)).is_some_and(|r| {
            r.len() > 1 && r.as_bytes()[0] == kind && r[1..].bytes().all(|b| b.is_ascii_digit())
        })
    }

    /// Line `i` is the start of an `asm!` block: the registers and flags
    /// are undefined there, so nothing before it is needed for them.
    fn asm_entry(&self, i: usize) -> bool {
        matches!(&self.lines[i], Line::Label(n) if self.asm_label(n, b'i'))
    }

    fn imm(&mut self, arg: &str) -> Loc {
        let t = arg[1..].trim();
        let v = if let Some(h) = t.strip_prefix('$') {
            u64::from_str_radix(h, 16).ok()
        } else if let Some(b) = t.strip_prefix('%') {
            u64::from_str_radix(b, 2).ok()
        } else {
            t.parse::<u64>().ok()
        };
        if let Some(v) = v
            && v < 256
        {
            return konst(v as u8);
        }
        let key: String = arg.chars().filter(|c| !c.is_whitespace()).collect();
        let n = self.syms.len() as u64;
        let id = *self.sym_ids.entry(key.clone()).or_insert_with(|| {
            self.syms.push(key);
            SYM + n
        });
        full(id)
    }

    /// The operand text of an immediate with value number `vn`.
    fn imm_text(&self, vn: u64) -> Option<String> {
        if vn < 256 {
            Some(format!("#${vn:02x}"))
        } else if vn < ENTRY {
            Some(self.syms[(vn - SYM) as usize].clone())
        } else {
            None
        }
    }

    /// A label of this function that the code generator made (or a local
    /// `@label` of an `asm!` block).
    fn local_label(&self, name: &str) -> bool {
        if name.starts_with('@') {
            return true;
        }
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        if let Some(r) = name.strip_prefix("_L").or_else(|| name.strip_prefix("_C")) {
            return digits(r);
        }
        name.strip_prefix(&format!("F{}_", self.id))
            .is_some_and(|r| r.len() > 1 && matches!(r.as_bytes()[0], b'b' | b'i' | b'e') && digits(&r[1..]))
    }

    fn parse(&mut self, text: &str) -> bool {
        let entry = format!("F{}", self.id);
        let mut seen_entry = false;
        let mut raw: Vec<(Option<String>, Option<(String, String)>, Option<String>)> = Vec::new();
        for l in text.lines() {
            let t = l.trim();
            if t.is_empty() || t.starts_with(';') {
                raw.push((None, None, Some(l.to_owned())));
                continue;
            }
            let code = t.split(';').next().unwrap_or("").trim();
            let mut rest = code;
            let mut label = None;
            if !l.starts_with(' ') && !l.starts_with('\t') {
                // `label:` or `label: instruction`
                let Some((name, r)) = code.split_once(':') else { return false };
                let name = name.trim();
                if name.is_empty() || name.contains(char::is_whitespace) {
                    return false;
                }
                label = Some(name.to_owned());
                rest = r.trim();
            }
            let ins = if rest.is_empty() {
                None
            } else {
                let (op, arg) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                Some((op.to_ascii_lowercase(), arg.trim().to_owned()))
            };
            raw.push((label, ins, None));
        }
        // Escaped bytes: of names used in anything but a plain operand (a
        // scratch pointer also in `(zp_pN),y`), and the scratch pointers
        // if the code names their addresses as numbers.
        for (_, ins, _) in &raw {
            let Some((_, arg)) = ins else { continue };
            if self.own(arg).is_some() || self.scratch_ptr(arg).is_some() {
                continue;
            }
            for w in arg.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$')) {
                if let Some(&(o, n)) = self.layout.get(w) {
                    for c in o..o + n {
                        self.escaped[c as usize] = true;
                    }
                }
                let numbered = !arg.starts_with('#')
                    && w
                    .strip_prefix('$')
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .is_some_and(|a| (0xf9..=0xfe).contains(&a));
                if numbered || crate::asm::ZP.contains(&w) {
                    for c in self.nframe..self.ncells {
                        self.escaped[c] = true;
                    }
                }
            }
        }
        let mut in_asm = false;
        for (label, ins, other) in raw {
            if let Some(o) = other {
                self.lines.push(Line::Other(o));
                continue;
            }
            if let Some(name) = label {
                if name == entry && !seen_entry {
                    seen_entry = true;
                } else if !self.local_label(&name) {
                    return false;
                }
                if self.asm_label(&name, b'i') {
                    in_asm = true;
                } else if self.asm_label(&name, b'e') {
                    in_asm = false;
                }
                self.lines.push(Line::Label(name));
            }
            if let Some((op, arg)) = ins {
                let Some(&op) = MNEMONICS.iter().find(|m| **m == op) else { return false };
                let mode = parse_mode(&arg);
                let cell = if mode == Mode::Mem {
                    self.own(&arg).filter(|&c| !self.escaped[c])
                } else {
                    None
                };
                let ival = (mode == Mode::Imm).then(|| self.imm(&arg));
                let ptr = if mode == Mode::IndY {
                    self.scratch_ptr(&arg).filter(|&c| !self.escaped[c])
                } else {
                    None
                };
                self.lines.push(Line::Ins(Ins {
                    op,
                    arg,
                    mode,
                    cell,
                    ival,
                    gone: false,
                    redundant: None,
                    always: false,
                    carry: None,
                    ptr,
                    plan: None,
                    in_asm,
                    step_reg: None,
                    first: None,
                    add_same: Vec::new(),
                    add_clear: false,
                    via: Vec::new(),
                    via_y: None,
                }));
            }
        }
        if !seen_entry {
            return false;
        }
        // Labels of this function may only be the targets of branches and
        // jumps: no subroutines, no addresses of code, no modified code.
        let labels: HashSet<&str> = self
            .lines
            .iter()
            .filter_map(|l| if let Line::Label(n) = l { Some(n.trim_start_matches('@')) } else { None })
            .collect();
        for l in &self.lines {
            let Line::Ins(i) = l else { continue };
            if is_branch(i.op) || (i.op == "jmp" && i.mode == Mode::Mem) {
                continue;
            }
            if i.arg.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).any(|w| labels.contains(w)) {
                return false;
            }
        }
        true
    }

    /// The key of a label: `@labels` are local to the code between two
    /// other labels.
    fn label_keys(&self) -> (Vec<usize>, HashMap<String, usize>) {
        let mut scope = vec![0; self.lines.len()];
        let mut cur = 0;
        let mut keys = HashMap::new();
        for (i, l) in self.lines.iter().enumerate() {
            if let Line::Label(n) = l {
                if n.starts_with('@') {
                    keys.insert(format!("{cur}{n}"), i);
                } else {
                    cur += 1;
                    keys.insert(n.clone(), i);
                }
            }
            scope[i] = cur;
        }
        (scope, keys)
    }

    fn key(scope: &[usize], i: usize, arg: &str) -> String {
        if arg.starts_with('@') { format!("{}{arg}", scope[i]) } else { arg.to_owned() }
    }

    /// The basic blocks. `None`: something the pass does not understand.
    fn blocks(&self) -> Option<Vec<Block>> {
        let (scope, keys) = self.label_keys();
        // lines of the labels something jumps to
        let mut targets: HashSet<usize> = HashSet::new();
        for (i, l) in self.lines.iter().enumerate() {
            if let Line::Ins(ins) = l
                && !ins.gone
                && (is_branch(ins.op) || (ins.op == "jmp" && ins.mode == Mode::Mem))
                && let Some(&t) = keys.get(&Self::key(&scope, i, &ins.arg))
            {
                targets.insert(t);
            }
        }
        let mut starts = vec![0usize];
        for (i, l) in self.lines.iter().enumerate() {
            match l {
                Line::Label(_) if targets.contains(&i) => starts.push(i),
                Line::Ins(ins) if !ins.gone && (is_branch(ins.op) || matches!(ins.op, "jmp" | "rts" | "rti")) => {
                    starts.push(i + 1)
                }
                _ => {}
            }
        }
        starts.sort_unstable();
        starts.dedup();
        starts.retain(|&s| s < self.lines.len());
        let mut block_of = vec![0usize; self.lines.len() + 1];
        for (b, w) in starts.iter().enumerate() {
            let end = starts.get(b + 1).copied().unwrap_or(self.lines.len());
            for s in &mut block_of[*w..end] {
                *s = b;
            }
        }
        let n = starts.len();
        let mut blocks = Vec::with_capacity(n);
        for b in 0..n {
            let start = starts[b];
            let end = starts.get(b + 1).copied().unwrap_or(self.lines.len());
            let next = if b + 1 < n { Tgt::Block(b + 1) } else { Tgt::External };
            let last = (start..end).rev().find_map(|i| match &self.lines[i] {
                Line::Ins(ins) if !ins.gone => Some((i, ins)),
                _ => None,
            });
            let target = |i: usize, ins: &Ins| match keys.get(&Self::key(&scope, i, &ins.arg)) {
                Some(&t) => Tgt::Block(block_of[t]),
                None => Tgt::External,
            };
            let term = match last {
                Some((i, ins)) if is_branch(ins.op) => {
                    if ins.always {
                        Term::Jmp(target(i, ins))
                    } else {
                        Term::Cond(target(i, ins), next)
                    }
                }
                Some((i, ins)) if ins.op == "jmp" => {
                    if ins.mode == Mode::Mem { Term::Jmp(target(i, ins)) } else { Term::Jmp(Tgt::External) }
                }
                Some((_, ins)) if matches!(ins.op, "rts" | "rti") => Term::Ret,
                _ => Term::Fall(next),
            };
            blocks.push(Block { start, end, term });
        }
        Some(blocks)
    }

    // ---- forward: values ----

    fn entry_state(&self) -> St {
        let mut st = St {
            l: (0..self.ncells + 3).map(|i| full(ENTRY + i as u64)).collect(),
            c: Tri::U,
            z: Tri::U,
            n: Tri::U,
            csrc: None,
            zsrc: ZSrc::None,
            cadd: None,
        };
        // (argument bytes that are the same constant in every call)
        for (c, v) in &self.entry_consts {
            st.l[3 + c] = v.clone();
        }
        st
    }

    /// The value of the operand of the instruction at line `li`.
    fn operand(&self, st: &St, li: usize, ins: &Ins) -> Loc {
        match (ins.mode, ins.cell) {
            (Mode::Imm, _) => ins.ival.clone().unwrap(),
            (Mode::Mem, Some(c)) => st.l[3 + c].clone(),
            _ => full(FRESH + 32 * li as u64),
        }
    }

    fn set_nz(st: &mut St, v: &Loc) {
        st.z = if v.hi == 0 {
            Tri::T
        } else if v.lo > 0 {
            Tri::F
        } else {
            Tri::U
        };
        st.n = if v.hi < 128 {
            Tri::F
        } else if v.lo >= 128 {
            Tri::T
        } else {
            Tri::U
        };
        st.zsrc = ZSrc::Val(v.vn);
    }

    /// A shift or rotation of `v`: the result and the new carry.
    fn shift(op: &str, v: &Loc, c: Tri, fresh: u64) -> (Loc, Tri) {
        let exact = v.lo == v.hi;
        match op {
            "asl" | "rol" => {
                let carry = if v.hi < 128 {
                    Tri::F
                } else if v.lo >= 128 {
                    Tri::T
                } else {
                    Tri::U
                };
                let cin = match (op, c) {
                    ("asl", _) | (_, Tri::F) => Some(0u8),
                    (_, Tri::T) => Some(1),
                    _ => None,
                };
                let r = match cin {
                    Some(b) if v.hi < 128 => ranged(fresh, v.lo * 2 + b, v.hi * 2 + b),
                    Some(b) if exact => konst((v.lo << 1) | b),
                    _ => full(fresh),
                };
                (r, carry)
            }
            _ => {
                let carry = if exact { tri(v.lo & 1 == 1) } else { Tri::U };
                let cin = match (op, c) {
                    ("lsr", _) | (_, Tri::F) => Some(0u8),
                    (_, Tri::T) => Some(0x80),
                    _ => None,
                };
                let r = match cin {
                    Some(b) => ranged(fresh, (v.lo >> 1) | b, (v.hi >> 1) | b),
                    None => full(fresh),
                };
                (r, carry)
            }
        }
    }

    /// The smallest `2^k - 1` that is not below `v`.
    fn mask_of(v: u8) -> u8 {
        if v == 0 { 0 } else { 0xff >> v.leading_zeros() }
    }

    /// The effect of one instruction on the state.
    fn step(&self, st: &mut St, li: usize, ins: &Ins) {
        let fresh = FRESH + 32 * li as u64 + 1;
        if matches!(
            ins.op,
            "adc" | "sbc" | "cmp" | "cpx" | "cpy" | "asl" | "lsr" | "rol" | "ror" | "clc" | "sec" | "plp" | "jsr"
        ) {
            st.cadd = None;
        }
        let reg = |op: &str| match op.as_bytes()[2] {
            b'x' => 1,
            b'y' => 2,
            _ => 0,
        };
        match ins.op {
            "lda" | "ldx" | "ldy" => {
                let v = self.operand(st, li, ins);
                Self::set_nz(st, &v);
                st.l[reg(ins.op)] = v;
            }
            "sta" | "stx" | "sty" => {
                if let Some(c) = ins.cell {
                    st.l[3 + c] = st.l[reg(ins.op)].clone();
                }
            }
            "tax" | "tay" | "txa" | "tya" => {
                let (from, to) = match ins.op {
                    "tax" => (0, 1),
                    "tay" => (0, 2),
                    "txa" => (1, 0),
                    _ => (2, 0),
                };
                let v = st.l[from].clone();
                Self::set_nz(st, &v);
                st.l[to] = v;
            }
            "tsx" => {
                let v = full(fresh);
                Self::set_nz(st, &v);
                st.l[1] = v;
            }
            "and" | "ora" | "eor" => {
                let m = self.operand(st, li, ins);
                let a = st.l[0].clone();
                let (ea, em) = (a.lo == a.hi, m.lo == m.hi);
                let r = match ins.op {
                    _ if ea && em => konst(match ins.op {
                        "and" => a.lo & m.lo,
                        "ora" => a.lo | m.lo,
                        _ => a.lo ^ m.lo,
                    }),
                    // a mask that keeps every bit the value can have
                    "and" if em && a.hi <= m.lo && m.lo & (m.lo.wrapping_add(1)) == 0 => a,
                    "and" => ranged(fresh, 0, a.hi.min(m.hi)),
                    "ora" | "eor" if em && m.lo == 0 => a,
                    "ora" => ranged(fresh, a.lo.max(m.lo), Self::mask_of(a.hi.max(m.hi))),
                    _ => ranged(fresh, 0, Self::mask_of(a.hi.max(m.hi))),
                };
                Self::set_nz(st, &r);
                st.l[0] = r;
            }
            "adc" => {
                let m = self.operand(st, li, ins);
                let a = st.l[0].clone();
                let (c0, c1) = match st.c {
                    Tri::F => (0u32, 0u32),
                    Tri::T => (1, 1),
                    Tri::U => (0, 1),
                };
                let (lo, hi) = (a.lo as u32 + m.lo as u32 + c0, a.hi as u32 + m.hi as u32 + c1);
                let r;
                if hi <= 255 {
                    st.c = Tri::F;
                    r = if m.hi == 0 && c1 == 0 { a } else { ranged(fresh, lo as u8, hi as u8) };
                } else if lo >= 256 {
                    st.c = Tri::T;
                    r = ranged(fresh, (lo - 256) as u8, (hi - 256) as u8);
                } else {
                    st.c = Tri::U;
                    r = full(fresh);
                }
                st.csrc = None;
                st.cadd = (r.vn == fresh).then_some(fresh);
                Self::set_nz(st, &r);
                st.l[0] = r;
            }
            "sbc" => {
                let m = self.operand(st, li, ins);
                let a = st.l[0].clone();
                // the borrow: at least, at most
                let (b0, b1) = match st.c {
                    Tri::T => (0i32, 0i32),
                    Tri::F => (1, 1),
                    Tri::U => (0, 1),
                };
                let (lo, hi) = (a.lo as i32 - m.hi as i32 - b1, a.hi as i32 - m.lo as i32 - b0);
                let r;
                if lo >= 0 {
                    st.c = Tri::T;
                    r = if m.hi == 0 && b1 == 0 { a } else { ranged(fresh, lo as u8, hi as u8) };
                } else if hi < 0 {
                    st.c = Tri::F;
                    r = ranged(fresh, (lo + 256) as u8, (hi + 256) as u8);
                } else {
                    st.c = Tri::U;
                    r = full(fresh);
                }
                st.csrc = None;
                Self::set_nz(st, &r);
                st.l[0] = r;
            }
            "cmp" | "cpx" | "cpy" => {
                let m = self.operand(st, li, ins);
                let r = st.l[match ins.op {
                    "cpx" => 1,
                    "cpy" => 2,
                    _ => 0,
                }]
                .clone();
                if r.vn == m.vn {
                    st.c = Tri::T;
                    st.z = Tri::T;
                } else {
                    st.c = if r.lo >= m.hi {
                        Tri::T
                    } else if r.hi < m.lo {
                        Tri::F
                    } else {
                        Tri::U
                    };
                    st.z = if r.lo == r.hi && m.lo == m.hi && r.lo == m.lo {
                        Tri::T
                    } else if r.hi < m.lo || m.hi < r.lo {
                        Tri::F
                    } else {
                        Tri::U
                    };
                    // x against x + k (the end of a slice against its
                    // start): not equal
                    let stepped = |a: u64, b: u64| {
                        self.adds.get(&a).is_some_and(|d| {
                            d.cin == Cin::Zero
                                && ((d.x == b && (1..256).contains(&d.y)) || (d.y == b && (1..256).contains(&d.x)))
                        })
                    };
                    if self.adds_valid && (stepped(m.vn, r.vn) || stepped(r.vn, m.vn)) {
                        st.z = Tri::F;
                    }
                }
                st.n = Tri::U;
                let cmp = Cmp { l: r, r: m };
                st.csrc = Some(cmp.clone());
                st.zsrc = ZSrc::Cmp(cmp);
            }
            "bit" => {
                st.z = Tri::U;
                st.n = Tri::U;
                st.zsrc = ZSrc::None;
            }
            "asl" | "lsr" | "rol" | "ror" => {
                let at = match (ins.mode, ins.cell) {
                    (Mode::Imp, _) => Some(0),
                    (_, Some(c)) => Some(3 + c),
                    _ => None,
                };
                match at {
                    Some(i) => {
                        let (r, c) = Self::shift(ins.op, &st.l[i], st.c, fresh);
                        st.c = c;
                        Self::set_nz(st, &r);
                        st.l[i] = r;
                    }
                    None => {
                        st.c = Tri::U;
                        st.z = Tri::U;
                        st.n = Tri::U;
                        st.zsrc = ZSrc::None;
                    }
                }
                st.csrc = None;
            }
            "inc" | "dec" | "inx" | "dex" | "iny" | "dey" => {
                let at = match ins.op {
                    "inx" | "dex" => Some(1),
                    "iny" | "dey" => Some(2),
                    _ => ins.cell.map(|c| 3 + c),
                };
                match at {
                    Some(i) => {
                        let v = &st.l[i];
                        let up = matches!(ins.op, "inc" | "inx" | "iny");
                        let r = if up && v.hi < 255 {
                            ranged(fresh, v.lo + 1, v.hi + 1)
                        } else if !up && v.lo > 0 {
                            ranged(fresh, v.lo - 1, v.hi - 1)
                        } else if v.lo == v.hi {
                            konst(if up { v.lo.wrapping_add(1) } else { v.lo.wrapping_sub(1) })
                        } else {
                            full(fresh)
                        };
                        Self::set_nz(st, &r);
                        st.l[i] = r;
                    }
                    None => {
                        st.z = Tri::U;
                        st.n = Tri::U;
                        st.zsrc = ZSrc::None;
                    }
                }
            }
            "clc" => {
                st.c = Tri::F;
                st.csrc = None;
            }
            "sec" => {
                st.c = Tri::T;
                st.csrc = None;
            }
            "pla" => {
                let v = full(fresh);
                Self::set_nz(st, &v);
                st.l[0] = v;
            }
            "plp" => {
                st.c = Tri::U;
                st.z = Tri::U;
                st.n = Tri::U;
                st.csrc = None;
                st.zsrc = ZSrc::None;
            }
            "jsr" => {
                for r in 0..3 {
                    st.l[r] = full(fresh + r as u64);
                }
                // the routine may use the scratch pointers
                for k in self.nframe..self.ncells {
                    st.l[3 + k] = full(fresh + 3 + (k - self.nframe) as u64);
                }
                st.c = Tri::U;
                st.z = Tri::U;
                st.n = Tri::U;
                st.csrc = None;
                st.zsrc = ZSrc::None;
            }
            _ => {}
        }
    }

    /// At the start of an `asm!` block (line `li`) the registers, the
    /// flags and the scratch pointers are undefined: nothing in the block may count on what the
    /// code before it left in them (and so that code is not kept for it,
    /// see `asm_entry`).
    fn forget_regs(&self, st: &mut St, li: usize) {
        for r in 0..3 {
            st.l[r] = full(FRESH + 32 * li as u64 + 12 + r as u64);
        }
        for k in self.nframe..self.ncells {
            st.l[3 + k] = full(FRESH + 32 * li as u64 + 15 + (k - self.nframe) as u64);
        }
        st.c = Tri::U;
        st.z = Tri::U;
        st.n = Tri::U;
        st.csrc = None;
        st.zsrc = ZSrc::None;
        st.cadd = None;
    }

    /// Whether the branch `op` is taken, if the state tells.
    fn known(st: &St, op: &str) -> Option<bool> {
        let (flag, when) = match op {
            "bcc" => (st.c, Tri::F),
            "bcs" => (st.c, Tri::T),
            "bne" => (st.z, Tri::F),
            "beq" => (st.z, Tri::T),
            "bpl" => (st.n, Tri::F),
            "bmi" => (st.n, Tri::T),
            _ => return None,
        };
        if flag == Tri::U { None } else { Some(flag == when) }
    }

    /// Narrows the range of every location with value number `vn`.
    fn narrow(st: &mut St, vn: u64, lo: u8, hi: u8) {
        if vn < ENTRY {
            return; // an immediate
        }
        for l in st.l.iter_mut() {
            if l.vn == vn {
                let (nlo, nhi) = (l.lo.max(lo), l.hi.min(hi));
                if nlo < nhi {
                    l.lo = nlo;
                    l.hi = nhi;
                } else if nlo == nhi {
                    *l = konst(nlo);
                }
                // nlo > nhi: this edge is never taken; leave the state
            }
        }
    }

    /// What the branch `op` tells on the edge where it is taken (or not).
    fn refine(st: &mut St, op: &str, taken: bool) {
        match (op, taken) {
            ("bcc", true) | ("bcs", false) => {
                st.c = Tri::F;
                if let Some(Cmp { l, r }) = st.csrc.clone() {
                    // l < r
                    if r.hi > 0 {
                        Self::narrow(st, l.vn, 0, r.hi - 1);
                    }
                    if l.lo < 255 {
                        Self::narrow(st, r.vn, l.lo + 1, 255);
                    }
                }
            }
            ("bcs", true) | ("bcc", false) => {
                st.c = Tri::T;
                if let Some(Cmp { l, r }) = st.csrc.clone() {
                    // l >= r
                    Self::narrow(st, l.vn, r.lo, 255);
                    Self::narrow(st, r.vn, 0, l.hi);
                }
            }
            ("beq", true) | ("bne", false) => {
                st.z = Tri::T;
                match st.zsrc.clone() {
                    // (a value that is zero is not negative)
                    ZSrc::Val(v) => {
                        st.n = Tri::F;
                        Self::narrow(st, v, 0, 0)
                    }
                    ZSrc::Cmp(Cmp { l, r }) => {
                        Self::narrow(st, l.vn, r.lo, r.hi);
                        Self::narrow(st, r.vn, l.lo, l.hi);
                    }
                    ZSrc::None => {}
                }
            }
            ("bne", true) | ("beq", false) => {
                st.z = Tri::F;
                match st.zsrc.clone() {
                    ZSrc::Val(v) => Self::narrow(st, v, 1, 255),
                    ZSrc::Cmp(Cmp { l, r }) => {
                        for (a, b) in [(&l, &r), (&r, &l)] {
                            if b.lo == b.hi {
                                if a.lo == b.lo && a.lo < 255 {
                                    Self::narrow(st, a.vn, a.lo + 1, 255);
                                } else if a.hi == b.lo && a.hi > 0 {
                                    Self::narrow(st, a.vn, 0, a.hi - 1);
                                }
                            }
                        }
                    }
                    ZSrc::None => {}
                }
            }
            ("bpl", true) | ("bmi", false) => {
                st.n = Tri::F;
                if let ZSrc::Val(v) = st.zsrc {
                    Self::narrow(st, v, 0, 127);
                }
            }
            ("bmi", true) | ("bpl", false) => {
                st.n = Tri::T;
                if let ZSrc::Val(v) = st.zsrc {
                    Self::narrow(st, v, 128, 255);
                }
            }
            _ => {}
        }
    }

    /// The state at the start of block `b`: what holds on every edge into
    /// it (`incoming`). `old` is the state so far, `changes` how often it
    /// has changed: ranges and flags only ever get less precise, and a
    /// value that keeps changing gets a number of its own, so the analysis
    /// comes to an end.
    fn join(&self, b: usize, old: Option<&St>, incoming: &[&St], changes: u32, ahead: bool) -> Option<St> {
        let (first, rest) = incoming.split_first()?;
        let nlocs = (self.ncells + 3) as u64;
        let phi = |i: usize| PHI + b as u64 * nlocs + i as u64;
        let mut new = (*first).clone();
        // N and Z are those of A on every edge: then also here.
        let nz_of_a = incoming.iter().all(|s| s.zsrc == ZSrc::Val(s.l[0].vn));
        for f in rest {
            for (i, (c, f)) in new.l.iter_mut().zip(&f.l).enumerate() {
                if c.vn != f.vn {
                    c.vn = phi(i);
                }
                c.lo = c.lo.min(f.lo);
                c.hi = c.hi.max(f.hi);
            }
            for (c, f) in [(&mut new.c, f.c), (&mut new.z, f.z), (&mut new.n, f.n)] {
                if *c != f {
                    *c = Tri::U;
                }
            }
            if new.csrc != f.csrc {
                new.csrc = None;
            }
            if new.zsrc != f.zsrc {
                new.zsrc = ZSrc::None;
            }
            if new.cadd != f.cadd {
                new.cadd = None;
            }
        }
        if let Some(old) = old {
            for (i, (c, o)) in new.l.iter_mut().zip(&old.l).enumerate() {
                if o.vn == phi(i) || (changes > 8 && c.vn != o.vn) {
                    c.vn = phi(i);
                }
                let (lo, hi) = (c.lo.min(o.lo), c.hi.max(o.hi));
                // a range that keeps growing (a counter): give up on it
                (c.lo, c.hi) = if changes > 3 && (lo, hi) != (o.lo, o.hi) { (0, 255) } else { (lo, hi) };
            }
            for (c, o) in [(&mut new.c, old.c), (&mut new.z, old.z), (&mut new.n, old.n)] {
                if o == Tri::U {
                    *c = Tri::U;
                }
            }
            if old.csrc.is_none() {
                new.csrc = None;
            }
            if old.zsrc == ZSrc::None {
                new.zsrc = ZSrc::None;
            }
            if old.cadd.is_none() {
                new.cadd = None;
            }
        }
        // Places that hold the same value on every edge (each arm of a
        // `match` leaves its result in A and in a variable) hold the same
        // value here: the later one gets the number of the earlier one.
        // (Not one that had its own number before: that is final. And not
        // at the head of a loop, `ahead` false: a counter that is known
        // to be in A there has to be brought there by its step, which
        // then is an addition where it was an `inc`.)
        if ahead && !rest.is_empty() && std::env::var_os("RUST64_NO_JOIN_EQ").is_none() {
            let mut merged: HashMap<u64, Vec<usize>> = HashMap::new();
            for j in 0..new.l.len() {
                if new.l[j].vn != phi(j) {
                    continue;
                }
                let key = first.l[j].vn;
                let same = merged.get(&key).and_then(|c| {
                    c.iter().copied().find(|&i| incoming.iter().all(|s| s.l[i].vn == s.l[j].vn))
                });
                match same {
                    Some(i) if !old.is_some_and(|o| o.l[j].vn == phi(j)) => {
                        new.l[j].vn = phi(i);
                    }
                    Some(_) => {}
                    None => merged.entry(key).or_default().push(j),
                }
            }
        }
        for c in new.l.iter_mut() {
            if c.lo == c.hi {
                c.vn = c.lo as u64;
            }
        }
        if nz_of_a {
            new.zsrc = ZSrc::Val(new.l[0].vn);
        }
        Some(new)
    }

    /// Whether the label the branch at line `i` goes to follows it with no
    /// instruction in between.
    fn label_follows(&self, i: usize, name: &str) -> bool {
        for l in &self.lines[i + 1..] {
            match l {
                Line::Label(n) if n == name => return true,
                Line::Ins(ins) if !ins.gone => return false,
                _ => {}
            }
        }
        false
    }

    /// The first tracked byte of the frame that holds the value `vn`.
    ///
    /// Not a scratch pointer: each access through one keeps the setup in
    /// front of it (unless both go), which is what `asm::zp_direct` relies
    /// on; so nothing reads a scratch pointer in another way, and a setup
    /// is not removed because the pointer holds the value already.
    fn canonical(&self, st: &St, vn: u64) -> Option<usize> {
        (0..self.nframe).find(|&c| st.l[3 + c].vn == vn && !self.escaped[c])
    }

    fn forward(&mut self, blocks: &[Block]) -> bool {
        let n = blocks.len();
        let mut inn: Vec<Option<St>> = vec![None; n];
        let mut changes = vec![0u32; n];
        // The states on the edges out of each block (taken or the only
        // one, not taken) and the edges into each block.
        let mut outs: Vec<[Option<St>; 2]> = vec![[None, None]; n];
        let mut preds: Vec<Vec<(usize, usize)>> = vec![Vec::new(); n];
        let succs = |b: usize| -> [Option<usize>; 2] {
            let blk = |t: Tgt| if let Tgt::Block(t) = t { Some(t) } else { None };
            match blocks[b].term {
                Term::Fall(t) | Term::Jmp(t) => [blk(t), None],
                Term::Cond(t, f) => [blk(t), blk(f)],
                Term::Ret => [None, None],
            }
        };
        for b in 0..n {
            for (slot, t) in succs(b).into_iter().enumerate() {
                if let Some(t) = t {
                    preds[t].push((b, slot));
                }
            }
        }
        let entry = self.entry_state();
        inn[0] = Some(entry.clone());
        let mut queued = vec![false; n];
        let mut work = std::collections::VecDeque::from([0usize]);
        queued[0] = true;
        while let Some(b) = work.pop_front() {
            queued[b] = false;
            let mut st = inn[b].clone().unwrap();
            let mut last = None;
            for i in blocks[b].start..blocks[b].end {
                if let Line::Ins(ins) = &self.lines[i]
                    && !ins.gone
                {
                    self.step(&mut st, i, ins);
                    last = Some(ins);
                } else if self.asm_entry(i) {
                    self.forget_regs(&mut st, i);
                }
            }
            let out = match blocks[b].term {
                Term::Cond(..) => {
                    let op = last.map_or("", |i| i.op);
                    let known = Self::known(&st, op);
                    let taken = (known != Some(false)).then(|| {
                        let mut s = st.clone();
                        Self::refine(&mut s, op, true);
                        s
                    });
                    let fall = (known != Some(true)).then(|| {
                        Self::refine(&mut st, op, false);
                        st
                    });
                    [taken, fall]
                }
                _ => [Some(st), None],
            };
            if out == outs[b] {
                continue;
            }
            outs[b] = out;
            for t in succs(b).into_iter().flatten() {
                let mut incoming: Vec<&St> =
                    preds[t].iter().filter_map(|&(p, slot)| outs[p][slot].as_ref()).collect();
                if t == 0 {
                    incoming.push(&entry);
                }
                let ahead = preds[t].iter().all(|&(p, _)| p < t);
                let new = self.join(t, inn[t].as_ref(), &incoming, changes[t], ahead);
                if new.is_some() && new != inn[t] {
                    inn[t] = new;
                    changes[t] += 1;
                    if !std::mem::replace(&mut queued[t], true) {
                        work.push_back(t);
                    }
                }
            }
        }

        self.adds_valid = false;
        // Rewrite with what is known.
        self.adds.clear();
        let mut changed = false;
        for (b, block) in blocks.iter().enumerate() {
            if inn[b].is_none() {
                // not reachable
                for i in block.start..block.end {
                    if let Line::Ins(ins) = &mut self.lines[i]
                        && !ins.gone
                    {
                        ins.gone = true;
                        changed = true;
                    }
                }
            }
        }
        // (what is stored to the argument slots of other functions)
        let mut passed: Vec<(String, u64, Option<u8>)> = Vec::new();
        for (b, block) in blocks.iter().enumerate() {
            let Some(mut st) = inn[b].clone() else { continue };
            for i in block.start..block.end {
                let Line::Ins(ins) = &self.lines[i] else {
                    if self.asm_entry(i) {
                        self.forget_regs(&mut st, i);
                    }
                    continue;
                };
                if ins.gone {
                    continue;
                }
                let mut new: Option<(&'static str, String)> = None;
                let mut gone = false;
                let mut redundant = None;
                let mut always = false;
                let reg = match ins.op {
                    "lda" => Some(0),
                    "ldx" => Some(1),
                    "ldy" => Some(2),
                    _ => None,
                };
                let readable = ins.mode == Mode::Imm || ins.cell.is_some();
                let ora = std::env::var_os("RUST64_NO_ORA").is_none();
                if is_branch(ins.op) {
                    match Self::known(&st, ins.op) {
                        Some(false) => gone = true,
                        Some(true) => {
                            if self.label_follows(i, &ins.arg) {
                                gone = true;
                            } else {
                                always = true;
                            }
                        }
                        None => {}
                    }
                } else if let Some(r) = reg
                    && readable
                {
                    let v = self.operand(&st, i, ins);
                    let holds = |q: usize| st.l[q].vn == v.vn;
                    // In the first rounds a constant is not taken from X
                    // or Y: an access that gets an index (`plan`) needs
                    // them free.
                    let keep_xy = self.early && v.vn < ENTRY;
                    if holds(r) {
                        // the flags are already those of the value, or
                        // the load goes when they are not needed
                        // (or of a constant, and known to be so: `lda #0`
                        // where a `bne` was not taken)
                        let flags = v.lo == v.hi && st.z == tri(v.lo == 0) && st.n == tri(v.lo >= 128);
                        if st.zsrc == ZSrc::Val(v.vn) || flags {
                            gone = true;
                        } else if r == 0 && ins.mode == Mode::Mem && !self.early && ora {
                            // only the flags are needed: without the
                            // memory, whose store may then go
                            new = Some(("ora", "#$00".to_owned()));
                        } else {
                            redundant = Some(NZ);
                        }
                    } else if r != 0 && holds(0) {
                        new = Some((if r == 1 { "tax" } else { "tay" }, String::new()));
                    } else if r == 0 && holds(1) && !keep_xy {
                        new = Some(("txa", String::new()));
                    } else if r == 0 && holds(2) && !keep_xy {
                        new = Some(("tya", String::new()));
                    } else if ins.mode == Mode::Mem {
                        if let Some(t) = self.imm_text(v.vn) {
                            new = Some((ins.op, t));
                        } else if let Some(c) = self.canonical(&st, v.vn)
                            && Some(c) != ins.cell
                        {
                            new = Some((ins.op, self.cell_name(c)));
                        }
                    }
                } else if matches!(ins.op, "adc" | "sbc" | "and" | "ora" | "eor" | "cmp" | "cpx" | "cpy")
                    && ins.cell.is_some()
                {
                    let v = self.operand(&st, i, ins);
                    if let Some(t) = self.imm_text(v.vn) {
                        new = Some((ins.op, t));
                    } else if let Some(c) = self.canonical(&st, v.vn)
                        && Some(c) != ins.cell
                    {
                        new = Some((ins.op, self.cell_name(c)));
                    }
                } else if matches!(ins.op, "sta" | "stx" | "sty")
                    && let Some(c) = ins.cell
                {
                    let r = match ins.op {
                        "sta" => 0,
                        "stx" => 1,
                        _ => 2,
                    };
                    if st.l[3 + c].vn == st.l[r].vn && c < self.nframe {
                        gone = true;
                    }
                } else if matches!(ins.mode, Mode::MemX | Mode::MemY)
                    && matches!(ins.op, "lda" | "sta" | "adc" | "sbc" | "and" | "ora" | "eor" | "cmp")
                    && !self.early
                    && std::env::var_os("RUST64_NO_INDEX_ADD").is_none()
                {
                    // The index is the other index register plus a constant
                    // (and that did not carry): the constant goes into the
                    // address, and the register that was made for the
                    // access is not needed.
                    let (r, other, from, to) =
                        if ins.mode == Mode::MemX { (1, 2, ",x", ",y") } else { (2, 1, ",y", ",x") };
                    if let Some(d) = self.adds.get(&st.l[r].vn)
                        && d.cin == Cin::Zero
                        && d.cout == Tri::F
                    {
                        let base = st.l[other].vn;
                        let k = if d.x == base && d.y < 256 {
                            Some(d.y)
                        } else if d.y == base && d.x < 256 {
                            Some(d.x)
                        } else {
                            None
                        };
                        if let (Some(k), Some(at)) = (k, ins.arg.strip_suffix(from))
                            && !at.contains(['(', ')'])
                        {
                            new = Some((ins.op, format!("{at}+{k}{to}")));
                        }
                    }
                } else if matches!(ins.op, "sta" | "stx" | "sty") && ins.mode == Mode::Mem && ins.arg.starts_with('L') {
                    let r = match ins.op {
                        "sta" => 0,
                        "stx" => 1,
                        _ => 2,
                    };
                    if let Some((base, off)) = sym_off(&ins.arg) {
                        let v = &st.l[r];
                        passed.push((base.to_owned(), off as u64, (v.lo == v.hi).then_some(v.lo as u8)));
                    }
                } else if matches!(ins.op, "tax" | "tay" | "txa" | "tya") {
                    let (from, to) = match ins.op {
                        "tax" => (0, 1),
                        "tay" => (0, 2),
                        "txa" => (1, 0),
                        _ => (2, 0),
                    };
                    if st.l[from].vn == st.l[to].vn {
                        redundant = Some(NZ);
                    }
                } else if (ins.op == "clc" && st.c == Tri::F) || (ins.op == "sec" && st.c == Tri::T) {
                    gone = true;
                }
                // An operation that leaves A as it is (`and #$0f` of a
                // value below 16, `adc #0` without a carry).
                if matches!(ins.op, "and" | "ora" | "eor" | "adc" | "sbc") && new.is_none() {
                    let before = st.l[0].vn;
                    let mut after = st.clone();
                    self.step(&mut after, i, ins);
                    if after.l[0].vn == before {
                        redundant = Some(if matches!(ins.op, "adc" | "sbc") { FLAGS } else { NZ });
                    }
                }
                let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
                if let Some((op, arg)) = new {
                    ins.op = op;
                    ins.mode = parse_mode(&arg);
                    ins.arg = arg;
                    ins.cell = None;
                    ins.ival = None;
                    changed = true;
                }
                if gone {
                    ins.gone = true;
                    changed = true;
                    continue;
                }
                let always = always || ins.always;
                changed |= ins.always != always;
                ins.always = always;
                ins.redundant = redundant;
                let (mode, arg) = (ins.mode, ins.arg.clone());
                if mode == Mode::Imm {
                    let v = self.imm(&arg);
                    let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
                    ins.ival = Some(v);
                } else if mode == Mode::Mem {
                    let cell = self.own(&arg).filter(|&c| !self.escaped[c]);
                    let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
                    ins.cell = cell;
                }
                let Line::Ins(ins) = &self.lines[i] else { unreachable!() };
                let plan = self.plan(&st, ins);
                // A + 1 or A - 1 where X or Y holds the same value
                let one = ins.ival.as_ref().is_some_and(|v| v.vn == 1);
                let constant = ins.ival.as_ref().is_some_and(|v| v.vn < 256);
                let (add_same, add_clear) = if ins.op == "adc" && constant {
                    let a = st.l[0].vn;
                    let same: Vec<usize> =
                        (0..self.nframe).filter(|&c| !self.escaped[c] && st.l[3 + c].vn == a).take(8).collect();
                    (same, st.c == Tri::F)
                } else {
                    (Vec::new(), false)
                };
                let stepping = match (ins.op, st.c) {
                    ("adc", Tri::F) | ("sbc", Tri::T) if one => {
                        let c = st.c;
                        [(1, RX), (2, RY)].into_iter().find(|&(r, _)| st.l[r].vn == st.l[0].vn).map(|(_, r)| (r, c))
                    }
                    _ => None,
                };
                let (via, via_y) = match ins.ptr {
                    Some(z) if ins.mode == Mode::IndY => {
                        let (lo, hi) = (st.l[3 + z].vn, st.l[3 + z + 1].vn);
                        let same: Vec<usize> = (0..self.nframe.saturating_sub(1))
                            .filter(|&c| !self.escaped[c] && st.l[3 + c].vn == lo && st.l[3 + c + 1].vn == hi)
                            .take(8)
                            .collect();
                        (same, (st.l[2].lo == st.l[2].hi).then_some(st.l[2].lo))
                    }
                    _ => (Vec::new(), None),
                };
                // an addition: what is added, and the carry that goes in
                let add = (ins.op == "adc").then(|| {
                    let cin = match (st.c, st.cadd) {
                        (Tri::F, _) => Some(Cin::Zero),
                        (Tri::T, _) => Some(Cin::One),
                        (Tri::U, Some(v)) => Some(Cin::Of(v)),
                        (Tri::U, None) => None,
                    };
                    (st.l[0].vn, self.operand(&st, i, ins).vn, cin)
                });
                self.step(&mut st, i, ins);
                let carry = match st.c {
                    _ if !(matches!(ins.op, "cmp" | "cpx" | "cpy" | "adc" | "sbc")
                        || (matches!(ins.op, "asl" | "lsr" | "rol" | "ror") && ins.mode == Mode::Imp)) =>
                    {
                        None
                    }
                    Tri::T => Some(true),
                    Tri::F => Some(false),
                    Tri::U => None,
                };
                if let Some((x, y, Some(cin))) = add
                    && let Some(v) = st.cadd
                {
                    self.adds.insert(v, AddDef { x, y, cin, cout: st.c });
                }
                let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
                ins.carry = carry;
                ins.plan = plan;
                ins.step_reg = stepping.map(|(r, c)| (r, st.c == c));
                ins.add_same = add_same;
                ins.add_clear = add_clear;
                ins.via = via;
                ins.via_y = via_y;
            }
        }
        self.stores = passed;
        self.adds_valid = true;
        // The branch at the end of each block, when the block is entered
        // from the one in front of it (for `rotate`: the first test of a
        // loop).
        let mut firsts: Vec<(usize, Option<bool>)> = Vec::new();
        let mut heads: HashMap<usize, St> = HashMap::new();
        for t in 1..n {
            let slot = match blocks[t - 1].term {
                Term::Fall(Tgt::Block(s)) if s == t => 0,
                Term::Cond(_, Tgt::Block(s)) if s == t => 1,
                _ => continue,
            };
            let (Term::Cond(..), Some(mut st)) = (blocks[t].term, outs[t - 1][slot].clone()) else { continue };
            heads.insert(blocks[t].start, st.clone());
            let mut last = None;
            for i in blocks[t].start..blocks[t].end {
                if let Line::Ins(ins) = &self.lines[i]
                    && !ins.gone
                {
                    self.step(&mut st, i, ins);
                    last = Some((i, ins.op));
                } else if self.asm_entry(i) {
                    self.forget_regs(&mut st, i);
                }
            }
            if let Some((i, op)) = last {
                firsts.push((i, Self::known(&st, op)));
            }
        }
        for l in self.lines.iter_mut() {
            if let Line::Ins(ins) = l {
                ins.first = None;
            }
        }
        for (i, first) in firsts {
            let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
            ins.first = first;
        }
        self.heads = heads;
        self.adds_valid = false;
        changed
    }

    /// The operand `address,x` of an access to `lo/hi + index + y`, where
    /// `lo` and the two parts of the high byte (`h1 + h2`) are immediates.
    fn abs_operand(&self, lo: u64, h1: u64, h2: u64, y: u8) -> Option<String> {
        if lo < 256 && h1 < 256 && h2 < 256 {
            let addr = (lo + 256 * ((h1 + h2) & 0xff) + y as u64) & 0xffff;
            return Some(format!("${addr:04x},x"));
        }
        // `#<(label+n)` with `#>(label+n)` and a number of pages
        let (hs, k) = if h1 >= 256 && h2 < 256 {
            (h1, h2)
        } else if h2 >= 256 && h1 < 256 {
            (h2, h1)
        } else {
            return None;
        };
        if lo < 256 {
            return None;
        }
        let lo = self.imm_text(lo)?;
        let hi = self.imm_text(hs)?;
        let expr = lo.strip_prefix("#<")?;
        if hi.strip_prefix("#>")? != expr {
            return None;
        }
        // without the brackets: `(x),y` would be another addressing mode
        let inner = expr.strip_prefix('(').and_then(|e| e.strip_suffix(')')).unwrap_or(expr);
        if inner.contains(['(', ')', ',', '-', '*', '/', '<', '>', '#']) {
            return None;
        }
        Some(format!("{inner}+{},x", 256 * k + y as u64))
    }

    /// A better way to make the access `ins` through a scratch pointer,
    /// given what is known before it: the pointer is the sum of an address
    /// and a byte, or of a pointer in the frame and a byte.
    fn plan(&self, st: &St, ins: &Ins) -> Option<Plan> {
        let p = ins.ptr?;
        if !matches!(ins.op, "lda" | "sta" | "adc" | "sbc" | "and" | "ora" | "eor" | "cmp") {
            return None;
        }
        let y = &st.l[2];
        if y.lo != y.hi {
            return None;
        }
        let y = y.lo;
        let (lo, hi) = (&st.l[3 + p], &st.l[3 + p + 1]);
        // a known address (the pointer of a function that was put into its
        // caller, to a variable of the caller)
        if lo.vn < ENTRY
            && hi.vn < ENTRY
            && (lo.vn < 256) == (hi.vn < 256)
            && let Some(o) = self.abs_operand(lo.vn, hi.vn, 0, y)
            && let Some(operand) = o.strip_suffix(",x")
        {
            return Some(Plan::Direct { operand: operand.to_owned() });
        }
        let lod = self.adds.get(&lo.vn)?;
        if lod.cin != Cin::Zero {
            return None;
        }
        let imm = |vn: u64| vn < ENTRY;
        // The high byte: an addition that takes the carry of the low one,
        // or a constant if the low one does not carry.
        let (h1, h2) = match self.adds.get(&hi.vn) {
            Some(h) if h.cin == Cin::Of(lo.vn) || (h.cin == Cin::Zero && lod.cout == Tri::F) => (h.x, h.y),
            None if imm(hi.vn) && lod.cout == Tri::F => (hi.vn, 0),
            _ => return None,
        };
        // Where a value is: a tracked byte, or A. (Not X, even if it holds
        // the value: the change must not need a register that the code
        // before it does not know to keep.)
        let find = |vn: u64, a_free: bool| -> Option<Src> {
            if let Some(c) = self.canonical(st, vn) {
                Some(Src::Cell(c))
            } else if a_free && st.l[0].vn == vn {
                Some(Src::A)
            } else {
                None
            }
        };
        for (base, idx) in [(lod.x, lod.y), (lod.y, lod.x)] {
            if imm(idx) {
                continue;
            }
            // an address
            if imm(base) && imm(h1) && imm(h2) {
                let operand = self.abs_operand(base, h1, h2, y)?;
                let idx = find(idx, ins.op == "lda")?;
                return Some(Plan::Abs { operand, idx });
            }
            // a pointer in the frame, at two bytes next to each other
            let high = if h2 == 0 {
                h1
            } else if h1 == 0 {
                h2
            } else {
                continue;
            };
            if y != 0 || imm(base) || imm(high) {
                continue;
            }
            let cell = (0..self.nframe.saturating_sub(1)).find(|&c| {
                !self.escaped[c]
                    && !self.escaped[c + 1]
                    && st.l[3 + c].vn == base
                    && st.l[3 + c + 1].vn == high
                    && self.cell_name(c + 1) == format!("{}+1", self.cell_name(c))
            });
            if let Some(cell) = cell {
                let idx = match find(idx, false) {
                    Some(Src::Cell(c)) => Src::Cell(c),
                    // no transfer from X to Y: only from A
                    _ if st.l[0].vn == idx => Src::A,
                    _ => continue,
                };
                return Some(Plan::Ptr { cell, zp: p, idx });
            }
        }
        None
    }

    /// An operand that names frame byte `c`.
    fn cell_name(&self, c: usize) -> String {
        if c >= self.nframe {
            let k = c - self.nframe;
            let name = crate::asm::ZP[k / 2];
            return if k % 2 == 0 { name.to_owned() } else { format!("{name}+1") };
        }
        // the name that starts closest below the byte; a local's own name
        // (`F<id>v<n>`) before the argument alias
        let mut best: Option<(&str, u64)> = None;
        for (name, &(o, n)) in self.layout {
            if o <= c as u64 && (c as u64) < o + n {
                let better = match best {
                    None => true,
                    Some((bn, bo)) => o > bo || (o == bo && name.as_str() < bn),
                };
                if better {
                    best = Some((name, o));
                }
            }
        }
        let (name, o) = best.unwrap();
        let off = c as u64 - o;
        if off == 0 { name.to_owned() } else { format!("{name}+{off}") }
    }

    // ---- backward: liveness ----

    /// The registers and flags an instruction reads and writes, whether it
    /// reads and writes its memory operand, and whether it has no other
    /// effect.
    fn effect(&self, ins: &Ins) -> (u8, u8, bool, bool, bool) {
        let acc = ins.mode == Mode::Imp;
        let (mut uses, defs, rd, wr, mut pure) = match ins.op {
            "lda" => (0, RA | NZ, true, false, true),
            "ldx" => (0, RX | NZ, true, false, true),
            "ldy" => (0, RY | NZ, true, false, true),
            "sta" => (RA, 0, false, true, true),
            "stx" => (RX, 0, false, true, true),
            "sty" => (RY, 0, false, true, true),
            "tax" => (RA, RX | NZ, false, false, true),
            "tay" => (RA, RY | NZ, false, false, true),
            "txa" => (RX, RA | NZ, false, false, true),
            "tya" => (RY, RA | NZ, false, false, true),
            "tsx" => (0, RX | NZ, false, false, true),
            "txs" => (RX, 0, false, false, false),
            "adc" | "sbc" => (RA | FC, RA | FLAGS, true, false, true),
            "and" | "ora" | "eor" => (RA, RA | NZ, true, false, true),
            "cmp" => (RA, NZ | FC, true, false, true),
            "cpx" => (RX, NZ | FC, true, false, true),
            "cpy" => (RY, NZ | FC, true, false, true),
            "bit" => (RA, NZ | FV, true, false, true),
            "asl" | "lsr" if acc => (RA, RA | NZ | FC, false, false, true),
            "rol" | "ror" if acc => (RA | FC, RA | NZ | FC, false, false, true),
            "asl" | "lsr" => (0, NZ | FC, true, true, true),
            "rol" | "ror" => (FC, NZ | FC, true, true, true),
            "inc" | "dec" => (0, NZ, true, true, true),
            "inx" | "dex" => (RX, RX | NZ, false, false, true),
            "iny" | "dey" => (RY, RY | NZ, false, false, true),
            "clc" | "sec" => (0, FC, false, false, true),
            "clv" => (0, FV, false, false, true),
            "pha" => (RA, 0, false, false, false),
            "pla" => (0, RA | NZ, false, false, false),
            "php" => (FLAGS, 0, false, false, false),
            "plp" => (0, FLAGS, false, false, false),
            "bcc" | "bcs" => (FC, 0, false, false, false),
            "beq" | "bne" => (FZ, 0, false, false, false),
            "bpl" | "bmi" => (FN, 0, false, false, false),
            "bvc" | "bvs" => (FV, 0, false, false, false),
            "jsr" => (self.call_needs(ins).0, ALL, false, false, false),
            _ => (0, 0, false, false, false),
        };
        match ins.mode {
            Mode::MemX | Mode::IndX => uses |= RX,
            Mode::MemY | Mode::IndY => uses |= RY,
            _ => {}
        }
        // A read that may be a hardware register, and a write to memory
        // that is not a tracked byte, stay.
        if rd && ins.cell.is_none() && ins.mode != Mode::Imm && ins.mode != Mode::Imp {
            let a = &ins.arg;
            let name = a.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).find(|w| !w.is_empty());
            if a.contains('$')
                || a.contains('(')
                || a.starts_with(|c: char| c.is_ascii_digit())
                || name.is_none_or(|n| (self.is_io)(n))
            {
                pure = false;
            }
        }
        if wr && ins.cell.is_none() {
            pure = false;
        }
        (uses, defs, rd, wr, pure)
    }

    /// Liveness before instruction `ins`, given liveness after it; true if
    /// the instruction is not needed.
    fn live_step(&self, live: &mut Live, ins: &Ins) -> bool {
        let (uses, defs, rd, wr, pure) = self.effect(ins);
        if let Some(flags) = ins.redundant
            && live.regs & flags == 0
            && !self.keep_redundant
        {
            // goes: the value passes through
            return true;
        }
        let cell_live = ins.cell.is_some_and(|c| live.cells[c]);
        if pure && live.regs & defs == 0 && !(wr && cell_live) && (defs != 0 || wr) {
            return true;
        }
        live.regs = (live.regs & !defs) | uses;
        if let Some(c) = ins.cell {
            if wr {
                live.cells[c] = false;
            }
            if rd {
                live.cells[c] = true;
            }
        }
        // the scratch pointers: read by an access through one, and
        // perhaps by a routine that is called
        if let Some(p) = ins.ptr {
            live.cells[p] = true;
            live.cells[p + 1] = true;
        }
        if ins.op == "jsr" && self.call_needs(ins).1 {
            for c in &mut live.cells[self.nframe..] {
                *c = true;
            }
        }
        false
    }

    /// At the start of an `asm!` block nothing is needed of the registers,
    /// the flags and the scratch pointers.
    fn kill_at_entry(&self, live: &mut Live) {
        live.regs = 0;
        for c in &mut live.cells[self.nframe..] {
            *c = false;
        }
    }

    /// What is live when control leaves the function.
    fn exit_live(&self, needs: (u8, bool)) -> Live {
        // (a result of up to three bytes is in the registers)
        let mut cells = vec![false; self.ncells];
        if self.ret > 3 {
            for c in cells.iter_mut().take(self.ret) {
                *c = true;
            }
        }
        if needs.1 {
            for c in &mut cells[self.nframe..] {
                *c = true;
            }
        }
        Live { regs: needs.0, cells }
    }

    /// What the routine that `ins` calls or jumps to may read: registers
    /// and flags, and whether it may read the scratch pointers.
    ///
    /// * A function of the code generator takes the first three bytes of
    ///   its arguments in A, X and Y and sets the scratch pointers up
    ///   itself.
    /// * A routine the code generator calls takes what its signature says
    ///   in A, X and Y (`call_regs`); of the runtime, the block routines
    ///   take scratch pointers.
    /// * A routine at a numeric address (the KERNAL) knows nothing of the
    ///   scratch pointers.
    /// * Anything else, and every call in an `asm!` block: whatever the
    ///   block has set up.
    fn call_needs(&self, ins: &Ins) -> (u8, bool) {
        let arg = ins.arg.as_str();
        if arg.strip_prefix('F').is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit())) {
            return ((self.call_regs)(arg).unwrap_or(RA | RX | RY), false);
        }
        let numeric = arg.starts_with('$') || arg.starts_with(|c: char| c.is_ascii_digit());
        if !ins.in_asm
            && let Some(regs) = (self.call_regs)(arg)
        {
            return (regs, arg.starts_with("rt_mem"));
        }
        if matches!(arg, "rt_panic" | "rt_exit") {
            return (0, false);
        }
        (ALL & !FV, !numeric)
    }

    /// What is live at the end of block `b`.
    fn live_out(&self, blocks: &[Block], live_in: &[Live], b: usize) -> Live {
        // where control leaves the function: by the branch or jump at the
        // end of the block, or by running into the next function
        let last = (blocks[b].start..blocks[b].end).rev().find_map(|i| match &self.lines[i] {
            Line::Ins(ins) if !ins.gone => Some(ins),
            _ => None,
        });
        let leaves = match (blocks[b].term, last) {
            (Term::Fall(_), _) | (_, None) => (0, false),
            (_, Some(ins)) if ins.mode != Mode::Mem => (ALL, true),
            (_, Some(ins)) => self.call_needs(ins),
        };
        // at a return: the result in A, X and Y
        let result = match self.ret {
            1 => RA,
            2 => RA | RX,
            3 => RA | RX | RY,
            _ => 0,
        };
        let (ext, ret) = (self.exit_live(leaves), self.exit_live((result, false)));
        let pick = |t: Tgt| match t {
            Tgt::Block(t) => &live_in[t],
            Tgt::External => &ext,
        };
        let edges: Vec<&Live> = match blocks[b].term {
            Term::Fall(t) | Term::Jmp(t) => vec![pick(t)],
            Term::Cond(t, f) => vec![pick(t), pick(f)],
            Term::Ret => vec![&ret],
        };
        let mut out = Live { regs: 0, cells: vec![false; self.ncells] };
        for l in edges {
            out.regs |= l.regs;
            for (o, c) in out.cells.iter_mut().zip(&l.cells) {
                *o |= *c;
            }
        }
        out
    }

    /// What is live at the start of each block.
    fn liveness(&self, blocks: &[Block]) -> Vec<Live> {
        let n = blocks.len();
        let mut live_in: Vec<Live> = (0..n).map(|_| Live { regs: 0, cells: vec![false; self.ncells] }).collect();
        loop {
            let mut changed = false;
            for b in (0..n).rev() {
                let mut live = self.live_out(blocks, &live_in, b);
                for i in (blocks[b].start..blocks[b].end).rev() {
                    if let Line::Ins(ins) = &self.lines[i]
                        && !ins.gone
                    {
                        self.live_step(&mut live, ins);
                    } else if self.asm_entry(i) {
                        self.kill_at_entry(&mut live);
                    }
                }
                let cur = &mut live_in[b];
                if cur.regs | live.regs != cur.regs {
                    cur.regs |= live.regs;
                    changed = true;
                }
                for (c, l) in cur.cells.iter_mut().zip(&live.cells) {
                    if *l && !*c {
                        *c = true;
                        changed = true;
                    }
                }
            }
            if !changed {
                return live_in;
            }
        }
    }

    /// Makes the accesses through scratch pointers that have a better way
    /// (`Opt::plan`) that way, where the registers it needs are free. The
    /// old pointer and its computation are then dead and go in the next
    /// round. True if something changed.
    fn apply_plans(&mut self, blocks: &[Block], live_in: &[Live]) -> bool {
        let mut inserts: Vec<(usize, Vec<(&'static str, String)>)> = Vec::new();
        let mut any = false;
        for b in 0..blocks.len() {
            let mut live = self.live_out(blocks, live_in, b);
            for i in (blocks[b].start..blocks[b].end).rev() {
                let Line::Ins(ins) = &self.lines[i] else {
                    if self.asm_entry(i) {
                        self.kill_at_entry(&mut live);
                    }
                    continue;
                };
                if ins.gone {
                    continue;
                }
                let Some(plan) = ins.plan.clone() else {
                    self.live_step(&mut live, ins);
                    continue;
                };
                // a store keeps the flags: the loads in front of it must
                // not be seen after it
                let flags_free = ins.op != "sta" || live.regs & NZ == 0;
                let mut before: Vec<(&'static str, String)> = Vec::new();
                let mut operand = None;
                let mut direct = None;
                match &plan {
                    Plan::Direct { operand: o } => direct = Some(o.clone()),
                    Plan::Abs { operand: o, idx } => {
                        if live.regs & RX == 0 && flags_free {
                            match idx {
                                Src::A => before.push(("tax", String::new())),
                                Src::Cell(c) => before.push(("ldx", self.cell_name(*c))),
                            }
                            operand = Some(o.clone());
                        }
                    }
                    Plan::Ptr { cell, zp, idx } => {
                        if live.regs & (RX | RY) == 0 && flags_free && !live.cells[*zp] && !live.cells[*zp + 1] {
                            before.push(("ldx", self.cell_name(*cell)));
                            before.push(("stx", self.cell_name(*zp)));
                            before.push(("ldx", self.cell_name(*cell + 1)));
                            before.push(("stx", self.cell_name(*zp + 1)));
                            match idx {
                                Src::Cell(c) => before.push(("ldy", self.cell_name(*c))),
                                _ => before.push(("tay", String::new())),
                            }
                        }
                    }
                }
                let applied = operand.is_some() || direct.is_some() || !before.is_empty();
                let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
                ins.plan = None;
                if let Some(o) = direct {
                    ins.arg = o;
                    ins.mode = Mode::Mem;
                    ins.ptr = None;
                    ins.cell = None;
                }
                if let Some(o) = operand {
                    ins.arg = o;
                    ins.mode = Mode::MemX;
                    ins.ptr = None;
                    ins.cell = None;
                }
                let Line::Ins(ins) = &self.lines[i] else { unreachable!() };
                self.live_step(&mut live, ins);
                if applied {
                    any = true;
                    for (op, arg) in before.iter().rev() {
                        let new = self.make_ins(*op, arg.clone());
                        self.live_step(&mut live, &new);
                    }
                    if !before.is_empty() {
                        inserts.push((i, before));
                    }
                }
            }
        }
        if !any {
            return false;
        }
        // from the end, so the line numbers stay right
        inserts.sort_by(|a, b| b.0.cmp(&a.0));
        for (i, before) in inserts {
            for (op, arg) in before.into_iter().rev() {
                let new = self.make_ins(op, arg);
                self.lines.insert(i, Line::Ins(new));
            }
        }
        // what the forward pass found may not hold any more
        for l in self.lines.iter_mut() {
            if let Line::Ins(ins) = l {
                ins.redundant = None;
                ins.carry = None;
                ins.plan = None;
                ins.step_reg = None;
            }
        }
        true
    }

    fn backward(&mut self, blocks: &[Block]) -> bool {
        let n = blocks.len();
        // Accesses that can be made a better way: first, and with what is
        // live when no load is taken for redundant (a later `ldy #0` that
        // is redundant now is not after the change).
        if self.lines.iter().any(|l| matches!(l, Line::Ins(i) if !i.gone && i.plan.is_some())) {
            self.keep_redundant = true;
            let live_in = self.liveness(blocks);
            let done = self.apply_plans(blocks, &live_in);
            self.keep_redundant = false;
            if done {
                return true;
            }
        }
        let live_in = self.liveness(blocks);
        let out_of = |b: usize, live_in: &[Live], this: &Self| this.live_out(blocks, live_in, b);
        let outs: Vec<Live> = (0..n).map(|b| out_of(b, &live_in, self)).collect();
        if self.merge_copies(blocks, &live_in[0], outs) {
            // the bytes have new names: liveness again
            return true;
        }
        let mut changed = false;
        for b in 0..n {
            let mut live = out_of(b, &live_in, self);
            for i in (blocks[b].start..blocks[b].end).rev() {
                let Line::Ins(ins) = &self.lines[i] else {
                    if self.asm_entry(i) {
                        self.kill_at_entry(&mut live);
                    }
                    continue;
                };
                if ins.gone {
                    continue;
                }
                // what each edge of the block's branch needs
                let edge = |t: Tgt| match t {
                    Tgt::Block(t) => live_in[t].regs,
                    Tgt::External => ALL,
                };
                let edges = match blocks[b].term {
                    Term::Cond(t, f) => Some((edge(t), edge(f))),
                    _ => None,
                };
                if let Some(op) = self.carry_branch(blocks[b].start, i, edges) {
                    // `lda #0` / `rol` / `beq`: branch on the carry itself
                    let mut left = 2;
                    for l in (blocks[b].start..i).rev() {
                        if let Line::Ins(ins) = &mut self.lines[l]
                            && !ins.gone
                        {
                            ins.gone = true;
                            left -= 1;
                            if left == 0 {
                                break;
                            }
                        }
                    }
                    let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
                    ins.op = op;
                    changed = true;
                    live.regs |= FC;
                    continue;
                }
                let prev = (blocks[b].start..i).rev().find(|&l| matches!(&self.lines[l], Line::Ins(p) if !p.gone));
                // A branch that is always taken, behind a load that is
                // only there to set its flag (the upper byte of a tag,
                // which is the constant 0): a jump, without the load.
                if ins.always
                    && is_branch(ins.op)
                    && let Some(l) = prev
                    && let Line::Ins(p) = &self.lines[l]
                    && matches!(p.mode, Mode::Imm | Mode::Imp)
                    && let Some(r) = match p.op {
                        "lda" | "txa" | "tya" => Some(RA),
                        "ldx" | "tax" => Some(RX),
                        "ldy" | "tay" => Some(RY),
                        _ => None,
                    }
                    && live.regs & (r | NZ) == 0
                {
                    let Line::Ins(p) = &mut self.lines[l] else { unreachable!() };
                    p.gone = true;
                    let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
                    ins.op = "jmp";
                    ins.always = false;
                    changed = true;
                    continue;
                }
                // `adc #1` / `tay` of the value Y holds: `iny`
                let prev_ins = prev.and_then(|l| match &self.lines[l] {
                    Line::Ins(p) => Some(p),
                    _ => None,
                });
                let reg = match ins.op {
                    "tax" => RX,
                    "tay" => RY,
                    _ => 0,
                };
                if let Some(p) = prev_ins
                    && let Some((r, carry_same)) = p.step_reg
                    && r == reg
                    && live.regs & (RA | FV) == 0
                    && (carry_same || live.regs & FC == 0)
                {
                    let op = match (p.op, reg) {
                        ("adc", RX) => "inx",
                        ("adc", _) => "iny",
                        (_, RX) => "dex",
                        _ => "dey",
                    };
                    let Line::Ins(p) = &mut self.lines[prev.unwrap()] else { unreachable!() };
                    p.gone = true;
                    let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
                    ins.op = op;
                    ins.redundant = None;
                    changed = true;
                }
                // `lda m` / `tay` with A not needed after it: `ldy m`
                let Line::Ins(ins) = &self.lines[i] else { unreachable!() };
                let prev = (blocks[b].start..i).rev().find(|&l| matches!(&self.lines[l], Line::Ins(p) if !p.gone));
                let load = prev.and_then(|l| match &self.lines[l] {
                    Line::Ins(p) if p.op == "lda" && matches!(p.mode, Mode::Imm | Mode::Mem) => {
                        Some((p.arg.clone(), p.mode, p.cell, p.ival.clone()))
                    }
                    _ => None,
                });
                if matches!(ins.op, "tax" | "tay")
                    && live.regs & RA == 0
                    && let Some((arg, mode, cell, ival)) = load
                {
                    let op = if ins.op == "tax" { "ldx" } else { "ldy" };
                    let Line::Ins(p) = &mut self.lines[prev.unwrap()] else { unreachable!() };
                    p.gone = true;
                    let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
                    ins.op = op;
                    ins.arg = arg;
                    ins.mode = mode;
                    ins.cell = cell;
                    ins.ival = ival;
                    ins.redundant = None;
                    changed = true;
                }
                // `txa` / `sta m` with A not needed after it: `stx m`
                let Line::Ins(ins) = &self.lines[i] else { unreachable!() };
                let prev = (blocks[b].start..i).rev().find(|&l| matches!(&self.lines[l], Line::Ins(p) if !p.gone));
                let from = prev.and_then(|l| match &self.lines[l] {
                    Line::Ins(p) if matches!(p.op, "txa" | "tya") => Some(p.op),
                    _ => None,
                });
                if ins.op == "sta"
                    && ins.mode == Mode::Mem
                    && live.regs & (RA | NZ) == 0
                    && let Some(t) = from
                {
                    let op = if t == "txa" { "stx" } else { "sty" };
                    let Line::Ins(p) = &mut self.lines[prev.unwrap()] else { unreachable!() };
                    p.gone = true;
                    let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
                    ins.op = op;
                    ins.redundant = None;
                    changed = true;
                }
                // `tya` / `cmp m` with A not needed after it: `cpy m`
                let prev = (blocks[b].start..i).rev().find(|&l| matches!(&self.lines[l], Line::Ins(p) if !p.gone));
                let transfer = prev.and_then(|l| match &self.lines[l] {
                    Line::Ins(p) if matches!(p.op, "txa" | "tya") => Some(p.op),
                    _ => None,
                });
                let Line::Ins(ins) = &self.lines[i] else { unreachable!() };
                if ins.op == "cmp"
                    && matches!(ins.mode, Mode::Imm | Mode::Mem)
                    && live.regs & RA == 0
                    && let Some(t) = transfer
                {
                    let op = if t == "txa" { "cpx" } else { "cpy" };
                    let Line::Ins(p) = &mut self.lines[prev.unwrap()] else { unreachable!() };
                    p.gone = true;
                    let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
                    ins.op = op;
                    ins.carry = None;
                    changed = true;
                }
                let Line::Ins(ins) = &self.lines[i] else { unreachable!() };
                // `tax` / `stx d` with X not needed after it: `sta d`
                let reg = match ins.op {
                    "stx" => Some(("tax", RX)),
                    "sty" => Some(("tay", RY)),
                    _ => None,
                };
                if let Some((transfer, reg)) = reg
                    && live.regs & (reg | NZ) == 0
                    && let Some(j) = (blocks[b].start..i)
                        .rev()
                        .find(|&l| matches!(&self.lines[l], Line::Ins(p) if !p.gone))
                    && matches!(&self.lines[j], Line::Ins(p) if p.op == transfer)
                {
                    for (line, op) in [(j, None), (i, Some("sta"))] {
                        let Line::Ins(ins) = &mut self.lines[line] else { unreachable!() };
                        match op {
                            Some(op) => ins.op = op,
                            None => ins.gone = true,
                        }
                    }
                    changed = true;
                }
                let Line::Ins(ins) = &self.lines[i] else { unreachable!() };
                if let Some((k, j, d)) = self.coalesce(blocks[b].start, i, &live) {
                    // `sta t` ... `lda t` / `sta d`: store to d at once
                    let name = self.cell_name(d);
                    for (line, gone) in [(k, false), (j, true), (i, true)] {
                        let Line::Ins(ins) = &mut self.lines[line] else { unreachable!() };
                        if gone {
                            ins.gone = true;
                        } else {
                            ins.arg = name.clone();
                            ins.cell = Some(d);
                            ins.redundant = None;
                        }
                    }
                    changed = true;
                    continue;
                }
                if let Some(k) = self.transfer(blocks[b].start, i, &live) {
                    // `sta t` ... `ldx t`: `tax` in place of the store
                    let op = if ins.op == "ldx" { "tax" } else { "tay" };
                    for (line, gone) in [(k, false), (i, true)] {
                        let Line::Ins(ins) = &mut self.lines[line] else { unreachable!() };
                        if gone {
                            ins.gone = true;
                        } else {
                            ins.op = op;
                            ins.arg = String::new();
                            ins.mode = Mode::Imp;
                            ins.cell = None;
                            ins.redundant = None;
                        }
                    }
                    changed = true;
                    continue;
                }
                // An instruction that is only there for a carry that is known
                // (a comparison whose branch is gone, `lda #0` / `rol`).
                let (_, defs, _, _, pure) = self.effect(ins);
                if let Some(c) = ins.carry
                    && live.regs & defs & !FC == 0
                    && live.regs & FC != 0
                    && pure
                {
                    let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
                    ins.op = if c { "sec" } else { "clc" };
                    ins.arg = String::new();
                    ins.mode = Mode::Imp;
                    ins.cell = None;
                    ins.ival = None;
                    ins.carry = None;
                    ins.redundant = None;
                    changed = true;
                }
                let Line::Ins(ins) = &self.lines[i] else { unreachable!() };
                if self.live_step(&mut live, ins) {
                    let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
                    ins.gone = true;
                    changed = true;
                }
            }
        }
        // When nothing else is left to do: loop counters into X or Y.
        if !changed && !self.early {
            return self.funnel(blocks)
                || self.sink_add(blocks, &live_in)
                || self.in_place(blocks, &live_in)
                || self.hoist(blocks, &live_in)
                || self.hoist_check(blocks, &live_in)
                || self.narrow_test(blocks, &live_in, true)
                || self.promote(blocks, &live_in)
                || self.sink(blocks, &live_in)
                || self.narrow_test(blocks, &live_in, false)
                || self.rotate(blocks, &live_in);
        }
        changed
    }

    /// Whether the blocks `t..=b` are only entered at `t`, from the block
    /// in front of it, by falling through.
    fn enters_at_top(blocks: &[Block], t: usize, b: usize) -> bool {
        for (p, block) in blocks.iter().enumerate() {
            if (t..=b).contains(&p) {
                continue;
            }
            let edges = match block.term {
                Term::Fall(s) => vec![(s, true)],
                Term::Jmp(s) => vec![(s, false)],
                Term::Cond(s, f) => vec![(s, false), (f, true)],
                Term::Ret => Vec::new(),
            };
            for (s, falls) in edges {
                if let Tgt::Block(s) = s
                    && (t..=b).contains(&s)
                    && !(s == t && p + 1 == t && falls)
                {
                    return false;
                }
            }
        }
        true
    }

    /// `end - start` for two 16-bit values (low and high byte each), if
    /// that is known to be 1..=255: both are constants or addresses the
    /// assembler knows, or the end was made by adding a constant to the
    /// start.
    fn span(&self, start: [&Loc; 2], end: [&Loc; 2]) -> Option<u8> {
        let word = |v: [&Loc; 2]| (v[0].vn < 256 && v[1].vn < 256).then(|| v[0].vn as u16 | (v[1].vn as u16) << 8);
        if let (Some(s), Some(e)) = (word(start), word(end)) {
            let d = e.wrapping_sub(s);
            return (1..=255).contains(&d).then_some(d as u8);
        }
        // two addresses that the assembler knows: `X` and `X+k`
        let sym = |v: &Loc| (SYM..ENTRY).contains(&v.vn).then(|| self.syms[(v.vn - SYM) as usize].as_str());
        if let (Some(s0), Some(s1), Some(e0), Some(e1)) = (sym(start[0]), sym(start[1]), sym(end[0]), sym(end[1])) {
            let a = s0.strip_prefix("#<(")?.strip_suffix(')')?;
            let k: u64 = e0.strip_prefix("#<(")?.strip_prefix(a)?.strip_prefix('+')?.strip_suffix(')')?.parse().ok()?;
            let same = s1.strip_prefix("#>(").and_then(|r| r.strip_suffix(')')) == Some(a)
                && e1.strip_prefix("#>(").and_then(|r| r.strip_suffix(')')) == Some(&format!("{a}+{k}")[..]);
            return (same && (1..=255).contains(&k)).then_some(k as u8);
        }
        let (lo, hi) = (self.adds.get(&end[0].vn)?, self.adds.get(&end[1].vn)?);
        let k = if lo.x == start[0].vn && lo.y < 256 {
            lo.y
        } else if lo.y == start[0].vn && lo.x < 256 {
            lo.x
        } else {
            return None;
        };
        let carried = hi.cin == Cin::Of(end[0].vn)
            && ((hi.x == start[1].vn && hi.y == 0) || (hi.y == start[1].vn && hi.x == 0));
        (lo.cin == Cin::Zero && k != 0 && carried).then_some(k as u8)
    }

    /// Whether block `x` of the loop `t..=b` (entered at `t` only) runs
    /// exactly once on every way from `t` back to `t`: no way from `x` to
    /// `x` without `t`, and none from `t` to `t` without `x`.
    fn once_per_round(blocks: &[Block], t: usize, b: usize, x: usize) -> bool {
        let reaches = |from: usize, avoid: usize| {
            let mut seen = vec![false; blocks.len()];
            let mut todo = vec![from];
            let mut first = true;
            while let Some(q) = todo.pop() {
                if !first && q == from {
                    return true;
                }
                if (!first && (q == avoid || seen[q])) || !(t..=b).contains(&q) {
                    continue;
                }
                if !first {
                    seen[q] = true;
                }
                first = false;
                let next = match blocks[q].term {
                    Term::Fall(s) | Term::Jmp(s) => vec![s],
                    Term::Cond(s, f) => vec![s, f],
                    Term::Ret => Vec::new(),
                };
                todo.extend(next.into_iter().filter_map(|s| match s {
                    Tgt::Block(s) => Some(s),
                    Tgt::External => None,
                }));
            }
            false
        };
        x != t && (t..=b).contains(&x) && !reaches(x, t) && !reaches(t, x)
    }

    /// The step of the 16-bit value at frame bytes `c`, `c + 1` in the loop
    /// `t..=b`, if these are its only writes there: `inc c` / `bne next` /
    /// `inc c+1` / `next:`. The block of the first and the three lines.
    fn inc_group(&self, blocks: &[Block], t: usize, b: usize, c: usize) -> Option<(usize, [usize; 3])> {
        if c + 1 >= self.nframe || self.escaped[c] || self.escaped[c + 1] {
            return None;
        }
        let mut writes = Vec::new();
        for i in blocks[t].start..blocks[b].end {
            if let Line::Ins(ins) = &self.lines[i]
                && !ins.gone
                && self.effect(ins).3
                && ins.mode == Mode::Mem
                && self.own(&ins.arg).is_some_and(|w| w == c || w == c + 1)
            {
                writes.push(i);
            }
        }
        let &[w0, w1] = &writes[..] else { return None };
        let code = |q: usize| -> Vec<usize> {
            (blocks[q].start..blocks[q].end)
                .filter(|&i| matches!(&self.lines[i], Line::Ins(ins) if !ins.gone))
                .collect()
        };
        let x = (t..=b).find(|&q| (blocks[q].start..blocks[q].end).contains(&w0))?;
        let cx = code(x);
        let (&[.., last, branch], true) = (&cx[..], x + 2 <= b) else { return None };
        let fits = last == w0
            && self.ins(w0).op == "inc"
            && self.ins(w0).cell == Some(c)
            && self.ins(branch).op == "bne"
            && !self.ins(branch).always
            && code(x + 1) == [w1]
            && self.ins(w1).op == "inc"
            && self.ins(w1).cell == Some(c + 1)
            && matches!(blocks[x].term, Term::Cond(Tgt::Block(l), Tgt::Block(h)) if l == x + 2 && h == x + 1)
            && matches!(blocks[x + 1].term, Term::Fall(Tgt::Block(l)) if l == x + 2);
        fits.then_some((x, [w0, branch, w1]))
    }

    /// Walks an array of bytes with an index in Y in place of a pointer
    /// (for `narrow_test`, which has found the loop `t..=b` with its test
    /// at lines `test`, the step of the pointer at lines `step`, and that
    /// it runs `k` rounds):
    ///
    /// ```text
    ///                                         ldy #0
    /// head:   lda p                   head:   cpy #k
    ///         cmp end                         bne body
    ///         bne body                        jmp exit
    ///         lda p+1                 body:   lda p
    ///         cmp end+1                       sta zp_p0
    ///         beq exit                        lda p+1
    /// body:   lda p                           sta zp_p0+1
    ///         sta zp_p0                       lda (zp_p0),y
    ///         lda p+1                         ...
    ///         sta zp_p0+1                     iny
    ///         ldy #0                          jmp head
    ///         lda (zp_p0),y
    ///         ...
    ///         inc p
    ///         bne next
    ///         inc p+1
    /// next:   jmp head
    /// ```
    ///
    /// The pointer stays at the start of the array, so the copy to the
    /// scratch pointer is the same in every round and goes in front of the
    /// loop (`hoist`), or away where the pointer is in the zero page.
    ///
    /// The other form of the loop copies the pointer at the head, steps it
    /// right away (`p = copy + 1`) and works with the copy. There the copy
    /// is made once, in front of the loop, and Y is stepped at the head: it
    /// starts at 255, and the head is `iny` / `cpy #k`.
    ///
    /// Other pointers that are stepped together with this one (the
    /// destination of a copy loop) stay where they are as well; if one is
    /// needed after the loop, the rounds are added to it at the exit.
    ///
    /// That needs, besides what `narrow_test` has checked: elements of one
    /// byte; the loop uses Y only as the zero of accesses through these
    /// pointers (`Ins::via`), and every other read of a pointer is a copy
    /// to a scratch pointer; neither Y nor the pointer is needed after the
    /// loop, and what the old test and the step left in A and the flags is
    /// not needed (A may be at the start of the body: the low byte of the
    /// pointer, for the copy; it is loaded again).
    #[allow(clippy::too_many_arguments)]
    fn index_loop(
        &mut self,
        blocks: &[Block],
        live_in: &[Live],
        (t, b, body, exit): (usize, usize, usize, usize),
        k: u8,
        test: [usize; 6],
        (p0, q0, prefix): (usize, Option<usize>, &[usize]),
        step: &[usize],
    ) -> bool {
        let [l0, m0, l1, m1, b1, exit_jump] = test;
        let test = [l0, m0, l1, m1, b1];
        // the bytes of the pointer and of its copy; what the accesses use
        let mut ptr = vec![p0, p0 + 1];
        ptr.extend(q0.into_iter().flat_map(|q| [q, q + 1]));
        let base = q0.unwrap_or(p0);
        let inside = |q: usize| (t..=b).contains(&q);
        let succs = |q: usize| -> Vec<Tgt> {
            match blocks[q].term {
                Term::Fall(s) | Term::Jmp(s) => vec![s],
                Term::Cond(s, f) => vec![s, f],
                Term::Ret => Vec::new(),
            }
        };
        // after the loop: no Y, no pointer
        let mut after_loop = vec![false; self.ncells];
        let mut one_exit = true;
        for q in t..=b {
            let out = self.live_out(blocks, live_in, q);
            // (a return between the blocks of the loop leaves it too)
            let ret = matches!(blocks[q].term, Term::Ret).then_some(Tgt::External);
            for s in succs(q).into_iter().chain(ret) {
                let l = match s {
                    Tgt::Block(s) if inside(s) => continue,
                    Tgt::Block(s) => {
                        one_exit &= s == exit && (blocks[q].start..blocks[q].end).contains(&exit_jump);
                        &live_in[s]
                    }
                    Tgt::External => {
                        one_exit = false;
                        &out
                    }
                };
                if l.regs & RY != 0 || ptr.iter().any(|&c| l.cells[c]) {
                    return false;
                }
                for (a, c) in after_loop.iter_mut().zip(&l.cells) {
                    *a |= *c;
                }
            }
        }
        // Other pointers that the loop walks along with this one (`inc a` /
        // `bne` / `inc a+1` directly in front of or behind the step) and
        // uses in the same way stay where they are too, and get the rounds
        // added where the loop ends, if they are needed then: that must be
        // the only way out of the loop.
        let mut others: Vec<(usize, [usize; 3])> = Vec::new();
        if q0.is_none() {
            let x = (t..=b).find(|&q| (blocks[q].start..blocks[q].end).contains(&step[0]));
            let mut seen: Vec<usize> = Vec::new();
            for i in blocks[t].start..blocks[b].end {
                let Line::Ins(ins) = &self.lines[i] else { continue };
                if ins.gone || ins.mode != Mode::IndY || ins.via.contains(&base) {
                    continue;
                }
                let found = ins.via.iter().copied().find(|&a| {
                    others.iter().any(|o| o.0 == a)
                        || (!seen.contains(&a) && !ptr.contains(&a) && !ptr.contains(&(a + 1)) && {
                            seen.push(a);
                            match (self.inc_group(blocks, t, b, a), x) {
                                (Some((xa, lines)), Some(x)) => {
                                    let first = |q: usize| {
                                        (blocks[q].start..blocks[q].end)
                                            .find(|&i| matches!(&self.lines[i], Line::Ins(ins) if !ins.gone))
                                    };
                                    let adjacent = (xa + 2 == x && first(x) == Some(step[0]))
                                        || (x + 2 == xa && first(xa) == Some(lines[0]));
                                    let needed = after_loop[a] || after_loop[a + 1];
                                    let can_add = one_exit
                                        && live_in[exit].regs & FV == 0
                                        && (self.ins(b1).op != "bne"
                                            || !(0..blocks.len()).any(|q| {
                                                q != t + 1
                                                    && succs(q).iter().any(|s| {
                                                        matches!(s, Tgt::Block(j) if (blocks[*j].start..blocks[*j].end).contains(&exit_jump))
                                                    })
                                            }));
                                    if adjacent
                                        && live_in[xa + 2].regs & NZ == 0
                                        && Self::once_per_round(blocks, t, b, xa)
                                        && (!needed || can_add)
                                    {
                                        others.push((a, lines));
                                        true
                                    } else {
                                        false
                                    }
                                }
                                _ => false,
                            }
                        })
                });
                if found.is_none() {
                    return false;
                }
            }
        }
        for &(a, _) in &others {
            ptr.extend([a, a + 1]);
        }
        let bases: Vec<usize> = std::iter::once(base).chain(others.iter().map(|o| o.0)).collect();
        const FLAGS_LEFT: u8 = FN | FZ | FC;
        if live_in[exit].regs & (RA | FLAGS_LEFT) != 0 || live_in[body].regs & FLAGS_LEFT != 0 {
            return false;
        }
        // A at the start of the body: loaded again there, if the body is
        // only reached from the test
        let reload = live_in[body].regs & RA != 0;
        if reload && (0..blocks.len()).any(|q| q != t && q != t + 1 && succs(q).contains(&Tgt::Block(body))) {
            return false;
        }
        let Some(&last) = step.iter().max() else { return false };
        let skip: Vec<usize> =
            test.iter().chain(step).chain(prefix).chain(others.iter().flat_map(|o| &o.1)).copied().collect();
        let mut zeros: Vec<usize> = Vec::new();
        for q in t..=b {
            let code = self.block_code(blocks, live_in, q);
            for (n, &(i, after)) in code.iter().enumerate() {
                // what the step leaves: nothing with the copy (it goes),
                // the flags of `iny` without
                if i == last && after & (if q0.is_some() { RA | FLAGS } else { NZ }) != 0 {
                    return false;
                }
                if skip.contains(&i) {
                    continue;
                }
                let ins = self.ins(i);
                let (uses, defs, ..) = self.effect(ins);
                if ins.mode == Mode::IndY {
                    if !ins.via.iter().any(|c| bases.contains(c)) || ins.via_y != Some(0) || defs & RY != 0 {
                        return false;
                    }
                } else if (uses | defs) & RY != 0 {
                    let zero = ins.op == "ldy" && ins.ival.as_ref().is_some_and(|v| v.vn == 0);
                    if !zero || after & NZ != 0 {
                        return false;
                    }
                    zeros.push(i);
                }
                // a read of the pointer: a copy to a scratch pointer
                let cell = if ins.mode == Mode::Mem { self.own(&ins.arg) } else { None };
                if cell.is_some_and(|c| ptr.contains(&c)) {
                    let (reg, store) = match ins.op {
                        "lda" => (RA, "sta"),
                        "ldx" => (RX, "stx"),
                        _ => return false,
                    };
                    let Some(&(next, after_next)) = code.get(n + 1) else { return false };
                    let next = self.ins(next);
                    if next.op != store
                        || next.mode != Mode::Mem
                        || !self.own(&next.arg).is_some_and(|c| c >= self.nframe)
                        || after_next & (reg | NZ) != 0
                    {
                        return false;
                    }
                }
            }
        }
        // the copy of the pointer, for in front of the loop
        let mut front: Vec<Line> = Vec::new();
        for &i in prefix {
            let (op, arg) = (self.ins(i).op, self.ins(i).arg.clone());
            front.push(Line::Ins(self.make_ins(op, arg)));
        }
        let start = if q0.is_some() { "#$ff" } else { "#$00" };
        let mut load = self.make_ins("ldy", start.into());
        load.ival = Some(self.imm(start));
        front.push(Line::Ins(load));

        // (what is added to the other pointers at the end)
        let mut at_end: Vec<Line> = Vec::new();
        for &(a, _) in &others {
            if !(after_loop[a] || after_loop[a + 1]) {
                continue;
            }
            let skip = crate::asm::fresh_label();
            let (lo, hi, rounds) = (self.cell_name(a), self.cell_name(a + 1), format!("#${k:02x}"));
            let mut add = self.make_ins("adc", rounds.clone());
            add.ival = Some(self.imm(&rounds));
            at_end.extend([
                Line::Ins(self.make_ins("clc", String::new())),
                Line::Ins(self.make_ins("lda", lo.clone())),
                Line::Ins(add),
                Line::Ins(self.make_ins("sta", lo)),
                Line::Ins(self.make_ins("bcc", skip.clone())),
                Line::Ins(self.make_ins("inc", hi)),
                Line::Label(skip),
            ]);
        }
        let other_steps: Vec<usize> = others.iter().flat_map(|o| o.1).collect();
        for i in zeros
            .into_iter()
            .chain([l0, l1, m1])
            .chain(step.iter().copied())
            .chain(prefix.iter().copied())
            .chain(other_steps)
        {
            let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
            ins.gone = true;
        }
        let Line::Ins(ins) = &mut self.lines[b1] else { unreachable!() };
        if ins.op == "bne" {
            ins.gone = true;
        } else {
            ins.op = "jmp";
        }
        let arg = format!("#${k:02x}");
        let mut cmp = self.make_ins("cpy", arg.clone());
        cmp.ival = Some(self.imm(&arg));
        self.lines[m0] = Line::Ins(cmp);
        // the step of Y: where the pointer was stepped, or at the head
        let at = if q0.is_some() { l0 } else { step[0] };
        self.lines[at] = Line::Ins(self.make_ins("iny", String::new()));
        // What is added at the end: behind the loop, where the test now
        // leaves to (the loop stays one that `rotate` takes).
        if !at_end.is_empty() {
            let label = crate::asm::fresh_label();
            let Line::Ins(jump) = &mut self.lines[exit_jump] else { unreachable!() };
            let to = std::mem::replace(&mut jump.arg, label.clone());
            at_end.insert(0, Line::Label(label));
            at_end.push(Line::Ins(self.make_ins("jmp", to)));
        }
        // (the later line first: the lines keep their numbers)
        for l in std::mem::take(&mut at_end).into_iter().rev() {
            self.lines.insert(blocks[b].end, l);
        }
        if reload {
            let at = (blocks[body].start..blocks[body].end)
                .find(|&i| !matches!(&self.lines[i], Line::Label(_)))
                .unwrap_or(blocks[body].end);
            let load = self.make_ins("lda", self.cell_name(p0));
            self.lines.insert(at, Line::Ins(load));
        }
        for l in front.into_iter().rev() {
            self.lines.insert(blocks[t].start, l);
        }
        self.clear_marks();
        true
    }

    /// Makes the test of a loop over a slice of known length an 8-bit one.
    /// The loop walks a pointer up to an end that is the pointer's start
    /// plus a constant below 256, and compares all 16 bits in every round:
    ///
    /// ```text
    /// head:   lda p                   head:   lda p           head:   cpx #n
    ///         cmp end                         cmp end                 bne body
    ///         bne body                        bne body                jmp exit
    ///         lda p+1                         jmp exit
    ///         cmp end+1
    ///         beq exit
    /// body:   ...
    /// ```
    ///
    /// (In front of the test the pointer may be copied, where the body works
    /// with the copy and steps the pointer as copy plus `s`.) On the way the pointer is never more than 255 away from its start,
    /// so it is at the end exactly when the low bytes are equal (middle).
    /// If the loop also counts in X or Y (`enumerate`, after `promote`),
    /// the counter tells the same, without a load (right).
    ///
    /// That needs: at the entry of the loop the end is the pointer plus
    /// `k`, 1..=255 (`span`, with what `forward` knows there: `heads`);
    /// the loop does not write the end and writes the pointer in one place
    /// only, a step by a constant `s` that divides `k`, which runs exactly
    /// once per round (`once_per_round`). Then the pointer is the start
    /// plus `s` times the rounds so far at every test. The counter is a
    /// register with a known value at the entry whose only change in the
    /// loop is one `inx`/`dex`/`iny`/`dey`, also once per round. What the
    /// old test left in A (and, with the counter, in the flags) must not
    /// be needed. The loop is as in `rotate`, which then can take it.
    ///
    /// Better than both is an index in Y in place of the pointer
    /// (`index_loop`); that is tried first, and before `promote` gives the
    /// registers away (`index_only`: nothing else). One loop per call; true
    /// if something changed.
    fn narrow_test(&mut self, blocks: &[Block], live_in: &[Live], index_only: bool) -> bool {
        for b in 0..blocks.len() {
            let Term::Jmp(Tgt::Block(t)) = blocks[b].term else { continue };
            if t == 0 || t + 1 >= b || !Self::enters_at_top(blocks, t, b) {
                continue;
            }
            let Some(st) = self.heads.get(&blocks[t].start).cloned() else { continue };
            let code = |q: usize| -> Vec<usize> {
                (blocks[q].start..blocks[q].end)
                    .filter(|&i| matches!(&self.lines[i], Line::Ins(ins) if !ins.gone))
                    .collect()
            };
            if (blocks[t].start..blocks[b].end).any(|i| self.asm_entry(i)) {
                continue;
            }
            // the test
            // (in front of it the pointer may be copied: the body then
            // works with the copy, and the step is `p = copy + s`)
            let head = code(t);
            let Some(cut) = head.len().checked_sub(3) else { continue };
            let (&[l0, m0, b0], &[l1, m1, b1]) = (&head[cut..], &code(t + 1)[..]) else { continue };
            let copy = match head[..cut] {
                [] => None,
                [a, b, c, d] => {
                    let cell = |i: usize, op: &str| self.ins(i).cell.filter(|_| self.ins(i).op == op);
                    let (Some(from0), Some(q0), Some(from1), Some(q1)) =
                        (cell(a, "lda"), cell(b, "sta"), cell(c, "lda"), cell(d, "sta"))
                    else {
                        continue;
                    };
                    if Some(from0) != self.ins(l0).cell || from1 != from0 + 1 || q1 != q0 + 1 || q1 >= self.nframe {
                        continue;
                    }
                    Some((q0, [b, d]))
                }
                _ => continue,
            };
            let (q0, q1) = match copy {
                Some((q0, _)) => (Some(q0), Some(q0 + 1)),
                None => (None, None),
            };
            let Term::Cond(Tgt::Block(body), Tgt::Block(low_equal)) = blocks[t].term else { continue };
            let Term::Cond(taken, fall) = blocks[t + 1].term else { continue };
            if low_equal != t + 1 || self.ins(b0).op != "bne" || self.ins(b0).always || self.ins(b1).always {
                continue;
            }
            // (the branch of the second half goes to the exit, or over a
            // jump to it)
            let (exit, exit_jump) = match self.ins(b1).op {
                "beq" if fall == Tgt::Block(body) => (taken, b1),
                "bne" if taken == Tgt::Block(body) => {
                    let Tgt::Block(j) = fall else { continue };
                    let (&[jump], Term::Jmp(e)) = (&code(j)[..], blocks[j].term) else { continue };
                    if self.ins(jump).op != "jmp" {
                        continue;
                    }
                    (e, jump)
                }
                _ => continue,
            };
            let Tgt::Block(exit) = exit else { continue };
            if (t..=b).contains(&exit) || !(t..=b).contains(&body) {
                continue;
            }
            let (Some(p0), Some(p1)) = (self.ins(l0).cell, self.ins(l1).cell) else { continue };
            if self.ins(l0).op != "lda" || self.ins(l1).op != "lda" || p1 != p0 + 1 || p1 >= self.nframe {
                continue;
            }
            let end = |i: usize| -> Option<(Option<usize>, Loc)> {
                let ins = self.ins(i);
                match (ins.op, ins.mode, ins.cell) {
                    ("cmp", Mode::Imm, _) => Some((None, ins.ival.clone()?)),
                    ("cmp", Mode::Mem, Some(c))
                        if c < self.nframe && c != p0 && c != p1 && Some(c) != q0 && Some(c) != q1 =>
                    {
                        Some((Some(c), st.l[3 + c].clone()))
                    }
                    _ => None,
                }
            };
            let (Some((e0, end0)), Some((e1, end1))) = (end(m0), end(m1)) else { continue };
            let Some(k) = self.span([&st.l[3 + p0], &st.l[3 + p1]], [&end0, &end1]) else { continue };
            // what the loop writes: not the end, the pointer in one step
            let mut steps: Vec<usize> = Vec::new();
            let mut copy_writes: Vec<usize> = Vec::new();
            let mut fixed = true;
            for i in blocks[t].start..blocks[b].end {
                let Line::Ins(ins) = &self.lines[i] else { continue };
                if ins.gone || !self.effect(ins).3 {
                    continue;
                }
                // (a write to memory that is not followed could be one)
                let c = if ins.mode == Mode::Mem { self.own(&ins.arg) } else { None };
                if c.is_some() && (c == e0 || c == e1) {
                    fixed = false;
                } else if c == Some(p0) || c == Some(p1) {
                    steps.push(i);
                } else if c.is_some() && (c == q0 || c == q1) && !copy.is_some_and(|(_, at)| at.contains(&i)) {
                    copy_writes.push(i);
                }
            }
            let &[w0, w1] = &steps[..] else { continue };
            if !fixed || self.ins(w0).cell != Some(p0) || self.ins(w1).cell != Some(p1) {
                continue;
            }
            let Some(x) = (t..=b).find(|&q| (blocks[q].start..blocks[q].end).contains(&w0)) else { continue };
            let cx = code(x);
            let Some(j) = cx.iter().position(|&i| i == w0) else { continue };
            let at = |j: usize| cx.get(j).map(|&i| self.ins(i));
            // the high byte: stepped behind a branch on the carry of the
            // low byte, in a block of its own
            let carried = |branch: &str| {
                j + 2 == cx.len()
                    && at(j + 1).is_some_and(|i| i.op == branch && !i.always)
                    && x + 2 <= b
                    && code(x + 1) == [w1]
                    && self.ins(w1).op == "inc"
                    && matches!(blocks[x].term, Term::Cond(Tgt::Block(l), Tgt::Block(h)) if l == x + 2 && h == x + 1)
                    && matches!(blocks[x + 1].term, Term::Fall(Tgt::Block(l)) if l == x + 2)
            };
            let konst = |i: &Ins| i.ival.as_ref().filter(|v| v.vn < 256).map(|v| v.vn as u8);
            let (s, group): (u8, Vec<usize>) = match self.ins(w0).op {
                "inc" if carried("bne") => (1, vec![w0, cx[j + 1], w1]),
                "sta" if j >= 2 => {
                    let (Some(load), Some(add)) = (at(j - 2), at(j - 1)) else { continue };
                    let clear = add.add_clear || (j >= 3 && at(j - 3).is_some_and(|i| i.op == "clc"));
                    let high = carried("bcc")
                        || (at(j + 1).is_some_and(|i| {
                            i.op == "lda"
                                && (i.cell == Some(p1) || (i.cell.is_some() && i.cell == q1 && at(j - 2).is_some_and(|l| l.cell == q0)))
                        })
                            && at(j + 2).is_some_and(|i| i.op == "adc" && konst(i) == Some(0))
                            && cx.get(j + 3) == Some(&w1)
                            && self.ins(w1).op == "sta");
                    // from the pointer itself, or from the copy: that
                    // still is the pointer if nothing wrote it since the
                    // head (the step comes first in the round)
                    let from_copy = load.cell.is_some() && load.cell == q0;
                    let copy_holds =
                        copy_writes.is_empty() || (x == body && copy_writes.iter().all(|&i| i > w1));
                    if load.op != "lda"
                        || !(load.cell == Some(p0) || (from_copy && copy_holds))
                        || add.op != "adc"
                        || !clear
                        || !high
                    {
                        continue;
                    }
                    let Some(s) = konst(add) else { continue };
                    let mut group = Vec::new();
                    if j >= 3 && at(j - 3).is_some_and(|i| i.op == "clc") {
                        group.push(cx[j - 3]);
                    }
                    group.extend([cx[j - 2], cx[j - 1], w0]);
                    if carried("bcc") {
                        group.extend([cx[j + 1], w1]);
                    } else {
                        group.extend([cx[j + 1], cx[j + 2], w1]);
                    }
                    (s, group)
                }
                _ => continue,
            };
            if s == 0 || k % s != 0 || !Self::once_per_round(blocks, t, b, x) {
                continue;
            }
            let rounds = k / s;
            // best: an index in Y in place of the pointer
            let in_place = self.ins(w0).op == "inc";
            if s == 1
                && (if copy.is_some() { !in_place && copy_writes.is_empty() } else { in_place })
                && self.index_loop(
                    blocks,
                    live_in,
                    (t, b, body, exit),
                    k,
                    [l0, m0, l1, m1, b1, exit_jump],
                    (p0, q0, &head[..cut]),
                    &group,
                )
            {
                return true;
            }
            if index_only {
                continue;
            }
            // a counter in a register
            const LEFT: u8 = RA | FN | FZ | FC;
            let mut counter = None;
            if (live_in[body].regs | live_in[exit].regs) & LEFT == 0 {
                for (r, reg, up, down, cmp) in [(1, RX, "inx", "dex", "cpx"), (2, RY, "iny", "dey", "cpy")] {
                    let mut defs: Vec<usize> = Vec::new();
                    for i in blocks[t].start..blocks[b].end {
                        if let Line::Ins(ins) = &self.lines[i]
                            && !ins.gone
                            && self.effect(ins).1 & reg != 0
                        {
                            defs.push(i);
                        }
                    }
                    let &[d] = &defs[..] else { continue };
                    let from = &st.l[r];
                    let to = match self.ins(d).op {
                        op if op == up => from.lo.wrapping_add(rounds),
                        op if op == down => from.lo.wrapping_sub(rounds),
                        _ => continue,
                    };
                    let Some(q) = (t..=b).find(|&q| (blocks[q].start..blocks[q].end).contains(&d)) else { continue };
                    if from.lo == from.hi && Self::once_per_round(blocks, t, b, q) {
                        counter = Some((cmp, to));
                        break;
                    }
                }
            }
            if counter.is_none() && live_in[exit].regs & RA != 0 {
                continue;
            }
            let over = self.ins(b1).op == "bne";
            if let Some((cmp, to)) = counter {
                let arg = format!("#${to:02x}");
                let mut new = self.make_ins(cmp, arg.clone());
                new.ival = Some(self.imm(&arg));
                self.lines[m0] = Line::Ins(new);
            }
            for (i, gone) in [(l0, counter.is_some()), (l1, true), (m1, true), (b1, over)] {
                let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
                ins.gone = gone;
                if i == b1 && !over {
                    ins.op = "jmp";
                }
            }
            self.clear_marks();
            return true;
        }
        false
    }

    /// Puts the test of a loop at its end:
    ///
    /// ```text
    /// head:   <test>                          jmp head
    ///         bcs exit                body:   ...
    ///         ...                     head:   <test>
    ///         jmp head                        bcc body
    /// exit:                           exit:
    /// ```
    ///
    /// One jump less in every round. The jump in front of the loop is not
    /// needed either when the first test is known to pass and the body
    /// uses nothing that the test leaves behind (registers, flags).
    ///
    /// For a loop whose first block is the whole test, with the exit
    /// directly after the loop, that is entered at the top only and is
    /// small enough for the branch back. One loop per call; true if
    /// something changed.
    fn rotate(&mut self, blocks: &[Block], live_in: &[Live]) -> bool {
        for b in 0..blocks.len() {
            let Term::Jmp(Tgt::Block(t)) = blocks[b].term else { continue };
            if t == 0 || t >= b || !Self::enters_at_top(blocks, t, b) {
                continue;
            }
            let ins_in = |q: usize| {
                (blocks[q].start..blocks[q].end).filter(|&i| matches!(&self.lines[i], Line::Ins(ins) if !ins.gone))
            };
            // The test ends with a branch to the exit, or (the exit was
            // too far away once) with a branch over a jump to the exit:
            // then the branch stays as it is and the jump goes.
            let Term::Cond(Tgt::Block(taken), Tgt::Block(next)) = blocks[t].term else { continue };
            let (body, over) = if taken == b + 1 && next == t + 1 {
                (t + 1, None)
            } else if taken == t + 2
                && next == t + 1
                && t + 2 <= b
                && matches!(blocks[next].term, Term::Jmp(Tgt::Block(e)) if e == b + 1)
                && ins_in(next).count() == 1
                && !(blocks[next].start..blocks[next].end).any(|i| matches!(&self.lines[i], Line::Label(_)))
            {
                (t + 2, ins_in(next).next())
            } else {
                continue;
            };
            // The head: its label, the test, the branch to the exit.
            let (hs, he) = (blocks[t].start, blocks[t].end);
            let Line::Label(head) = &self.lines[hs] else { continue };
            let head = head.clone();
            if head.starts_with('@') || (hs + 1..he).any(|i| matches!(&self.lines[i], Line::Label(_))) {
                continue;
            }
            let Some(br) = ins_in(t).last() else { continue };
            let Some(jmp) = ins_in(b).last() else { continue };
            let (Line::Ins(branch), Line::Ins(back)) = (&self.lines[br], &self.lines[jmp]) else { continue };
            let Some(inverse) = crate::asm::opposite(branch.op) else { continue };
            if branch.always || branch.arg.starts_with('@') || back.op != "jmp" || back.arg != head {
                continue;
            }
            // the branch back must reach: sizes are upper bounds
            let size: i64 = (t..=b)
                .flat_map(ins_in)
                .map(|i| match &self.lines[i] {
                    Line::Ins(ins) => crate::asm::max_size(ins.op, &ins.arg),
                    _ => 0,
                })
                .sum();
            if size > 110 {
                continue;
            }
            // Without the jump to the test: the first test passes, and
            // the body needs nothing of what the test computes.
            let mut defs = 0u8;
            let mut writes = false;
            for i in ins_in(t) {
                let Line::Ins(ins) = &self.lines[i] else { unreachable!() };
                let (_, d, _, wr, _) = self.effect(ins);
                defs |= d;
                writes |= wr;
            }
            let enter_body = branch.first == Some(over.is_some()) && !writes && live_in[body].regs & defs == 0;

            let label = crate::asm::fresh_label();
            let mut lines: Vec<Line> = std::mem::take(&mut self.lines);
            let mut rest = lines.split_off(blocks[b].end);
            let mut loop_body = lines.split_off(he);
            let mut test = lines.split_off(hs);
            // (line numbers inside the parts)
            loop_body.remove(jmp - he);
            if !enter_body {
                lines.push(Line::Ins(self.make_ins("jmp", head)));
            }
            match over {
                Some(j) => {
                    loop_body.remove(j - he);
                }
                None => {
                    let Line::Ins(branch) = &mut test[br - hs] else { unreachable!() };
                    branch.op = inverse;
                    branch.arg = label.clone();
                    lines.push(Line::Label(label));
                }
            }
            lines.append(&mut loop_body);
            lines.append(&mut test);
            lines.append(&mut rest);
            self.lines = lines;
            for l in self.lines.iter_mut() {
                if let Line::Ins(ins) = l {
                    ins.redundant = None;
                    ins.carry = None;
                    ins.plan = None;
                    ins.step_reg = None;
                    ins.first = None;
                }
            }
            return true;
        }
        false
    }

    /// The instructions of block `b` (their lines) with the registers and
    /// flags that are needed after each.
    fn block_code(&self, blocks: &[Block], live_in: &[Live], b: usize) -> Vec<(usize, u8)> {
        let mut code = Vec::new();
        let mut live = self.live_out(blocks, live_in, b);
        for i in (blocks[b].start..blocks[b].end).rev() {
            if let Line::Ins(ins) = &self.lines[i]
                && !ins.gone
            {
                code.push((i, live.regs));
                self.live_step(&mut live, ins);
            } else if self.asm_entry(i) {
                self.kill_at_entry(&mut live);
            }
        }
        code.reverse();
        code
    }

    fn ins(&self, i: usize) -> &Ins {
        match &self.lines[i] {
            Line::Ins(ins) => ins,
            _ => unreachable!(),
        }
    }

    fn clear_marks(&mut self) {
        for l in self.lines.iter_mut() {
            if let Line::Ins(ins) = l {
                ins.redundant = None;
                ins.carry = None;
                ins.plan = None;
                ins.step_reg = None;
                ins.first = None;
                ins.add_same.clear();
                ins.add_clear = false;
                ins.via.clear();
                ins.via_y = None;
            }
        }
    }

    /// An addition of a constant to one or two frame bytes, `dst = src +
    /// k`, around the `adc` at `code[k]`:
    ///
    /// ```text
    ///         clc             (or the carry is known to be clear)
    ///         lda src         (or A is known to hold src)
    ///         adc #k
    ///         sta dst
    ///         lda src+1       and for two bytes
    ///         adc #0
    ///         sta dst+1
    /// ```
    ///
    /// Each `lda` may be followed by a store of the old value to another
    /// byte (`sta old`: the counter of `enumerate` is copied and stepped
    /// in one go); the load and that store are not part of the addition.
    ///
    /// Returns the first and the last index in `code`, src, dst, the
    /// constants (one per byte) and the indexes in between that are not
    /// part of it. When dst holds the value that is added to (a copy of it
    /// is what src is), src is dst.
    #[allow(clippy::type_complexity)]
    fn add_group(
        &self,
        code: &[(usize, u8)],
        k: usize,
        tracked: bool,
    ) -> Option<(usize, usize, usize, usize, Vec<u8>, Vec<usize>)> {
        let at = |j: usize| code.get(j).map(|&(i, _)| self.ins(i));
        // a byte of the frame: one that is tracked, or (to rewrite the
        // addition where it is) any
        let frame = |ins: &Ins| {
            let c = if tracked || ins.mode != Mode::Mem { ins.cell } else { self.own(&ins.arg) };
            c.filter(|&c| c < self.nframe)
        };
        let konst = |ins: &Ins| ins.ival.as_ref().filter(|v| v.vn < 256).map(|v| v.vn as u8);
        let add = at(k)?;
        if add.op != "adc" {
            return None;
        }
        let k0 = konst(add)?;
        let store = at(k + 1)?;
        let dst = frame(store).filter(|_| store.op == "sta")?;
        let mut keep = Vec::new();
        // `lda c`, or `lda c` / `sta old`, in front of index j: where the
        // load is and the byte it loads
        let load = |j: usize, not: usize| -> Option<(usize, usize, bool)> {
            let l1 = j.checked_sub(1).and_then(at)?;
            if l1.op == "lda" {
                return frame(l1).map(|c| (j - 1, c, false));
            }
            let l2 = j.checked_sub(2).and_then(at)?;
            if l1.op == "sta" && frame(l1).is_some_and(|c| c != not && c != not + 1) && l2.op == "lda" {
                return frame(l2).map(|c| (j - 2, c, true));
            }
            None
        };
        // (the `clc` is in front of the load, or directly in front of the
        // `adc`)
        let is_clc = |j: usize| j.checked_sub(1).and_then(at).is_some_and(|c| c.op == "clc");
        let mut first = k;
        let mut clc = is_clc(first);
        if clc {
            first -= 1;
        }
        let loaded = load(first, dst);
        if let Some((at_load, _, copied)) = loaded {
            first = at_load;
            if copied {
                keep.extend([at_load, at_load + 1]);
            }
        }
        if !clc && is_clc(first) {
            first -= 1;
            clc = true;
        }
        if !clc && !add.add_clear {
            return None;
        }
        let src = if add.add_same.contains(&dst) {
            dst
        } else {
            loaded.map(|l| l.1).or_else(|| add.add_same.first().copied())?
        };
        // the high byte: its `adc` is at k + 3, or at k + 4 behind a copy
        for hi in [k + 3, k + 4] {
            if let (Some(a), Some(st)) = (at(hi), at(hi + 1))
                && a.op == "adc"
                && st.op == "sta"
                && let Some(k1) = konst(a)
                && src + 1 < self.nframe
                && dst + 1 < self.nframe
                && frame(st) == Some(dst + 1)
                && let Some((at_load, c, copied)) = load(hi, dst)
                && at_load == k + 2
                && (c == src + 1 || a.add_same.contains(&(src + 1)))
            {
                if copied {
                    keep.extend([at_load, at_load + 1]);
                }
                return Some((first, hi + 1, src, dst, vec![k0, k1], keep));
            }
        }
        Some((first, k + 1, src, dst, vec![k0], keep))
    }

    /// A loop with several jumps back to its head (`continue`, an `if`
    /// without `else` at the end of the body) gets one: the others go to
    /// the last one, which is a block of its own then. That is where the
    /// step of the loop can go (`sink_add`); if nothing is put there, the
    /// jumps to the jump are straightened again at the end. One loop per
    /// call; true if something changed.
    fn funnel(&mut self, blocks: &[Block]) -> bool {
        let n = blocks.len();
        let last_ins = |b: usize| {
            (blocks[b].start..blocks[b].end).rev().find(|&i| matches!(&self.lines[i], Line::Ins(ins) if !ins.gone))
        };
        for t in 1..n {
            // the blocks from the head on that jump or branch to it
            let back: Vec<usize> = (t..n)
                .filter(|&q| match blocks[q].term {
                    Term::Jmp(Tgt::Block(s)) | Term::Cond(Tgt::Block(s), _) => s == t,
                    _ => false,
                })
                .collect();
            let (Some(&latch), true) = (back.last(), back.len() >= 2) else { continue };
            let Some(at) = last_ins(latch) else { continue };
            let Line::Ins(jump) = &self.lines[at] else { continue };
            if jump.op != "jmp" || jump.mode != Mode::Mem || jump.in_asm || jump.arg.starts_with('@') {
                continue;
            }
            let head = jump.arg.clone();
            // (all of them name the head the same way, outside `asm!`)
            let sources: Vec<usize> = back[..back.len() - 1].iter().filter_map(|&q| last_ins(q)).collect();
            if sources.len() + 1 != back.len()
                || !sources.iter().all(|&i| matches!(&self.lines[i], Line::Ins(ins) if ins.arg == head && !ins.in_asm))
            {
                continue;
            }
            // the jump alone in its block: the label in front of it, or a
            // new one
            let alone = (blocks[latch].start..at).all(|i| match &self.lines[i] {
                Line::Ins(ins) => ins.gone,
                _ => true,
            });
            let label = (blocks[latch].start..at)
                .rev()
                .find_map(|i| match &self.lines[i] {
                    Line::Label(l) if !l.starts_with('@') => Some(l.clone()),
                    _ => None,
                })
                .filter(|_| alone);
            let (label, new) = match label {
                Some(l) => (l, false),
                None => (crate::asm::fresh_label(), true),
            };
            for i in sources {
                let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
                ins.arg = label.clone();
            }
            if new {
                self.lines.insert(at, Line::Label(label));
            }
            self.clear_marks();
            return true;
        }
        false
    }

    /// Moves an addition of a constant (`add_group`: the step of an
    /// iterator's pointer or of a counter) to the end of its loop, in front
    /// of the jump back, when nothing on the way uses the result. Code for
    /// `for x in slice` steps the pointer first and works with a copy of
    /// the old one; with the step at the end the copy and the pointer are
    /// the same on the way, the copy can go (`merge_copies`), and the step
    /// is done in place (`in_place`). One per call; true if something
    /// changed.
    fn sink_add(&mut self, blocks: &[Block], live_in: &[Live]) -> bool {
        const OUT: u8 = RA | FLAGS;
        let n = blocks.len();
        let succs = |b: usize| -> Vec<Tgt> {
            match blocks[b].term {
                Term::Fall(t) | Term::Jmp(t) => vec![t],
                Term::Cond(t, f) => vec![t, f],
                Term::Ret => Vec::new(),
            }
        };
        for x in 0..n {
            // the innermost loop around the block: up to a jump back
            let Some((t, b)) = (x..n)
                .filter_map(|b| match blocks[b].term {
                    Term::Jmp(Tgt::Block(t)) if t <= x => Some((t, b)),
                    _ => None,
                })
                .min_by_key(|&(t, b)| b - t)
            else {
                continue;
            };
            if live_in[t].regs & OUT != 0 {
                continue;
            }
            let code = self.block_code(blocks, live_in, x);
            'group: for k in 0..code.len() {
                let Some((first, last, src, dst, ks, keep)) = self.add_group(&code, k, true) else { continue };
                if code[last].1 & OUT != 0 {
                    continue;
                }
                let width = ks.len();
                let dsts = dst..dst + width;
                let srcs = src..src + width;
                // an instruction in the way: it uses the result, or changes
                // what is added to
                let in_way = |i: usize| match &self.lines[i] {
                    Line::Ins(ins) if !ins.gone => {
                        let (_, _, _, wr, _) = self.effect(ins);
                        ins.cell.is_some_and(|c| dsts.contains(&c) || (wr && srcs.contains(&c)))
                            || ins.ptr.is_some_and(|p| dsts.contains(&p) || dsts.contains(&(p + 1)))
                    }
                    _ => false,
                };
                let jump = code.last().map(|&(i, _)| i);
                if x == b {
                    // already at the end, with nothing but other steps
                    // behind it?
                    let mut j = last + 1;
                    while j + 1 < code.len() {
                        match (j..j + 3).find_map(|k| self.add_group(&code, k, true).filter(|g| g.0 == j)) {
                            Some(g) => j = g.1 + 1,
                            None => break,
                        }
                    }
                    if j + 1 >= code.len() {
                        continue;
                    }
                    if (code[last].0 + 1..blocks[x].end).any(in_way) {
                        continue;
                    }
                } else {
                    if (code[last].0 + 1..blocks[x].end).any(in_way) {
                        continue;
                    }
                    let mut seen = vec![false; n];
                    let mut work = vec![x];
                    let mut reached = false;
                    while let Some(q) = work.pop() {
                        for s in succs(q) {
                            match s {
                                Tgt::External => {
                                    let out = self.live_out(blocks, live_in, q);
                                    if dsts.clone().any(|d| out.cells[d]) {
                                        continue 'group;
                                    }
                                }
                                Tgt::Block(s) if s == b => reached = true,
                                Tgt::Block(s) if s < t || s > b => {
                                    if dsts.clone().any(|d| live_in[s].cells[d]) {
                                        continue 'group;
                                    }
                                }
                                Tgt::Block(s) if s == t || s == x => continue 'group,
                                Tgt::Block(s) => {
                                    if !std::mem::replace(&mut seen[s], true) {
                                        if (blocks[s].start..blocks[s].end).any(in_way) {
                                            continue 'group;
                                        }
                                        work.push(s);
                                    }
                                }
                            }
                        }
                    }
                    if !reached || (blocks[b].start..blocks[b].end).any(in_way) {
                        continue;
                    }
                    // every round of the loop comes through this block
                    if x != t {
                        let mut seen = vec![false; n];
                        let mut work = vec![t];
                        seen[t] = true;
                        seen[x] = true;
                        while let Some(q) = work.pop() {
                            for s in succs(q) {
                                if let Tgt::Block(s) = s
                                    && (t..=b).contains(&s)
                                    && !std::mem::replace(&mut seen[s], true)
                                {
                                    work.push(s);
                                }
                            }
                        }
                        if seen[b] {
                            continue;
                        }
                    }
                }
                let at = if x == b { jump.unwrap() } else { self.block_code(blocks, live_in, b).last().unwrap().0 };
                let mut new = vec![("clc", String::new())];
                for (j, k) in ks.iter().enumerate() {
                    new.push(("lda", self.cell_name(src + j)));
                    new.push(("adc", format!("#${k:02x}")));
                    new.push(("sta", self.cell_name(dst + j)));
                }
                for j in (first..=last).filter(|j| !keep.contains(j)) {
                    let Line::Ins(ins) = &mut self.lines[code[j].0] else { unreachable!() };
                    ins.gone = true;
                }
                for (op, arg) in new.into_iter().rev() {
                    let mut ins = self.make_ins(op, arg);
                    if ins.mode == Mode::Imm {
                        let v = self.imm(&ins.arg.clone());
                        ins.ival = Some(v);
                    }
                    self.lines.insert(at, Line::Ins(ins));
                }
                self.clear_marks();
                return true;
            }
        }
        false
    }

    /// An addition of a constant to a frame byte or to two of them that
    /// puts the sum where the value was, when A and the flags are not
    /// needed after it:
    ///
    /// ```text
    ///         clc                     inc x           or      clc
    ///         lda x                   bne skip                lda x
    ///         adc #1                  inc x+1                 adc #k
    ///         sta x           skip:                           sta x
    ///         lda x+1                                         bcc skip
    ///         adc #0                                          inc x+1
    ///         sta x+1                                 skip:
    /// ```
    ///
    /// One per call; true if something changed.
    fn in_place(&mut self, blocks: &[Block], live_in: &[Live]) -> bool {
        for b in 0..blocks.len() {
            let code = self.block_code(blocks, live_in, b);
            for k in 0..code.len() {
                let Some((first, last, src, dst, ks, keep)) = self.add_group(&code, k, false) else { continue };
                // the value that is added to is that of the byte itself
                if !keep.is_empty() || src != dst {
                    continue;
                }
                let after = code[last].1;
                let line = |j: usize| code[j].0;
                let set = |this: &mut Self, i: usize, op: Option<(&'static str, String)>| {
                    let Line::Ins(ins) = &mut this.lines[i] else { unreachable!() };
                    match op {
                        Some((op, arg)) => {
                            ins.op = op;
                            ins.mode = parse_mode(&arg);
                            ins.cell = None;
                            ins.ival = None;
                            ins.arg = arg;
                        }
                        None => ins.gone = true,
                    }
                };
                if ks.len() == 1 {
                    // (the flags N and Z are those of the sum either way)
                    if ks[0] != 1 || after & (RA | FC | FV) != 0 {
                        continue;
                    }
                    for j in first..=k {
                        set(self, line(j), None);
                    }
                    let name = self.cell_name(dst);
                    set(self, line(k + 1), Some(("inc", name)));
                } else {
                    if ks[1] != 0 || after & (RA | FLAGS) != 0 || self.own(&self.ins(line(k + 2)).arg) != Some(dst + 1) {
                        continue;
                    }
                    let skip = crate::asm::fresh_label();
                    let (lo, hi) = (self.cell_name(dst), self.cell_name(dst + 1));
                    if ks[0] == 1 {
                        for j in first..=k {
                            set(self, line(j), None);
                        }
                        set(self, line(k + 1), Some(("inc", lo)));
                        set(self, line(k + 2), Some(("bne", skip.clone())));
                    } else {
                        set(self, line(k + 2), Some(("bcc", skip.clone())));
                    }
                    set(self, line(k + 3), None);
                    set(self, line(k + 4), Some(("inc", hi)));
                    self.lines.insert(line(k + 4) + 1, Line::Label(skip));
                }
                // the operands that are frame bytes again
                for &(i, _) in &code[first..=last] {
                    let Line::Ins(ins) = &self.lines[i] else { unreachable!() };
                    if !ins.gone && ins.mode == Mode::Mem {
                        let cell = self.own(&ins.arg).filter(|&c| !self.escaped[c]);
                        let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
                        ins.cell = cell;
                    }
                }
                self.clear_marks();
                return true;
            }
        }
        false
    }

    /// How often the loop of blocks `t..=b` writes each tracked byte, and
    /// whether something in it may use the scratch pointers (a call, an
    /// `asm!` block).
    fn loop_writes(&self, blocks: &[Block], t: usize, b: usize) -> (Vec<u32>, bool) {
        let mut writes = vec![0u32; self.ncells];
        let mut calls = false;
        for i in blocks[t].start..blocks[b].end {
            calls |= self.asm_entry(i);
            let Line::Ins(ins) = &self.lines[i] else { continue };
            if ins.gone {
                continue;
            }
            calls |= ins.op == "jsr";
            if let Some(c) = ins.cell
                && self.effect(ins).3
            {
                writes[c] += 1;
            }
        }
        (writes, calls)
    }

    /// Whether an instruction does the same in every round of a loop, given
    /// that the registers and flags it uses do: it computes in the
    /// registers, reads an immediate, a tracked byte that the loop does not
    /// write (`writes`, `calls`: see `loop_writes`) or that the run it
    /// belongs to has stored (`outs`), or data that the program never
    /// changes; or it stores into a tracked byte.
    fn fixed_ins(&self, ins: &Ins, writes: &[u32], calls: bool, outs: &[usize]) -> bool {
        let fixed = match (ins.mode, ins.cell) {
            (Mode::Imm, _) => true,
            (Mode::Mem, Some(c)) => (writes[c] == 0 && !(c >= self.nframe && calls)) || outs.contains(&c),
            (Mode::Mem | Mode::MemX | Mode::MemY, None) => {
                let name = ins.arg.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).next();
                !ins.arg.contains(['$', '(']) && name.is_some_and(|n| (self.is_const)(n))
            }
            _ => false,
        };
        match ins.op {
            "lda" | "ldx" | "ldy" | "adc" | "sbc" | "and" | "ora" | "eor" | "cmp" | "cpx" | "cpy" => fixed,
            "tax" | "tay" | "txa" | "tya" | "clc" | "sec" | "inx" | "dex" | "iny" | "dey" => true,
            "asl" | "lsr" | "rol" | "ror" => ins.mode == Mode::Imp,
            "sta" | "stx" | "sty" => ins.cell.is_some(),
            _ => false,
        }
    }

    /// Moves a check that gives the same result in every round of a loop
    /// (a bounds check of an index that the loop does not change) in front
    /// of the loop:
    ///
    /// ```text
    ///                                         lda i
    ///                                         cmp #8
    ///                                         bcc ok
    /// head:   <test>                          jmp rt_panic
    ///         beq exit                ok:
    ///         lda i                   head:   <test>
    ///         cmp #8                          beq exit
    ///         bcc body                body:   ...
    ///         jmp rt_panic                    jmp head
    /// body:   ...
    ///         jmp head
    /// ```
    ///
    /// That is the same program only if the check runs whenever the loop is
    /// reached: the first test of the loop is known to lead to the check
    /// (`Ins::first`; a loop over a slice whose end is its start plus a
    /// constant), and the test itself does nothing but compare. The check
    /// is a block that only computes (`fixed_ins`, no stores) and ends with
    /// a branch one side of which is a jump to `rt_panic`; nothing after it
    /// needs the registers and flags it leaves. The loop is as in
    /// `promote`. One check per call; true if something changed.
    fn hoist_check(&mut self, blocks: &[Block], live_in: &[Live]) -> bool {
        let n = blocks.len();
        let ins_in = |this: &Self, q: usize| -> Vec<usize> {
            (blocks[q].start..blocks[q].end).filter(|&i| matches!(&this.lines[i], Line::Ins(ins) if !ins.gone)).collect()
        };
        for b in 0..n {
            let Term::Jmp(Tgt::Block(t)) = blocks[b].term else { continue };
            if t == 0 || t > b || !Self::enters_at_top(blocks, t, b) {
                continue;
            }
            // the first test: only comparing, and where it leads to
            let Term::Cond(taken, fall) = blocks[t].term else { continue };
            let head = ins_in(self, t);
            let Some((&br, test)) = head.split_last() else { continue };
            if test.iter().any(|&i| {
                let (_, _, _, wr, pure) = self.effect(self.ins(i));
                wr || !pure
            }) {
                continue;
            }
            let Some(first) = self.ins(br).first else { continue };
            let Tgt::Block(c) = (if first { taken } else { fall }) else { continue };
            if c <= t || c > b {
                continue;
            }
            // the check
            let Term::Cond(ctaken, cfall) = blocks[c].term else { continue };
            let is_panic = |this: &Self, s: Tgt| match s {
                Tgt::Block(s) => {
                    let code = ins_in(this, s);
                    code.len() == 1 && this.ins(code[0]).op == "jmp" && this.ins(code[0]).arg == "rt_panic"
                }
                Tgt::External => false,
            };
            let (go, panic_taken) = match (is_panic(self, ctaken), is_panic(self, cfall)) {
                (true, false) => (cfall, true),
                (false, true) => (ctaken, false),
                _ => continue,
            };
            let Tgt::Block(go) = go else { continue };
            let check = ins_in(self, c);
            let Some((&cbr, run)) = check.split_last() else { continue };
            if (blocks[c].start..blocks[c].end).any(|i| self.asm_entry(i)) {
                continue;
            }
            let (writes, calls) = self.loop_writes(blocks, t, b);
            let mut defined = 0u8;
            let mut fits = !run.is_empty();
            for &i in run {
                let ins = self.ins(i);
                let (uses, defs, _, wr, _) = self.effect(ins);
                fits &= uses & !defined == 0 && !wr && self.fixed_ins(ins, &writes, calls, &[]);
                defined |= defs;
            }
            let branch = self.ins(cbr);
            let (uses, ..) = self.effect(branch);
            let Some(inverse) = crate::asm::opposite(branch.op) else { continue };
            if !fits
                || branch.always
                || uses & !defined != 0
                || live_in[t].regs & defined != 0
                || live_in[go].regs & defined != 0
            {
                continue;
            }
            let passes = if panic_taken { inverse } else { branch.op };
            // in front of the loop: the run, the branch around the panic
            let ok = crate::asm::fresh_label();
            let mut new: Vec<Line> = Vec::new();
            for &i in run {
                let (op, arg) = (self.ins(i).op, self.ins(i).arg.clone());
                let mut ins = self.make_ins(op, arg.clone());
                if ins.mode == Mode::Imm {
                    ins.ival = Some(self.imm(&arg));
                }
                new.push(Line::Ins(ins));
            }
            new.push(Line::Ins(self.make_ins(passes, ok.clone())));
            new.push(Line::Ins(self.make_ins("jmp", "rt_panic".into())));
            new.push(Line::Label(ok));
            // in the loop: nothing, or a jump over the panic
            for &i in run {
                let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
                ins.gone = true;
            }
            let Line::Ins(ins) = &mut self.lines[cbr] else { unreachable!() };
            if panic_taken {
                ins.gone = true;
            } else {
                ins.op = "jmp";
            }
            let at = blocks[t].start;
            for l in new.into_iter().rev() {
                self.lines.insert(at, l);
            }
            self.clear_marks();
            return true;
        }
        false
    }

    /// Moves a computation that gives the same result in every round of a
    /// loop in front of the loop:
    ///
    /// ```text
    ///                                         lda row
    ///                                         asl
    /// head:   ...                             tax
    ///         lda row                         lda table,x
    ///         asl                             sta ptr
    ///         tax                             lda table+1,x
    ///         lda table,x                     sta ptr+1
    ///         sta ptr                 head:   ...
    ///         lda table+1,x                   lda (ptr),y
    ///         sta ptr+1                       ...
    ///         lda (ptr),y                     jmp head
    ///         ...
    ///         jmp head
    /// ```
    ///
    /// The computation is a run of instructions that
    /// * starts with a load and uses no register or flag from before it,
    /// * reads only immediates, frame bytes that the loop does not write
    ///   and data that the program never changes,
    /// * stores into tracked bytes (of the frame, or a scratch pointer)
    ///   that nothing else in the loop writes, that are not needed when
    ///   the loop is entered (so every use in the loop comes after the
    ///   run) and not after the loop,
    /// * leaves nothing in the registers and flags that is needed after
    ///   it, and changes none that is needed when the loop is entered.
    ///
    /// It has no effect besides its stores, so it may also run when the
    /// loop does not. The loop is as in `promote`. One run per call; true
    /// if something changed.
    fn hoist(&mut self, blocks: &[Block], live_in: &[Live]) -> bool {
        let n = blocks.len();
        let succs = |b: usize| -> Vec<Tgt> {
            match blocks[b].term {
                Term::Fall(t) | Term::Jmp(t) => vec![t],
                Term::Cond(t, f) => vec![t, f],
                Term::Ret => Vec::new(),
            }
        };
        let mut loops: Vec<(usize, usize)> = (0..n)
            .filter_map(|b| match blocks[b].term {
                Term::Jmp(Tgt::Block(t)) if t > 0 && t <= b => Some((t, b)),
                _ => None,
            })
            .collect();
        loops.sort_by_key(|&(t, b)| b - t);
        for (t, b) in loops {
            if !Self::enters_at_top(blocks, t, b) {
                continue;
            }
            let (writes, calls) = self.loop_writes(blocks, t, b);
            // what is needed where the loop is left
            let mut exit = vec![false; self.ncells];
            for q in t..=b {
                // (a return between the blocks of the loop leaves it too)
                let ret = matches!(blocks[q].term, Term::Ret).then_some(Tgt::External);
                for s in succs(q).into_iter().chain(ret) {
                    let out;
                    let l = match s {
                        Tgt::Block(s) if (t..=b).contains(&s) => continue,
                        Tgt::Block(s) => &live_in[s],
                        Tgt::External => {
                            out = self.live_out(blocks, live_in, q);
                            &out
                        }
                    };
                    for (e, c) in exit.iter_mut().zip(&l.cells) {
                        *e |= *c;
                    }
                }
            }
            for q in t..=b {
                let code = self.block_code(blocks, live_in, q);
                for k in 0..code.len() {
                    if self.ins(code[k].0).op != "lda" {
                        continue;
                    }
                    // the run: as far as it stays the same in every round
                    let mut defined = 0u8;
                    let mut outs: Vec<usize> = Vec::new();
                    let mut end = None;
                    for (j, &(i, _)) in code.iter().enumerate().skip(k) {
                        let ins = self.ins(i);
                        let (uses, defs, ..) = self.effect(ins);
                        if uses & !defined != 0 {
                            break;
                        }
                        if !self.fixed_ins(ins, &writes, calls, &outs) {
                            break;
                        }
                        defined |= defs;
                        if let ("sta" | "stx" | "sty", Some(c)) = (ins.op, ins.cell) {
                            outs.push(c);
                            end = Some(j);
                        }
                    }
                    let Some(end) = end else { continue };
                    if end - k < 2 {
                        continue;
                    }
                    let run = &code[k..=end];
                    let mut clobbers = 0u8;
                    let mut stores = vec![0u32; self.ncells];
                    for &(i, _) in run {
                        let ins = self.ins(i);
                        clobbers |= self.effect(ins).1;
                        if let ("sta" | "stx" | "sty", Some(c)) = (ins.op, ins.cell) {
                            stores[c] += 1;
                        }
                    }
                    let outs_ok = outs.iter().all(|&c| {
                        writes[c] == stores[c]
                            && !live_in[t].cells[c]
                            && !exit[c]
                            && !(c >= self.nframe && calls)
                    });
                    if !outs_ok || code[end].1 & clobbers != 0 || live_in[t].regs & clobbers != 0 {
                        continue;
                    }
                    let mut moved = Vec::new();
                    for &(i, _) in run.iter().rev() {
                        moved.push(self.lines.remove(i));
                    }
                    let at = blocks[t].start;
                    for l in moved {
                        self.lines.insert(at, l);
                    }
                    self.clear_marks();
                    return true;
                }
            }
        }
        false
    }

    /// Moves an `inx`, `iny`, `dex` or `dey` down to the end of its block,
    /// over instructions that do not use the register, when the flags it
    /// sets are needed neither where it is nor where it goes. One per call;
    /// true if something changed.
    fn sink(&mut self, blocks: &[Block], live_in: &[Live]) -> bool {
        for b in 0..blocks.len() {
            // the instructions of the block and what is needed after each
            let mut code: Vec<(usize, u8)> = Vec::new();
            let mut live = self.live_out(blocks, live_in, b);
            let mut entry = false;
            for i in (blocks[b].start..blocks[b].end).rev() {
                entry |= self.asm_entry(i);
                if let Line::Ins(ins) = &self.lines[i]
                    && !ins.gone
                {
                    code.push((i, live.regs));
                    self.live_step(&mut live, ins);
                }
            }
            if entry {
                continue;
            }
            code.reverse();
            for (k, &(i, after)) in code.iter().enumerate() {
                let Line::Ins(ins) = &self.lines[i] else { unreachable!() };
                let reg = match ins.op {
                    "inx" | "dex" => RX,
                    "iny" | "dey" => RY,
                    _ => continue,
                };
                if after & NZ != 0 {
                    continue;
                }
                // the last instruction it can go behind
                let mut to = None;
                for &(j, after_j) in &code[k + 1..] {
                    let Line::Ins(other) = &self.lines[j] else { unreachable!() };
                    let (uses, defs, ..) = self.effect(other);
                    if (uses | defs) & reg != 0 || is_branch(other.op) || matches!(other.op, "jmp" | "rts" | "rti") {
                        break;
                    }
                    if after_j & NZ == 0 {
                        to = Some(j);
                    }
                }
                // only to the end of the block: in front of its jump or
                // branch, or as its last instruction
                let last = code.iter().rev().map(|&(j, _)| j).find(|&j| {
                    !matches!(&self.lines[j], Line::Ins(o) if is_branch(o.op) || matches!(o.op, "jmp" | "rts" | "rti"))
                });
                if let Some(to) = to
                    && Some(to) == last
                {
                    let line = self.lines.remove(i);
                    self.lines.insert(to, line);
                    for l in self.lines.iter_mut() {
                        if let Line::Ins(ins) = l {
                            ins.redundant = None;
                            ins.carry = None;
                            ins.plan = None;
                            ins.step_reg = None;
                        }
                    }
                    return true;
                }
            }
        }
        false
    }

    /// Keeps a frame byte in X or Y for the time of a loop: a byte that
    /// the loop only loads into A, stores from A, increments or decrements
    /// (a counter), when the loop does not use that register and neither
    /// the byte nor the register is needed after it. `lda c` becomes `tya`,
    /// `sta c` becomes `tay`, `inc c` becomes `iny`; the byte is loaded
    /// into the register in front of the loop.
    ///
    /// The code of a `for` loop then is `tya` / ... / `sta old` / `iny` /
    /// `lda old`: the counter is stepped first, and the body works with a
    /// copy of the old value. `sink` moves the `iny` to the end of the
    /// block; the next round finds that Y holds the copy and drops it.
    ///
    /// A loop is taken as the blocks from the target of a jump back to the
    /// jump, when nothing jumps into them from outside and the first one
    /// is entered from the block in front of it by falling through (there
    /// the load goes). One byte per call; true if something changed.
    fn promote(&mut self, blocks: &[Block], live_in: &[Live]) -> bool {
        let n = blocks.len();
        let succs = |b: usize| -> Vec<Tgt> {
            match blocks[b].term {
                Term::Fall(t) | Term::Jmp(t) => vec![t],
                Term::Cond(t, f) => vec![t, f],
                Term::Ret => Vec::new(),
            }
        };
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        for b in 0..n {
            for t in succs(b) {
                if let Tgt::Block(t) = t
                    && t <= b
                    && t > 0
                {
                    ranges.push((t, b));
                }
            }
        }
        ranges.sort_by_key(|&(t, b)| b - t);
        'range: for (t, b) in ranges {
            let inside = |q: usize| (t..=b).contains(&q);
            for p in (0..n).filter(|&p| !inside(p)) {
                for (k, s) in succs(p).into_iter().enumerate() {
                    let Tgt::Block(s) = s else { continue };
                    if !inside(s) {
                        continue;
                    }
                    let falls = match blocks[p].term {
                        Term::Fall(_) => true,
                        Term::Cond(..) => k == 1,
                        _ => false,
                    };
                    if !(s == t && p + 1 == t && falls) {
                        continue 'range;
                    }
                }
            }
            // What the loop does with the index registers and the bytes.
            let mut used = 0u8;
            let mut accesses: HashMap<usize, Vec<usize>> = HashMap::new();
            let mut unfit: HashSet<usize> = HashSet::new();
            // the byte the instruction before loaded into A
            let mut loaded = None;
            for i in blocks[t].start..blocks[b].end {
                if self.asm_entry(i) {
                    continue 'range;
                }
                let Line::Ins(ins) = &self.lines[i] else { continue };
                if ins.gone {
                    continue;
                }
                // A byte of a pointer that is dereferenced stays in memory:
                // in the zero page the pointer is used where it is.
                if ins.op == "sta"
                    && ins.cell.is_some_and(|c| c >= self.nframe)
                    && let Some(c) = loaded
                {
                    unfit.insert(c);
                }
                loaded = if ins.op == "lda" { ins.cell } else { None };
                let (uses, defs, ..) = self.effect(ins);
                used |= (uses | defs) & (RX | RY);
                if let Some(c) = ins.cell {
                    if c < self.nframe && matches!(ins.op, "lda" | "sta" | "inc" | "dec") {
                        accesses.entry(c).or_default().push(i);
                    } else {
                        unfit.insert(c);
                    }
                }
            }
            // The registers and flags needed after each instruction, and
            // what is needed where the loop is left.
            let mut after: HashMap<usize, u8> = HashMap::new();
            let mut exit = Live { regs: 0, cells: vec![false; self.ncells] };
            for q in t..=b {
                let out = self.live_out(blocks, live_in, q);
                // (a return between the blocks of the loop leaves it too)
                let ret = matches!(blocks[q].term, Term::Ret).then_some(Tgt::External);
                for s in succs(q).into_iter().chain(ret) {
                    let l = match s {
                        Tgt::Block(s) if inside(s) => continue,
                        Tgt::Block(s) => &live_in[s],
                        Tgt::External => &out,
                    };
                    exit.regs |= l.regs;
                    for (e, c) in exit.cells.iter_mut().zip(&l.cells) {
                        *e |= *c;
                    }
                }
                let mut live = out;
                for i in (blocks[q].start..blocks[q].end).rev() {
                    if let Line::Ins(ins) = &self.lines[i]
                        && !ins.gone
                    {
                        after.insert(i, live.regs);
                        self.live_step(&mut live, ins);
                    }
                }
            }
            for (reg, load, ops) in
                [(RY, "ldy", ["tya", "tay", "iny", "dey"]), (RX, "ldx", ["txa", "tax", "inx", "dex"])]
            {
                if (used | exit.regs) & reg != 0 {
                    continue;
                }
                let fits = |c: usize, at: &Vec<usize>| {
                    at.len() >= 2
                        && !unfit.contains(&c)
                        && !exit.cells[c]
                        && !(live_in[t].cells[c] && live_in[t].regs & NZ != 0)
                        && at.iter().all(|i| match &self.lines[*i] {
                            // the transfer sets the flags, the store did not
                            Line::Ins(ins) if ins.op == "sta" => after[i] & NZ == 0,
                            _ => true,
                        })
                };
                let best = accesses
                    .iter()
                    .filter(|(c, at)| fits(**c, at))
                    // first a byte that lives from one round to the next
                    // (the counter itself, not a copy of it)
                    .max_by_key(|(c, at)| (live_in[t].cells[**c], at.len(), usize::MAX - **c))
                    .map(|(c, at)| (*c, at.clone()));
                let Some((c, at)) = best else { continue };
                for i in at {
                    let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
                    ins.op = match ins.op {
                        "lda" => ops[0],
                        "sta" => ops[1],
                        "inc" => ops[2],
                        _ => ops[3],
                    };
                    ins.arg = String::new();
                    ins.mode = Mode::Imp;
                    ins.cell = None;
                }
                if live_in[t].cells[c] {
                    let new = self.make_ins(load, self.cell_name(c));
                    self.lines.insert(blocks[t].start, Line::Ins(new));
                }
                for l in self.lines.iter_mut() {
                    if let Line::Ins(ins) = l {
                        ins.redundant = None;
                        ins.carry = None;
                        ins.plan = None;
                        ins.step_reg = None;
                    }
                }
                return true;
            }
        }
        false
    }

    /// A load `ldx t` or `ldy t` (at line `i`, `live` after it) of a byte
    /// that was stored from A earlier in the same straight-line code and is
    /// only read here, where the index register is not used in between and
    /// the flags are needed neither after the store nor after the load: the
    /// line of the store, which can be the transfer.
    fn transfer(&self, start: usize, i: usize, live: &Live) -> Option<usize> {
        let ins_at = |l: usize| match &self.lines[l] {
            Line::Ins(ins) if !ins.gone => Some(ins),
            _ => None,
        };
        let load = ins_at(i)?;
        let t = load.cell.filter(|&t| t < self.nframe)?;
        let reg = match load.op {
            "ldx" => RX,
            "ldy" => RY,
            _ => return None,
        };
        if live.cells[t] || live.regs & NZ != 0 {
            return None;
        }
        let mut k = None;
        for l in (start..i).rev() {
            let Some(ins) = ins_at(l) else { continue };
            if ins.cell == Some(t) {
                if ins.op == "sta" {
                    k = Some(l);
                }
                break;
            }
            let (uses, defs, ..) = self.effect(ins);
            if (uses | defs) & reg != 0 {
                return None;
            }
        }
        let k = k?;
        // the transfer sets N and Z where the store did not
        for l in k + 1..i {
            let Some(ins) = ins_at(l) else { continue };
            let (uses, defs, ..) = self.effect(ins);
            if uses & NZ != 0 {
                return None;
            }
            if defs & NZ == NZ {
                break;
            }
        }
        Some(k)
    }

    /// A branch on a carry that was made a number first (`lda #0` / `rol` /
    /// `beq` or `bne`, the result of a checked addition): the branch on
    /// the carry, if A and the flags are not needed after it. `edges` is
    /// what is live where the branch is taken and where it is not. (The
    /// carry was clear after the `rol`; with the branch on the carry it is
    /// set on one of the edges.)
    fn carry_branch(&self, start: usize, i: usize, edges: Option<(u8, u8)>) -> Option<&'static str> {
        let Line::Ins(br) = &self.lines[i] else { return None };
        let (taken, fall) = edges?;
        let (op, set) = match br.op {
            "beq" => ("bcc", fall),
            "bne" => ("bcs", taken),
            _ => return None,
        };
        if br.always || (taken | fall) & (RA | NZ) != 0 || set & FC != 0 {
            return None;
        }
        let mut prev = (start..i).rev().filter_map(|l| match &self.lines[l] {
            Line::Ins(ins) if !ins.gone => Some(ins),
            _ => None,
        });
        let rol = prev.next()?;
        let lda = prev.next()?;
        let zero = lda.op == "lda" && lda.ival.as_ref().is_some_and(|v| v.vn == 0);
        (rol.op == "rol" && rol.mode == Mode::Imp && zero).then_some(op)
    }

    /// Makes one byte of two tracked bytes that a copy (`lda s` / `sta d`)
    /// connects and that never hold different values that are both still
    /// needed: every use of one is renamed to the other, and the copy then
    /// stores a byte into itself, which the next round removes.
    ///
    /// Two bytes interfere when one is written while the other is live,
    /// unless that write is the copy between them. The result and the
    /// arguments keep their places (the callers use them); two of them are
    /// never merged. `entry` is what is live at the start of the function,
    /// `outs` at the end of each block.
    fn merge_copies(&mut self, blocks: &[Block], entry: &Live, outs: Vec<Live>) -> bool {
        let n = self.ncells;
        if n == 0 || n > 1024 {
            return false;
        }
        let mut interferes = vec![false; n * n];
        let mut copies: Vec<(usize, usize)> = Vec::new();
        // what is live at the entry came from outside: all different
        let at_entry: Vec<usize> = (0..n).filter(|&c| entry.cells[c]).collect();
        for &a in &at_entry {
            for &b in &at_entry {
                if a != b {
                    interferes[a * n + b] = true;
                }
            }
        }
        for (b, mut live) in outs.into_iter().enumerate() {
            let code: Vec<usize> = (blocks[b].start..blocks[b].end)
                .filter(|&i| matches!(&self.lines[i], Line::Ins(ins) if !ins.gone))
                .collect();
            for (k, &i) in code.iter().enumerate().rev() {
                let Line::Ins(ins) = &self.lines[i] else { unreachable!() };
                let (_, _, _, wr, _) = self.effect(ins);
                if wr && let Some(d) = ins.cell {
                    // the byte this store copies, if it is a copy
                    let from = (ins.op == "sta" && k > 0)
                        .then(|| match &self.lines[code[k - 1]] {
                            Line::Ins(p) if p.op == "lda" => p.cell,
                            _ => None,
                        })
                        .flatten();
                    if let Some(s) = from
                        && s != d
                    {
                        copies.push((d, s));
                    }
                    for c in 0..n {
                        if live.cells[c] && c != d && Some(c) != from {
                            interferes[d * n + c] = true;
                            interferes[c * n + d] = true;
                        }
                    }
                }
                self.live_step(&mut live, ins);
            }
        }
        // The byte each byte has become.
        let mut now: Vec<usize> = (0..n).collect();
        let mut merged = false;
        for (d, s) in copies {
            let (d, s) = (now[d], now[s]);
            if d >= self.nframe || s >= self.nframe {
                continue;
            }
            if d == s || interferes[d * n + s] || (d < self.pinned && s < self.pinned) {
                continue;
            }
            // the one that stays: a pinned byte, else the lower one
            let (keep, go) = if d < self.pinned || (s >= self.pinned && d < s) { (d, s) } else { (s, d) };
            for c in 0..n {
                if interferes[go * n + c] {
                    interferes[keep * n + c] = true;
                    interferes[c * n + keep] = true;
                }
                if now[c] == go {
                    now[c] = keep;
                }
            }
            merged = true;
        }
        if !merged {
            return false;
        }
        for i in 0..self.lines.len() {
            let Line::Ins(ins) = &self.lines[i] else { continue };
            let Some(c) = ins.cell else { continue };
            if now[c] != c {
                let name = self.cell_name(now[c]);
                let Line::Ins(ins) = &mut self.lines[i] else { unreachable!() };
                ins.arg = name;
                ins.cell = Some(now[c]);
                ins.redundant = None;
            }
        }
        true
    }

    /// A copy `lda t` / `sta d` (or through X or Y; the store at line `i`,
    /// `live` after it) of a byte `t` that was stored earlier in the same straight-line code
    /// and is not needed afterwards, where nothing in between uses `t` or
    /// `d` and A and its flags are not needed after the copy: the line of
    /// the store to `t`, the line of the load, and `d`.
    fn coalesce(&self, start: usize, i: usize, live: &Live) -> Option<(usize, usize, usize)> {
        let ins_at = |l: usize| match &self.lines[l] {
            Line::Ins(ins) if !ins.gone => Some(ins),
            _ => None,
        };
        let store = ins_at(i)?;
        let d = store.cell?;
        let (load_op, reg) = match store.op {
            "sta" => ("lda", RA),
            "stx" => ("ldx", RX),
            "sty" => ("ldy", RY),
            _ => return None,
        };
        if !live.cells[d] || live.regs & (reg | NZ) != 0 {
            return None;
        }
        let j = (start..i).rev().find(|&l| ins_at(l).is_some())?;
        let load = ins_at(j)?;
        let t = load.cell?;
        if t >= self.nframe || load.op != load_op || t == d || live.cells[t] {
            return None;
        }
        // A scratch pointer is set up earlier this way: nothing in between
        // may use or change it.
        let scratch = d >= self.nframe;
        for k in (start..j).rev() {
            let Some(ins) = ins_at(k) else {
                if scratch && self.asm_entry(k) {
                    return None;
                }
                continue;
            };
            if ins.cell == Some(t) {
                return matches!(ins.op, "sta" | "stx" | "sty").then_some((k, j, d));
            }
            if ins.cell == Some(d) {
                return None;
            }
            if scratch && (ins.op == "jsr" || ins.ptr.is_some_and(|p| p == d || p + 1 == d)) {
                return None;
            }
        }
        None
    }

    fn emit(&self) -> String {
        let mut out = String::new();
        for l in &self.lines {
            match l {
                Line::Label(n) => {
                    let _ = writeln!(out, "{n}:");
                }
                Line::Ins(i) if i.gone => {}
                Line::Ins(i) if i.arg.is_empty() => {
                    let _ = writeln!(out, "        {}", i.op);
                }
                Line::Ins(i) => {
                    let _ = writeln!(out, "        {} {}", i.op, i.arg);
                }
                Line::Other(s) => {
                    out.push_str(s);
                    out.push('\n');
                }
            }
        }
        out
    }
}
