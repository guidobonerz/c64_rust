//! Text output for rat assembly and the memory operands of the generated code.

use std::fmt::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Label numbers are unique across all buffers (code, data, wrappers).
static LABELS: AtomicUsize = AtomicUsize::new(0);

/// The zero-page pointer registers used for indirect accesses
/// (`lda (zp),y`). Each operation uses at most three of them: two sources
/// and the destination.
pub const ZP: [&str; 3] = ["zp_p0", "zp_p1", "zp_p2"];

/// A memory location.
#[derive(Clone, Debug)]
pub enum Addr {
    /// An address known when assembling: a frame slot, static data or a
    /// constant address such as `$d020`. The string is a rat expression.
    Abs(String),
    /// Memory behind a pointer: the two-byte pointer value is stored at the
    /// absolute address `ptr`, the location is `*ptr + off`.
    Ind { ptr: String, off: u64 },
    /// An element of a small table at a known address: `base + off + X`,
    /// where X is loaded from the byte at `x` (an index already multiplied
    /// by the element size; the table is at most 256 bytes).
    Idx { base: String, x: String, off: u64 },
}

impl Addr {
    pub fn offset(&self, n: u64) -> Addr {
        match self {
            Addr::Abs(e) if n == 0 => Addr::Abs(e.clone()),
            Addr::Abs(e) => Addr::Abs(format!("{e}+{n}")),
            Addr::Ind { ptr, off } => Addr::Ind { ptr: ptr.clone(), off: off + n },
            Addr::Idx { base, x, off } => {
                Addr::Idx { base: base.clone(), x: x.clone(), off: off + n }
            }
        }
    }
}

/// A value: bytes in memory, or one operand per byte: an immediate
/// (`#$05`, `#<label`) or an absolute address (a view of another value, e.g.
/// the low byte of a local followed by `#$00` for a zero-extension).
#[derive(Clone, Debug)]
pub enum Val {
    Mem(Addr),
    Imm(Vec<String>),
}

impl Val {
    pub fn imm_bytes(bytes: &[u8]) -> Val {
        Val::Imm(bytes.iter().map(|b| format!("#${b:02x}")).collect())
    }

    pub fn imm_u64(v: u64, size: u64) -> Val {
        let bytes: Vec<u8> = (0..size).map(|i| (v >> (8 * i)) as u8).collect();
        Val::imm_bytes(&bytes)
    }

    pub fn offset(&self, n: u64) -> Val {
        match self {
            Val::Mem(a) => Val::Mem(a.offset(n)),
            Val::Imm(b) => Val::Imm(b.iter().skip(n as usize).cloned().collect()),
        }
    }

    /// The operand of byte `i` if it needs no setup: an immediate or an
    /// absolute address.
    pub fn byte(&self, i: u64) -> Option<String> {
        match self {
            Val::Imm(b) => Some(b.get(i as usize).cloned().unwrap_or_else(|| "#$00".into())),
            Val::Mem(Addr::Abs(e)) => Some(if i == 0 { e.clone() } else { format!("{e}+{i}") }),
            Val::Mem(_) => None,
        }
    }

    /// Whether byte `i` is the constant `v`.
    pub fn byte_is(&self, i: u64, v: u8) -> bool {
        match self {
            Val::Imm(b) => match b.get(i as usize) {
                Some(s) => s
                    .strip_prefix("#$")
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                    .is_some_and(|x| x == v),
                None => v == 0,
            },
            Val::Mem(_) => false,
        }
    }
}

/// An assembly text buffer.
#[derive(Default)]
pub struct Asm {
    pub text: String,
}

impl Asm {
    pub fn ins(&mut self, s: impl AsRef<str>) {
        let _ = writeln!(self.text, "        {}", s.as_ref());
    }

    pub fn label(&mut self, l: &str) {
        let _ = writeln!(self.text, "{l}:");
    }

    pub fn comment(&mut self, s: impl AsRef<str>) {
        for line in s.as_ref().lines() {
            let _ = writeln!(self.text, "; {line}");
        }
    }

    pub fn raw(&mut self, s: impl AsRef<str>) {
        let _ = writeln!(self.text, "{}", s.as_ref());
    }

    /// A new unique label.
    pub fn new_label(&mut self) -> String {
        format!("_L{}", LABELS.fetch_add(1, Ordering::Relaxed) + 1)
    }

    /// Prepares the pointer register `zp` for accesses to `a` (only
    /// indirect locations need it).
    pub fn setup(&mut self, a: &Addr, zp: usize) {
        if let Addr::Ind { ptr, .. } = a {
            let z = ZP[zp];
            self.ins(format!("lda {ptr}"));
            self.ins(format!("sta {z}"));
            self.ins(format!("lda {ptr}+1"));
            self.ins(format!("sta {z}+1"));
        }
    }

    pub fn setup_val(&mut self, v: &Val, zp: usize) {
        if let Val::Mem(a) = v {
            self.setup(a, zp);
        }
    }

    /// `op` (lda, adc, sbc, and, ora, eor, cmp) with byte `i` of `v`. An
    /// indirect `v` must have been prepared with [`Asm::setup`] on `zp`.
    pub fn op_byte(&mut self, op: &str, v: &Val, i: u64, zp: usize) {
        match v {
            Val::Imm(b) => {
                let byte = b.get(i as usize).cloned().unwrap_or_else(|| "#0".into());
                self.ins(format!("{op} {byte}"));
            }
            Val::Mem(Addr::Abs(e)) => {
                if i == 0 {
                    self.ins(format!("{op} {e}"));
                } else {
                    self.ins(format!("{op} {e}+{i}"));
                }
            }
            Val::Mem(Addr::Ind { off, .. }) => {
                self.ins(format!("ldy #{}", off + i));
                self.ins(format!("{op} ({}),y", ZP[zp]));
            }
            Val::Mem(Addr::Idx { base, x, off }) => {
                self.ins(format!("ldx {x}"));
                self.ins(format!("{op} {base}+{},x", off + i));
            }
        }
    }

    pub fn lda(&mut self, v: &Val, i: u64, zp: usize) {
        self.op_byte("lda", v, i, zp);
    }

    /// `sta` (or another store/read-modify-write op) to byte `i` of `a`.
    pub fn sta(&mut self, a: &Addr, i: u64, zp: usize) {
        match a {
            Addr::Abs(e) => {
                if i == 0 {
                    self.ins(format!("sta {e}"));
                } else {
                    self.ins(format!("sta {e}+{i}"));
                }
            }
            Addr::Ind { off, .. } => {
                self.ins(format!("ldy #{}", off + i));
                self.ins(format!("sta ({}),y", ZP[zp]));
            }
            Addr::Idx { base, x, off } => {
                self.ins(format!("ldx {x}"));
                self.ins(format!("sta {base}+{},x", off + i));
            }
        }
    }

    /// Copies `size` bytes from `src` to `dst`.
    pub fn copy(&mut self, dst: &Addr, src: &Val, size: u64) {
        if size == 0 {
            return;
        }
        // (a value that was built where it goes)
        if let (Addr::Abs(d), Val::Mem(Addr::Abs(s))) = (dst, src)
            && d == s
        {
            return;
        }
        if size > 16 {
            if let Val::Mem(s) = src {
                if self.copy_block(dst, s, size) {
                    return;
                }
                self.addr_to(s, ZP[0]);
                self.addr_to(dst, ZP[1]);
                self.ins(format!("lda #{}", size & 0xff));
                self.ins("sta rt_n");
                self.ins(format!("lda #{}", size >> 8));
                self.ins("sta rt_n+1");
                self.ins("jsr rt_memcpy");
                return;
            }
        }
        if (4..=16).contains(&size)
            && let Val::Mem(s) = src
            && self.copy_loop(dst, s, size)
        {
            return;
        }
        self.setup_val(src, 0);
        self.setup(dst, 2);
        for i in 0..size {
            // (a byte of a view that already is where it goes)
            if let (Addr::Abs(d), Val::Imm(b)) = (dst, src)
                && b.get(i as usize).is_some_and(|s| if i == 0 { s == d } else { *s == format!("{d}+{i}") })
            {
                continue;
            }
            self.lda(src, i, 0);
            self.sta(dst, i, 2);
        }
    }

    /// A copy of up to 256 bytes between places that do not overlap as a
    /// loop in place of the call to `rt_memcpy`; false: not for these
    /// operands.
    pub fn copy_block(&mut self, dst: &Addr, src: &Addr, size: u64) -> bool {
        // not an I/O register: those are written once, in order
        let plain = |e: &str| !e.contains('$');
        if size == 0 || size > 255 {
            return false;
        }
        let l = format!("_C{}", LABELS.fetch_add(1, Ordering::Relaxed) + 1);
        match (src, dst) {
            (Addr::Abs(s), Addr::Abs(d)) if plain(s) && plain(d) => {
                self.ins(format!("ldx #{}", size & 0xff));
                self.label(&l);
                self.ins(format!("lda {s}-1,x"));
                self.ins(format!("sta {d}-1,x"));
                self.ins("dex");
                self.ins(format!("bne {l}"));
                true
            }
            _ if size <= 128 => {
                self.addr_to(src, ZP[0]);
                self.addr_to(dst, ZP[1]);
                self.ins(format!("ldy #{}", size - 1));
                self.label(&l);
                self.ins(format!("lda ({}),y", ZP[0]));
                self.ins(format!("sta ({}),y", ZP[1]));
                self.ins("dey");
                self.ins(format!("bpl {l}"));
                true
            }
            _ => false,
        }
    }

    /// Sets `size` bytes (1 to 255) at `dst` to the byte `val`, as a loop
    /// in place of the call to `rt_memset`.
    pub fn fill_block(&mut self, dst: &Addr, val: &Val, size: u64) {
        let l = format!("_C{}", LABELS.fetch_add(1, Ordering::Relaxed) + 1);
        match dst {
            Addr::Abs(d) if !d.contains('$') => {
                self.setup_val(val, 0);
                self.lda(val, 0, 0);
                self.ins(format!("ldx #{size}"));
                self.label(&l);
                self.ins(format!("sta {d}-1,x"));
                self.ins("dex");
                self.ins(format!("bne {l}"));
            }
            _ => {
                self.addr_to(dst, ZP[1]);
                self.setup_val(val, 0);
                self.lda(val, 0, 0);
                self.ins(format!("ldy #{size}"));
                self.label(&l);
                self.ins("dey");
                self.ins(format!("sta ({}),y", ZP[1]));
                self.ins(format!("bne {l}"));
            }
        }
    }

    /// A copy through a pointer as a loop (10 to 13 bytes, where one byte
    /// costs 6 unrolled); false: not for these operands. The label is a
    /// `_C` label: [`zp_direct`] knows that the pointer registers are the
    /// same on both sides of it.
    fn copy_loop(&mut self, dst: &Addr, src: &Addr, size: u64) -> bool {
        // not an I/O register: those are written once, in order
        let plain = |e: &str| !e.contains('$');
        let n = size - 1;
        let (load, store, y, x) = match (src, dst) {
            (Addr::Ind { off, .. }, Addr::Abs(d)) if plain(d) && off + size <= 128 => {
                if *off == 0 {
                    (format!("lda ({}),y", ZP[0]), format!("sta {d},y"), n, false)
                } else {
                    (format!("lda ({}),y", ZP[0]), format!("sta {d},x"), off + n, true)
                }
            }
            (Addr::Abs(e), Addr::Ind { off, .. }) if plain(e) && off + size <= 128 => {
                if *off == 0 {
                    (format!("lda {e},y"), format!("sta ({}),y", ZP[2]), n, false)
                } else {
                    (format!("lda {e},x"), format!("sta ({}),y", ZP[2]), off + n, true)
                }
            }
            (Addr::Ind { off, .. }, Addr::Ind { off: doff, .. }) if off == doff && off + size <= 128 => {
                (format!("lda ({}),y", ZP[0]), format!("sta ({}),y", ZP[2]), off + n, *off != 0)
            }
            _ => return false,
        };
        self.setup(src, 0);
        self.setup(dst, 2);
        let l = format!("_C{}", LABELS.fetch_add(1, Ordering::Relaxed) + 1);
        if x {
            self.ins(format!("ldx #{n}"));
        }
        self.ins(format!("ldy #{y}"));
        self.label(&l);
        self.ins(load);
        self.ins(store);
        self.ins("dey");
        if x {
            self.ins("dex");
        }
        self.ins(format!("bpl {l}"));
        true
    }

    /// Stores the address of `a` into the two bytes at `dst` (an absolute
    /// address or a zero-page register).
    pub fn addr_to(&mut self, a: &Addr, dst: &str) {
        match a {
            Addr::Abs(e) => {
                self.ins(format!("lda #<({e})"));
                self.ins(format!("sta {dst}"));
                self.ins(format!("lda #>({e})"));
                self.ins(format!("sta {dst}+1"));
            }
            Addr::Idx { base, x, off } => {
                self.ins("clc");
                self.ins(format!("lda {x}"));
                self.ins(format!("adc #<({base}+{off})"));
                self.ins(format!("sta {dst}"));
                self.ins("lda #0");
                self.ins(format!("adc #>({base}+{off})"));
                self.ins(format!("sta {dst}+1"));
            }
            Addr::Ind { ptr, off } => {
                if *off == 0 {
                    self.ins(format!("lda {ptr}"));
                    self.ins(format!("sta {dst}"));
                    self.ins(format!("lda {ptr}+1"));
                    self.ins(format!("sta {dst}+1"));
                } else {
                    self.ins("clc");
                    self.ins(format!("lda {ptr}"));
                    self.ins(format!("adc #<{off}"));
                    self.ins(format!("sta {dst}"));
                    self.ins(format!("lda {ptr}+1"));
                    self.ins(format!("adc #>{off}"));
                    self.ins(format!("sta {dst}+1"));
                }
            }
        }
    }
}

/// A new unique label for code that a pass adds.
pub fn fresh_label() -> String {
    format!("_L{}", LABELS.fetch_add(1, Ordering::Relaxed) + 1)
}

/// Whether the value of operand `x` can be remembered in a register: an
/// immediate, or a symbolic absolute address (frames, statics, runtime
/// registers). Numeric addresses are left alone: they may be I/O registers,
/// where every read counts.
fn trackable(x: &str) -> bool {
    !x.is_empty() && !x.contains(',') && !x.contains('(') && !x.starts_with('$') && x != "a"
}

/// Removes loads of A or Y whose value the register already holds: `sta X`
/// / `lda X`, a repeated `lda #n` or `ldy #n`. A load is only removed when
/// the N/Z flags it would set are not used before the next instruction that
/// sets them again. Labels and calls forget everything.
pub fn peephole(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let parse = |l: &str| -> Option<(String, String)> {
        let t = l.trim();
        if t.is_empty() || t.starts_with(';') || !l.starts_with(' ') {
            return None;
        }
        let (op, arg) = t.split_once(' ').unwrap_or((t, ""));
        Some((op.to_ascii_lowercase(), arg.trim().to_owned()))
    };
    // Whether the flags of a removed load are needed: look ahead to the next
    // instruction that sets N/Z (safe) or reads them / a label (unsafe).
    let flags_needed = |from: usize| -> bool {
        for l in &lines[from..] {
            let t = l.trim();
            if t.is_empty() || t.starts_with(';') {
                continue;
            }
            let Some((op, arg)) = parse(l) else { return true }; // a label
            match op.as_str() {
                "bne" | "beq" | "bpl" | "bmi" | "php" => return true,
                "sta" | "stx" | "sty" | "clc" | "sec" | "cli" | "sei" | "pha" | "nop" | "bcc"
                | "bcs" | "bvc" | "bvs" | "clv" | "cld" | "sed" => continue,
                "asl" | "lsr" | "rol" | "ror" if arg.is_empty() || arg == "a" => return false,
                _ => return false,
            }
        }
        true
    };
    let mut out = String::with_capacity(text.len());
    let mut a: Vec<String> = Vec::new();
    let mut y: Option<String> = None;
    // The memory operand X was loaded from.
    let mut x: Option<String> = None;
    // N/Z reflect the value in A (set by the last instruction that set them).
    let mut nz_a = false;
    // The instruction before, if nothing can come in between.
    let mut last = String::new();
    for (i, l) in lines.iter().enumerate() {
        let Some((op, arg)) = parse(l) else {
            if !l.trim().is_empty() && !l.trim().starts_with(';') {
                a.clear(); // a label
                y = None;
                x = None;
                nz_a = false;
                last.clear();
            }
            out.push_str(l);
            out.push('\n');
            continue;
        };
        let mut keep = true;
        match op.as_str() {
            "lda" => {
                if trackable(&arg) && a.contains(&arg) && (nz_a || !flags_needed(i + 1)) {
                    keep = false;
                } else {
                    a.clear();
                    if trackable(&arg) {
                        a.push(arg.clone());
                    }
                }
            }
            "ldy" => {
                if arg.starts_with('#') && y.as_deref() == Some(arg.as_str()) && !flags_needed(i + 1) {
                    keep = false;
                } else {
                    y = arg.starts_with('#').then(|| arg.clone());
                }
            }
            "ldx" => {
                if trackable(&arg) && x.as_deref() == Some(arg.as_str()) && !flags_needed(i + 1) {
                    keep = false;
                } else {
                    x = trackable(&arg).then(|| arg.clone());
                }
            }
            "sta" => {
                if trackable(&arg) && !a.contains(&arg) {
                    a.push(arg.clone());
                }
                // The store may change the byte X was loaded from.
                if !trackable(&arg) || x.as_deref() == Some(arg.as_str()) {
                    x = None;
                }
            }
            "stx" | "sty" | "inc" | "dec" => {
                a.retain(|v| v.starts_with('#'));
                if op != "stx" {
                    x = None;
                }
            }
            "tax" | "inx" | "dex" | "tsx" => x = None,
            "asl" | "lsr" | "rol" | "ror" => {
                if arg.is_empty() || arg == "a" {
                    a.clear();
                } else {
                    a.retain(|v| v.starts_with('#'));
                    x = None;
                }
            }
            // `tax` / `txa`: A and the flags are as they were
            "txa" | "tya" if last == if op == "txa" { "tax" } else { "tay" } => keep = false,
            "adc" | "sbc" | "and" | "ora" | "eor" | "pla" | "txa" | "tya" => a.clear(),
            "iny" | "dey" | "tay" => y = None,
            "jsr" => {
                a.clear();
                y = None;
                x = None;
            }
            _ => {}
        }
        if keep {
            nz_a = match op.as_str() {
                "lda" | "adc" | "sbc" | "and" | "ora" | "eor" | "pla" | "txa" | "tya" => true,
                "asl" | "lsr" | "rol" | "ror" => arg.is_empty() || arg == "a",
                "sta" | "stx" | "sty" | "clc" | "sec" | "cli" | "sei" | "pha" | "php" | "nop"
                | "clv" | "cld" | "sed" | "bne" | "beq" | "bpl" | "bmi" | "bcc" | "bcs" | "bvc"
                | "bvs" => nz_a,
                _ => false,
            };
            last = op;
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}

/// An upper bound of the size of an instruction line in bytes.
pub fn max_size(op: &str, arg: &str) -> i64 {
    match op {
        "bne" | "beq" | "bpl" | "bmi" | "bcc" | "bcs" | "bvc" | "bvs" => 2,
        _ if arg.is_empty() || arg == "a" => 1,
        _ if arg.starts_with('#') => 2,
        _ => 3,
    }
}

pub fn opposite(branch: &str) -> Option<&'static str> {
    Some(match branch {
        "bne" => "beq",
        "beq" => "bne",
        "bpl" => "bmi",
        "bmi" => "bpl",
        "bcc" => "bcs",
        "bcs" => "bcc",
        "bvc" => "bvs",
        "bvs" => "bvc",
        _ => return None,
    })
}

#[derive(Clone)]
enum Line {
    Label(String),
    Ins(String, String),
    Other(String),
}

fn next_ins(lines: &[Line], mut i: usize) -> Option<usize> {
    while i < lines.len() {
        if let Line::Ins(..) = lines[i] {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Whether label `name` follows line `i` with no instruction in between.
fn label_follows(lines: &[Line], i: usize, name: &str) -> bool {
    for l in &lines[i + 1..] {
        match l {
            Line::Label(n) if n == name => return true,
            Line::Label(_) | Line::Other(_) => {}
            Line::Ins(..) => return false,
        }
    }
    false
}

/// Cleans up the control flow of one function:
/// * a jump to a label that is followed by another `jmp` (or `rts`) goes
///   there directly,
/// * a jump to the next line is removed,
/// * `bcc skip / jmp target / skip:` becomes `bcs target` when the target
///   is certainly within branch range (sizes are upper bounds),
/// * `bne a / beq b / a: jmp target / b:` becomes `bne target` likewise,
/// * labels nothing refers to any more are removed (the peephole pass then
///   sees through them). Only local labels (`_L…`, `F…_b…`) are touched.
pub fn relax(text: &str) -> String {
    relax_with(text, &|op, arg| max_size(op, arg), 110)
}

/// [`relax`] with the sizes of the lines given by `size` (upper bounds)
/// and branches allowed up to `range` bytes away.
pub fn relax_with(text: &str, size: &dyn Fn(&str, &str) -> i64, range: i64) -> String {
    let mut lines: Vec<Line> = text
        .lines()
        .map(|l| {
            let t = l.trim();
            if t.is_empty() || t.starts_with(';') {
                Line::Other(l.to_owned())
            } else if !l.starts_with(' ') {
                match t.strip_suffix(':') {
                    Some(name) => Line::Label(name.to_owned()),
                    None => Line::Other(l.to_owned()),
                }
            } else {
                let (op, arg) = t.split_once(' ').unwrap_or((t, ""));
                Line::Ins(op.to_owned(), arg.trim().to_owned())
            }
        })
        .collect();
    let local = |name: &str| name.starts_with("_L") || (name.starts_with('F') && name.contains("_b"));
    for _ in 0..8 {
        let mut changed = false;
        // positions (upper bounds) of lines and labels
        let mut pos = Vec::with_capacity(lines.len());
        let mut at = 0i64;
        let mut label_pos = std::collections::HashMap::new();
        for (i, l) in lines.iter().enumerate() {
            pos.push(at);
            match l {
                Line::Label(n) => {
                    label_pos.insert(n.clone(), (i, at));
                }
                Line::Ins(op, arg) => at += size(op, arg),
                Line::Other(s) => {
                    // a label with code after it, or something unknown
                    let t = s.split(';').next().unwrap_or("").trim();
                    if !t.is_empty() && !t.contains('=') {
                        at += match t.split_once(':') {
                            Some((_, rest)) if rest.trim().is_empty() => 0,
                            Some((_, rest)) => {
                                let (op, arg) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
                                size(op, arg.trim())
                            }
                            None => 100_000,
                        };
                    }
                }
            }
        }
        // 1. thread jumps: jmp L where L: jmp T / rts
        for i in 0..lines.len() {
            let Line::Ins(op, arg) = &lines[i] else { continue };
            if op != "jmp" {
                continue;
            }
            let Some(&(li, _)) = label_pos.get(arg) else { continue };
            let Some(ni) = next_ins(&lines, li) else { continue };
            if ni == i {
                continue;
            }
            match &lines[ni] {
                Line::Ins(o, a) if o == "jmp" && a != arg && !a.starts_with('(') => {
                    lines[i] = Line::Ins("jmp".into(), a.clone());
                    changed = true;
                }
                Line::Ins(o, _) if o == "rts" => {
                    lines[i] = Line::Ins("rts".into(), String::new());
                    changed = true;
                }
                _ => {}
            }
        }
        // 1b. jsr X / rts  ->  jmp X; what follows a jmp or rts up to
        // the next label is never reached
        for i in 0..lines.len() {
            let Line::Ins(op, arg) = &lines[i] else { continue };
            if op == "jsr"
                && let Some(j) = next_ins(&lines, i + 1)
                && matches!(&lines[j], Line::Ins(o, _) if o == "rts")
            {
                lines[i] = Line::Ins("jmp".into(), arg.clone());
                changed = true;
            }
        }
        let mut dead = false;
        for l in lines.iter_mut() {
            match l {
                // (a directive is data, perhaps of an `asm!` block)
                Line::Ins(op, _) if op.starts_with('.') => dead = false,
                Line::Ins(op, _) => {
                    if dead {
                        *l = Line::Other(String::new());
                        changed = true;
                    } else {
                        dead = matches!(op.as_str(), "jmp" | "rts" | "rti");
                    }
                }
                Line::Label(_) => dead = false,
                Line::Other(s) => {
                    let t = s.trim();
                    if !t.is_empty() && !t.starts_with(';') {
                        dead = false;
                    }
                }
            }
        }
        // 2. jmp to the next line
        for i in 0..lines.len() {
            let Line::Ins(op, arg) = &lines[i] else { continue };
            if op == "jmp" && label_follows(&lines, i, arg) {
                lines[i] = Line::Other(String::new());
                changed = true;
            }
        }
        // 3. bcc skip / jmp target / skip:  ->  bcs target
        for i in 0..lines.len() {
            let Line::Ins(op, skip) = &lines[i] else { continue };
            let Some(opp) = opposite(op) else { continue };
            let Some(j) = next_ins(&lines, i + 1) else { continue };
            let Line::Ins(jop, target) = &lines[j] else { continue };
            if jop != "jmp" || !local(target) {
                continue;
            }
            // only the jmp between the branch and its skip label
            if !label_follows(&lines, j, skip)
                || (i + 1..j).any(|m| matches!(lines[m], Line::Label(_)))
            {
                continue;
            }
            let Some(&(_, tpos)) = label_pos.get(target) else { continue };
            let d = tpos - (pos[i] + 2);
            if (-range..=range).contains(&d) {
                let target = target.clone();
                lines[i] = Line::Ins(opp.to_owned(), target);
                lines[j] = Line::Other(String::new());
                changed = true;
            }
        }
        // 3c. bne a / beq b / a:  ->  beq b
        // (the first branch only skips the second)
        for i in 0..lines.len() {
            let Line::Ins(op, la) = &lines[i] else { continue };
            let Some(opp) = opposite(op) else { continue };
            let Some(j) = next_ins(&lines, i + 1) else { continue };
            let Line::Ins(op2, _) = &lines[j] else { continue };
            if op2 == opp
                && !(i + 1..j).any(|m| matches!(lines[m], Line::Label(_)))
                && label_follows(&lines, j, la)
            {
                lines[i] = Line::Other(String::new());
                changed = true;
            }
        }
        // 3b. bne a / beq b / a: jmp target / b:  ->  bne target
        // (the second branch is the opposite of the first: always taken)
        for i in 0..lines.len() {
            let Line::Ins(op, la) = &lines[i] else { continue };
            let Some(opp) = opposite(op) else { continue };
            let Some(j) = next_ins(&lines, i + 1) else { continue };
            let Line::Ins(op2, lb) = &lines[j] else { continue };
            if op2 != opp || !local(la) || (i + 1..j).any(|m| matches!(lines[m], Line::Label(_))) {
                continue;
            }
            let Some(k) = next_ins(&lines, j + 1) else { continue };
            let Line::Ins(jop, target) = &lines[k] else { continue };
            // between the branches and the jump: the label `a` alone
            let mut between = (j + 1..k).filter_map(|m| match &lines[m] {
                Line::Label(n) => Some(n),
                _ => None,
            });
            if jop != "jmp"
                || !local(target)
                || between.next() != Some(la)
                || between.next().is_some()
                || !label_follows(&lines, k, lb)
            {
                continue;
            }
            // nothing else leads to `a`
            let refs = lines
                .iter()
                .filter(|l| match l {
                    Line::Ins(_, arg) | Line::Other(arg) => {
                        arg.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).any(|w| w == la)
                    }
                    Line::Label(_) => false,
                })
                .count();
            let Some(&(_, tpos)) = label_pos.get(target) else { continue };
            let d = tpos - (pos[i] + 2);
            if refs == 1 && (-range..=range).contains(&d) {
                lines[i] = Line::Ins(op.clone(), target.clone());
                lines[j] = Line::Other(String::new());
                lines[k] = Line::Other(String::new());
                changed = true;
            }
        }
        // 4. unreferenced local labels
        let mut used = std::collections::HashSet::new();
        for l in &lines {
            if let Line::Ins(_, arg) | Line::Other(arg) = l {
                for w in arg.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
                    used.insert(w.to_owned());
                }
            }
        }
        // (block labels too: the passes after this see through them)
        let block = |n: &str| {
            n.strip_prefix('F').and_then(|r| r.split_once("_b")).is_some_and(|(id, b)| {
                !id.is_empty()
                    && !b.is_empty()
                    && id.bytes().all(|c| c.is_ascii_digit())
                    && b.bytes().all(|c| c.is_ascii_digit())
            })
        };
        for l in lines.iter_mut() {
            if let Line::Label(n) = l
                && (n.starts_with("_L") || block(n))
                && !used.contains(n.as_str())
            {
                *l = Line::Other(String::new());
                changed = true;
            }
        }
        lines.retain(|l| !matches!(l, Line::Other(s) if s.is_empty()));
        if !changed {
            break;
        }
    }
    let mut out = String::with_capacity(text.len());
    for l in &lines {
        match l {
            Line::Label(n) => {
                out.push_str(n);
                out.push_str(":\n");
            }
            Line::Ins(op, arg) if arg.is_empty() => {
                let _ = writeln!(out, "        {op}");
            }
            Line::Ins(op, arg) => {
                let _ = writeln!(out, "        {op} {arg}");
            }
            Line::Other(s) => {
                out.push_str(s);
                out.push('\n');
            }
        }
    }
    out
}

/// Makes the branches long that do not reach their targets any more after
/// text was put between them: `bcc far` becomes `bcs skip` / `jmp far` /
/// `skip:`. Sizes are upper bounds, as in [`relax`], which makes such a
/// branch short again if it can be.
pub fn widen(lines: &mut Vec<String>) {
    loop {
        let mut pos = Vec::with_capacity(lines.len());
        let mut label_pos = std::collections::HashMap::new();
        let mut at = 0i64;
        for l in lines.iter() {
            pos.push(at);
            let code = l.split(';').next().unwrap_or("");
            let mut rest = code.trim();
            if !l.starts_with([' ', '\t'])
                && let Some((name, r)) = rest.split_once(':')
            {
                label_pos.insert(name.trim().to_owned(), at);
                rest = r.trim();
            }
            if !rest.is_empty() && !rest.contains('=') {
                let (op, arg) = rest.split_once(' ').unwrap_or((rest, ""));
                at += max_size(op, arg.trim());
            }
        }
        let far = lines.iter().enumerate().find_map(|(i, l)| {
            if !l.starts_with([' ', '\t']) {
                return None;
            }
            let (op, target) = l.split(';').next().unwrap_or("").trim().split_once(' ')?;
            let opp = opposite(op)?;
            let target = target.trim();
            let d = label_pos.get(target)? - (pos[i] + 2);
            (!target.starts_with('@') && !(-110..=110).contains(&d)).then(|| (i, opp, target.to_owned()))
        });
        let Some((i, opp, target)) = far else { break };
        let skip = fresh_label();
        lines.splice(
            i..=i,
            [format!("        {opp} {skip}"), format!("        jmp {target}"), format!("{skip}:")],
        );
    }
}

/// The identifiers in assembly text (not in comments).
fn idents(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .flat_map(|l| {
            l.split(';').next().unwrap_or("").split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        })
        .filter(|t| t.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_'))
}

/// Removes the routines, tables and equates of library assembly (the
/// runtime and the rat sources) that the program does not use.
///
/// The text is cut into chunks at its global labels (`name:` or
/// `name = value` at the start of a line; `@local` and `_L…` labels belong
/// to the chunk before them). A chunk is kept if `program` (the generated
/// code and data) or a kept chunk mentions its name, or if the kept chunk
/// before it runs into it (its last instruction is not `rts`, `jmp` or
/// `rti`). Text before the first label is always kept.
///
/// A chunk can have two forms: what is in the first branch of an
/// `.ifdef NAME { ... } .else { ... }` counts only if the chunk NAME is
/// kept anyway (the assembler then picks that branch). So a small routine
/// can pass its work on to a larger one where the program has both.
pub fn prune(lib: &str, program: &str) -> String {
    struct Chunk<'a> {
        name: &'a str,
        text: String,
        equate: bool,
        falls_through: bool,
    }
    let mut preamble = String::new();
    let mut chunks: Vec<Chunk> = Vec::new();
    for line in lib.lines() {
        let starts_chunk = line
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && !line.starts_with("_L");
        let name = if starts_chunk {
            let end = line
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(line.len());
            let rest = line[end..].trim_start();
            (rest.starts_with(':') || rest.starts_with('=')).then(|| (&line[..end], rest.starts_with('=')))
        } else {
            None
        };
        if let Some((name, equate)) = name {
            chunks.push(Chunk { name, text: String::new(), equate, falls_through: false });
        }
        match chunks.last_mut() {
            Some(c) => {
                c.text.push_str(line);
                c.text.push('\n');
            }
            None => {
                preamble.push_str(line);
                preamble.push('\n');
            }
        }
    }
    for c in chunks.iter_mut() {
        // The last instruction or directive of the chunk.
        let mut last = None;
        for line in c.text.lines() {
            let mut t = line.split(';').next().unwrap_or("").trim();
            // strip a leading label
            if let Some(i) = t.find(':') {
                let (l, r) = t.split_at(i);
                if !l.contains(char::is_whitespace) {
                    t = r[1..].trim();
                }
            }
            // the end of a conditional block (`}`, `} .else {`) is no code
            if !t.is_empty() && !t.starts_with('}') {
                last = Some(t.split_whitespace().next().unwrap_or("").to_ascii_lowercase());
            }
        }
        c.falls_through = match last.as_deref() {
            None => true, // only a label: an alias of what follows
            Some(op) => !(matches!(op, "rts" | "jmp" | "rti") || op.starts_with('.') || c.equate),
        };
    }
    let index: std::collections::HashMap<&str, usize> =
        chunks.iter().enumerate().map(|(i, c)| (c.name, i)).collect();
    // What a chunk refers to: always, and in the first branch of an
    // `.ifdef NAME {` (up to its `}`), which is only assembled if the
    // chunk NAME is kept for another reason.
    let refs = |text: &str| -> (Vec<usize>, Vec<(usize, Vec<usize>)>) {
        let mut always = Vec::new();
        let mut cond: Vec<(usize, Vec<usize>)> = Vec::new();
        let mut inside = false;
        for line in text.lines() {
            let t = line.split(';').next().unwrap_or("").trim();
            if let Some(rest) = t.strip_prefix(".ifdef ") {
                let name = rest.trim_end_matches('{').trim();
                match index.get(name) {
                    Some(&i) => {
                        cond.push((i, Vec::new()));
                        inside = true;
                    }
                    None => inside = false,
                }
                continue;
            }
            if t.starts_with('}') {
                inside = false;
            }
            let found = idents(line).filter_map(|t| index.get(t).copied());
            match cond.last_mut() {
                Some((_, v)) if inside => v.extend(found),
                _ => always.extend(found),
            }
        }
        (always, cond)
    };
    let mut keep = vec![false; chunks.len()];
    let mut work: Vec<usize> = idents(program)
        .chain(idents(&preamble))
        .filter_map(|t| index.get(t).copied())
        .collect();
    let mut waiting: Vec<(usize, Vec<usize>)> = Vec::new();
    loop {
        while let Some(i) = work.pop() {
            if std::mem::replace(&mut keep[i], true) {
                continue;
            }
            let (always, cond) = refs(&chunks[i].text);
            work.extend(always);
            waiting.extend(cond);
            if chunks[i].falls_through && i + 1 < chunks.len() {
                work.push(i + 1);
            }
        }
        // branches whose condition holds by now
        let (ready, rest): (Vec<_>, Vec<_>) = waiting.into_iter().partition(|(c, _)| keep[*c]);
        waiting = rest;
        work.extend(ready.into_iter().flat_map(|(_, v)| v).filter(|&i| !keep[i]));
        if work.is_empty() {
            break;
        }
    }
    let mut out = preamble;
    for (c, k) in chunks.iter().zip(&keep) {
        if *k {
            out.push_str(&c.text);
        }
    }
    out
}

/// Uses pointers that live in the zero page directly: the setup
///
/// ```text
///         lda P
///         sta zp_pN
///         lda P+1
///         sta zp_pN+1
/// ```
///
/// (or without its first line, where A holds the low byte of P from a load
/// further up) is removed and the following `(zp_pN),y` accesses become
/// `(P),y`, when P
/// is in the zero page (`in_zp`) and, as far as the scratch pointer holds P
/// (the region),
/// * P is not written,
/// * nothing else reads the scratch pointer (a routine that is called),
/// * the code is straight, apart from branches inside the region (to `_C`
///   labels of the code generator and to `@labels` of an `asm!` block).
///
/// The region ends at the next setup, at a return, at a jump, or at any
/// other label that is reached from in front of the setup. Labels that are
/// only reached from behind it belong to the region, with all the lines
/// that branch to them: a setup that the data-flow pass moved in front of
/// a loop serves the whole loop. Behind the region nothing may use the
/// scratch pointer before it is set again.
pub fn zp_direct(text: &str, in_zp: impl Fn(&str) -> bool) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    // The label and the instruction of a line.
    type Parsed = (Option<String>, Option<(String, String)>);
    let parse = |l: &str| -> Parsed {
        let t = l.split(';').next().unwrap_or("").trim();
        if t.is_empty() {
            return (None, None);
        }
        let (label, rest) = if l.starts_with(' ') || l.starts_with('\t') {
            (None, t)
        } else {
            match t.split_once(':') {
                Some((n, r)) if !n.contains(char::is_whitespace) => (Some(n.to_owned()), r.trim()),
                _ => (Some(t.to_owned()), ""),
            }
        };
        let ins = (!rest.is_empty()).then(|| {
            let (op, arg) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            (op.to_ascii_lowercase(), arg.trim().to_owned())
        });
        (label, ins)
    };
    let ins = |l: &str| -> Option<(String, String)> {
        match parse(l) {
            (None, i) => i,
            _ => None,
        }
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    // A function of the code generator: it sets the pointers up itself.
    let generated = |arg: &str| arg.strip_prefix('F').is_some_and(digits);
    // A label inside a function that the code generator made.
    let local = |arg: &str| {
        arg.strip_prefix("_L").is_some_and(digits)
            || arg.strip_prefix('F').and_then(|r| r.split_once("_b")).is_some_and(|(id, b)| digits(id) && digits(b))
    };
    let word = |line: &str, name: &str| {
        line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '@')).any(|w| w == name)
    };
    let mut refs: std::collections::HashMap<String, Vec<usize>> = std::collections::HashMap::new();
    let mut refs_stale = true;
    let mut i = 0;
    while i + 3 < lines.len() {
        let quad: Vec<Option<(String, String)>> = (0..4).map(|k| ins(&lines[i + k])).collect();
        // through A, or through X (the data-flow pass, in front of an
        // access with an index in Y)
        let full = match (&quad[0], &quad[1], &quad[2], &quad[3]) {
            (Some((o0, p)), Some((o1, z)), Some((o2, p1)), Some((o3, z1))) => {
                let via_x = o0 == "ldx" && o1 == "stx" && o2 == "ldx" && o3 == "stx";
                (((o0 == "lda" && o1 == "sta" && o2 == "lda" && o3 == "sta") || via_x)
                    && ZP.contains(&z.as_str())
                    && *p1 == format!("{p}+1")
                    && *z1 == format!("{z}+1")
                    && in_zp(p))
                .then(|| (p.clone(), z.clone(), via_x, 4))
            }
            _ => None,
        };
        // Without the first load: A holds the low byte of P from a load
        // further up (the test of a loop, `lda P` / `cmp end` / `beq`),
        // with nothing in between that changes A or P.
        let short = || -> Option<(String, String, bool, usize)> {
            let (Some((o1, z)), Some((o2, p1)), Some((o3, z1))) = (&quad[0], &quad[1], &quad[2]) else { return None };
            let p = p1.strip_suffix("+1")?;
            if !(o1 == "sta" && o2 == "lda" && o3 == "sta")
                || !ZP.contains(&z.as_str())
                || *z1 != format!("{z}+1")
                || !in_zp(p)
            {
                return None;
            }
            for k in (0..i).rev() {
                let (label, instr) = parse(&lines[k]);
                if label.is_some() {
                    return None;
                }
                let Some((op, arg)) = instr else { continue };
                if op == "lda" {
                    return (arg == p).then(|| (p.to_owned(), z.clone(), false, 3));
                }
                let keeps = matches!(
                    op.as_str(),
                    "cmp" | "cpx" | "cpy" | "bne" | "beq" | "bcc" | "bcs" | "bpl" | "bmi" | "bvc" | "bvs" | "clc"
                        | "sec" | "ldx" | "ldy" | "inx" | "iny" | "dex" | "dey"
                ) || (matches!(op.as_str(), "sta" | "stx" | "sty")
                    && arg != p
                    && !arg.starts_with(&format!("{p}+"))
                    && !arg.contains([',', '(']));
                if !keeps {
                    return None;
                }
            }
            None
        };
        let Some((p, z, via_x, len)) = full.or_else(short) else {
            i += 1;
            continue;
        };
        let through = format!("({z}),y");
        let sets_up =
            |op: &str, arg: &str| matches!(op, "sta" | "stx") && (arg == z || arg == format!("{z}+1"));
        // The region in which the scratch pointer holds P.
        // The lines that name each operand (for the labels: the branches and
        // jumps to them), as the lines are now.
        if refs_stale {
            refs.clear();
            for (k, l) in lines.iter().enumerate() {
                if let Some((_, arg)) = parse(l).1 {
                    refs.entry(arg).or_default().push(k);
                }
            }
            refs_stale = false;
        }
        // The region: first up to the next label (a statement), then, if
        // the scratch pointer is used beyond that, with the loops behind
        // the setup.
        let mut found = None;
        for extend in [false, true] {
            let mut end = i + len;
            let mut ok = true;
            let mut loops: Vec<(usize, String)> = Vec::new();
            let mut seen_label = false;
            // The lines that branch or jump to a label.
            let mentions = |name: &str| -> &[usize] { refs.get(name).map_or(&[], |v| &v[..]) };
            // labels inside the region that are only reached from behind the
            // setup: of a loop that the setup was moved in front of
            let mut inner: Vec<String> = Vec::new();
            // the region ends where the scratch pointer is set again or the
            // function returns; with a jump
            let (mut closed, mut at_jump) = (false, false);
            while end < lines.len() {
                let (label, instr) = parse(&lines[end]);
                if let Some(name) = label {
                    if name.starts_with('@') {
                        loops.push((end, name));
                    } else if !name.starts_with("_C") {
                        if !extend || !local(&name) || mentions(&name).iter().any(|&k| k < i) {
                            break; // a label
                        }
                        inner.push(name);
                    }
                    seen_label = true;
                }
                let Some((op, arg)) = instr else {
                    end += 1;
                    continue;
                };
                if sets_up(&op, &arg) || matches!(op.as_str(), "rts" | "rti") {
                    closed = true;
                    break;
                }
                if op == "jmp" && (generated(&arg) || local(&arg)) {
                    at_jump = true;
                    break;
                }
                // A call (or a jump to a routine: `jsr` / `rts`) may read the
                // scratch pointer; so does any other use of it.
                if (matches!(op.as_str(), "jsr" | "jmp") && !generated(&arg))
                    || (arg.contains(z.as_str()) && arg != through)
                {
                    ok = false;
                    break;
                }
                let writes = matches!(
                    op.as_str(),
                    "sta" | "stx" | "sty" | "inc" | "dec" | "asl" | "lsr" | "rol" | "ror"
                );
                if writes && (arg == p || arg.starts_with(&format!("{p}+"))) {
                    // P changes: the region ends here, if the code was straight
                    // so far and what follows does not use the scratch pointer
                    // (`*p = x; p += 1`).
                    ok = !seen_label;
                    for l in &lines[end + 1..] {
                        let (label, instr) = parse(l);
                        if label.is_some() {
                            break;
                        }
                        let Some((op, arg)) = instr else { continue };
                        if sets_up(&op, &arg)
                            || matches!(op.as_str(), "rts" | "rti")
                            || (op == "jmp" && (generated(&arg) || local(&arg)))
                        {
                            break;
                        }
                        if (matches!(op.as_str(), "jsr" | "jmp") && !generated(&arg)) || arg.contains(z.as_str()) {
                            ok = false;
                            break;
                        }
                    }
                    break;
                }
                end += 1;
            }
            // The labels in the region: only reached from inside it (the jump
            // that ends it counts as inside).
            let limit = if at_jump { end + 1 } else { end };
            if ok && inner.iter().any(|name| mentions(name).iter().any(|&k| k < i + len || k >= limit)) {
                ok = false;
            }
            // Beyond the region nothing uses the scratch pointer before it is
            // set again (as the text goes: to the end of the function).
            if ok && !closed {
                for l in &lines[end..] {
                    let (label, instr) = parse(l);
                    if label.is_some_and(|n| generated(&n)) {
                        break;
                    }
                    let Some((op, arg)) = instr else { continue };
                    if sets_up(&op, &arg) {
                        break;
                    }
                    if arg.contains(z.as_str()) {
                        ok = false;
                        break;
                    }
                }
            }
            // The `@labels` in the region: only branched to from inside it.
            for (at, name) in &loops {
                if !ok {
                    break;
                }
                let is_scope_end = |l: &str| parse(l).0.is_some_and(|n| !n.starts_with('@'));
                let lo = (0..*at).rev().find(|&k| is_scope_end(&lines[k])).map_or(0, |k| k + 1);
                let hi = (*at + 1..lines.len()).find(|&k| is_scope_end(&lines[k])).unwrap_or(lines.len());
                for k in (lo..hi).filter(|&k| k != *at && !(i + len..end).contains(&k)) {
                    if word(&lines[k], name) {
                        ok = false;
                        break;
                    }
                }
            }
            // A branch from the region to an `@label` outside it: the code
            // there may use the pointer too.
            if ok {
                for l in &lines[i + len..end] {
                    if let Some((_, arg)) = parse(l).1
                        && arg.starts_with('@')
                        && !loops.iter().any(|(_, n)| *n == arg)
                    {
                        ok = false;
                        break;
                    }
                }
            }
            // The removed loads also set A (or X) and the flags: what follows
            // must load the register again before using either (labels on the
            // way do not matter: this is about the code that runs after the
            // setup).
            if ok && via_x {
                // The next instruction sets the flags, and X is not used up to
                // its next load, the end of the statement (a label) or the end
                // of the function.
                let mut first = true;
                for l in &lines[i + len..] {
                    let (label, instr) = parse(l);
                    if label.is_some_and(|n| !n.starts_with('@') && !n.starts_with("_C")) {
                        break;
                    }
                    let Some((op, arg)) = instr else { continue };
                    let arg = arg.to_ascii_lowercase();
                    if std::mem::take(&mut first) && !matches!(op.as_str(), "lda" | "ldy" | "ldx" | "tay" | "tya" | "pla") {
                        ok = false;
                        break;
                    }
                    // (a function of the program may take X as an argument)
                    let routine = matches!(op.as_str(), "jsr" | "jmp") && !local(&arg);
                    if matches!(op.as_str(), "ldx" | "tax" | "tsx" | "rts" | "rti")
                        || (matches!(op.as_str(), "jsr" | "jmp") && !routine && !arg.starts_with('@'))
                    {
                        break;
                    }
                    if routine
                        || matches!(op.as_str(), "txa" | "txs" | "stx" | "inx" | "dex" | "cpx")
                        || arg.ends_with(",x")
                        || arg.ends_with(",x)")
                    {
                        ok = false;
                        break;
                    }
                }
            } else if ok {
                ok = false;
                for l in &lines[i + len..] {
                    let Some((op, _)) = parse(l).1 else { continue };
                    match op.as_str() {
                        "lda" | "pla" | "txa" | "tya" => {
                            ok = true;
                            break;
                        }
                        "ldx" | "ldy" | "clc" | "sec" | "inx" | "iny" | "dex" | "dey" => {}
                        _ => break,
                    }
                }
            }
            if ok {
                found = Some(end);
                break;
            }
        }
        let Some(end) = found else {
            i += 1;
            continue;
        };
        let direct = format!("({p}),y");
        for l in &mut lines[i + len..end] {
            if l.contains(&through) {
                *l = l.replace(&through, &direct);
            }
        }
        lines.drain(i..i + len);
        refs_stale = true;
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Whether the program text can change machine state that the exit code
/// sets up again for BASIC: the interrupt setup, the memory configuration,
/// the VIC mode and bank, the SID.
///
/// The answer is "no" only if every write is to a place that is known and
/// harmless: a label of the program, or an absolute address outside the
/// CPU port, the interrupt vectors and the I/O area (the border and
/// background colours `$D020`-`$D02E` are left as they are and do not count).
/// A store through a pointer or with an index on a numeric address, a call
/// to a numeric address outside the ROMs, an indirect jump, `sei` and
/// labels that are names for such addresses all count as "can change it".
/// Calls into the BASIC and KERNAL ROMs do not: they leave the machine in
/// a state BASIC can go on with.
pub fn touches_hw(text: &str) -> bool {
    let risky_addr = |a: u32| a < 2 || (0x0314..=0x0319).contains(&a) || (0xd000..=0xdfff).contains(&a) && !(0xd020..=0xd02e).contains(&a);
    // A number or a sum of numbers (`$d000+32`).
    let number = |e: &str| -> Option<u32> {
        let mut sum = 0u32;
        for part in e.split('+') {
            let part = part.trim();
            sum += match part.strip_prefix('$') {
                Some(h) => u32::from_str_radix(h, 16).ok()?,
                None => part.parse().ok()?,
            };
        }
        Some(sum)
    };
    let numeric = |e: &str| e.starts_with('$') || e.chars().next().is_some_and(|c| c.is_ascii_digit());
    // Names for risky addresses: `name = $d011`.
    let mut names = std::collections::HashSet::new();
    for l in text.lines() {
        let l = l.split(';').next().unwrap_or("");
        if let Some((name, value)) = l.split_once('=')
            && !l.starts_with(' ')
            && numeric(value.trim())
            && number(value).is_none_or(risky_addr)
        {
            names.insert(name.trim().to_owned());
        }
    }
    for l in text.lines() {
        let l = l.split(';').next().unwrap_or("");
        let mut t = l.trim();
        // strip a label in front of the instruction
        if let Some(i) = t.find(':')
            && !t[..i].contains(char::is_whitespace)
        {
            t = t[i + 1..].trim();
        }
        if t.is_empty() || t.starts_with('.') || t.contains('=') {
            continue;
        }
        let (op, arg) = t.split_once(char::is_whitespace).unwrap_or((t, ""));
        let (op, arg) = (op.to_ascii_lowercase(), arg.trim());
        match op.as_str() {
            "sei" | "txs" | "brk" => return true,
            "jsr" | "jmp" => {
                let rom = |a: u32| (0xa000..=0xbfff).contains(&a) || a >= 0xe000;
                if arg.starts_with('(') || numeric(arg) && !number(arg).is_some_and(rom) {
                    return true;
                }
            }
            "sta" | "stx" | "sty" | "inc" | "dec" | "asl" | "lsr" | "rol" | "ror" => {
                if arg.is_empty() || arg.eq_ignore_ascii_case("a") {
                    continue;
                }
                if arg.contains('(') {
                    return true;
                }
                let (base, indexed) = match arg.split_once(',') {
                    Some((b, _)) => (b.trim(), true),
                    None => (arg, false),
                };
                if numeric(base) {
                    if indexed || number(base).is_none_or(risky_addr) {
                        return true;
                    }
                } else {
                    let end = base
                        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '@'))
                        .unwrap_or(base.len());
                    if names.contains(&base[..end]) {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }
    false
}

/// Whether the assembly names one of the interrupt vectors `$0314`-`$0319`
/// (by number or through a name for it): the exit code then sets the IRQ
/// vector to the KERNAL's handler again.
pub fn touches_irq(text: &str) -> bool {
    let vector = |e: &str| -> bool {
        let e = e.trim().trim_start_matches(['#', '<', '>', '(']);
        let end = e.find(|c: char| !(c.is_ascii_alphanumeric() || c == '$' || c == '+' || c == ' ')).unwrap_or(e.len());
        let mut sum = 0u32;
        for part in e[..end].split('+') {
            let part = part.trim();
            let v = match part.strip_prefix('$') {
                Some(h) => u32::from_str_radix(h, 16).ok(),
                None => part.parse().ok(),
            };
            match v {
                Some(v) => sum += v,
                None => return false,
            }
        }
        (0x0314..=0x0319).contains(&sum)
    };
    let mut names = std::collections::HashSet::new();
    for l in text.lines() {
        let l = l.split(';').next().unwrap_or("");
        if let Some((name, value)) = l.split_once('=')
            && !l.starts_with(' ')
            && vector(value)
        {
            names.insert(name.trim().to_owned());
        }
    }
    text.lines().any(|l| {
        let l = l.split(';').next().unwrap_or("");
        let mut t = l.trim();
        if let Some(i) = t.find(':')
            && !t[..i].contains(char::is_whitespace)
        {
            t = t[i + 1..].trim();
        }
        if t.is_empty() || t.starts_with('.') || t.contains('=') {
            return false;
        }
        let Some((_, arg)) = t.split_once(char::is_whitespace) else { return false };
        let arg = arg.trim();
        if vector(arg) {
            return true;
        }
        let base = arg.trim_start_matches(['#', '<', '>', '(']);
        let end = base.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '@')).unwrap_or(base.len());
        names.contains(&base[..end])
    })
}

/// `X+2+1` -> (`X`, 3); `None` if the operand is not a symbol plus offsets.
pub fn sym_off(arg: &str) -> Option<(&str, u64)> {
    let mut parts = arg.split('+');
    let base = parts.next()?;
    let mut chars = base.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let mut off = 0;
    for p in parts {
        off += p.parse::<u64>().ok()?;
    }
    Some((base, off))
}

/// One line of a function for [`forward`].
enum FLine {
    /// A label, or a line the pass does not read (kept as it is): what is
    /// known about memory and A is forgotten there.
    Barrier(String),
    Ins(String, String),
    /// A comment or an empty line.
    Other(String),
    Removed,
}

/// Where the names of a function's frame are: name -> (offset in the
/// frame, size). Two names can mean the same bytes (`L<id>_<n>` and
/// `F<id>v<n>`; a caller fills several arguments through one name), so the
/// passes identify a byte by its offset.
pub type FrameLayout = std::collections::HashMap<String, (u64, u64)>;

/// Forwards copies inside one function and removes what that leaves dead.
/// The bytes of the function's own frame (`layout`) can only be changed by
/// this function's code, unless their address is taken or they are indexed
/// ("escaped"), so:
/// * in straight-line code, a read of a byte that holds a copy of an
///   immediate or of another byte reads the original instead,
/// * a read of constant data whose bytes are known (`konst`) becomes an
///   immediate,
/// * a store to a byte that is never read is removed (not below
///   `args_end`: the result and the arguments, which other functions use),
/// * a load of A directly followed by another load of A is removed.
pub fn forward(
    text: &str,
    id: usize,
    layout: &FrameLayout,
    args_end: u64,
    konst: &mut dyn FnMut(&str, u64) -> Option<String>,
) -> String {
    use std::collections::{HashMap, HashSet};
    const IMM_OK: [&str; 11] =
        ["lda", "ldx", "ldy", "adc", "sbc", "and", "ora", "eor", "cmp", "cpx", "cpy"];
    // The frame byte a plain operand names.
    let own = |arg: &str| -> Option<u64> {
        let (base, off) = sym_off(arg)?;
        layout.get(base).map(|&(o, _)| o + off)
    };

    let mut lines: Vec<FLine> = text
        .lines()
        .map(|l| {
            let t = l.trim();
            if t.is_empty() || t.starts_with(';') {
                FLine::Other(l.to_owned())
            } else if !l.starts_with(' ') || t.starts_with('.') || t.starts_with('@') {
                FLine::Barrier(l.to_owned())
            } else {
                let (op, arg) = t.split_once(' ').unwrap_or((t, ""));
                let arg = arg.split(';').next().unwrap_or("").trim();
                FLine::Ins(op.to_ascii_lowercase(), arg.to_owned())
            }
        })
        .collect();

    for _ in 0..8 {
        let mut changed = false;
        // Escaped bytes: of names used in anything but a plain operand.
        let mut escaped: HashSet<u64> = HashSet::new();
        for l in &lines {
            let s = match l {
                FLine::Barrier(s) => s.as_str(),
                FLine::Ins(_, arg) if own(arg).is_none() => arg.as_str(),
                _ => continue,
            };
            for w in s.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
                if let Some(&(o, n)) = layout.get(w) {
                    escaped.extend(o..o + n);
                }
            }
        }
        let plain = |arg: &str| -> Option<u64> { own(arg).filter(|c| !escaped.contains(c)) };

        // Labels of this function that nothing jumps to are no barriers.
        let mut refd: HashSet<&str> = HashSet::new();
        for l in &lines {
            let s = match l {
                FLine::Ins(_, arg) => arg.as_str(),
                FLine::Barrier(s) if !s.trim_end().ends_with(':') => s.as_str(),
                _ => continue,
            };
            refd.extend(s.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')));
        }
        let soft: Vec<bool> = lines
            .iter()
            .map(|l| {
                let FLine::Barrier(s) = l else { return false };
                let Some(n) = s.trim_end().strip_suffix(':') else { return false };
                let local = n.strip_prefix(&format!("F{id}_")).is_some_and(|r| {
                    r.len() > 1
                        && matches!(r.as_bytes()[0], b'b' | b'i' | b'e')
                        && r[1..].bytes().all(|b| b.is_ascii_digit())
                });
                local && !refd.contains(n)
            })
            .collect();

        // 1. forward copies. A value is the operand to read it with and,
        // if it is a frame byte, which one.
        type V = (String, Option<u64>);
        let mut map: HashMap<u64, V> = HashMap::new();
        let mut a: Option<V> = None;
        for (li, l) in lines.iter_mut().enumerate() {
            let (op, arg) = match l {
                FLine::Ins(op, arg) => (op, arg),
                FLine::Barrier(_) if soft[li] => continue,
                FLine::Barrier(_) => {
                    map.clear();
                    a = None;
                    continue;
                }
                _ => continue,
            };
            let kill = |map: &mut HashMap<u64, V>, c: u64| {
                map.remove(&c);
                map.retain(|_, v| v.1 != Some(c));
            };
            let holds = |a: &Option<V>, c: u64| a.as_ref().is_some_and(|v| v.1 == Some(c));
            match op.as_str() {
                "sta" => {
                    if let Some(c) = plain(arg)
                        && !holds(&a, c)
                    {
                        kill(&mut map, c);
                        match &a {
                            Some(v) => {
                                map.insert(c, v.clone());
                            }
                            None => a = Some((arg.clone(), Some(c))),
                        }
                    }
                }
                "stx" | "sty" | "inc" | "dec" | "asl" | "lsr" | "rol" | "ror" => {
                    if arg.is_empty() || arg == "a" {
                        a = None;
                    } else if let Some(c) = plain(arg) {
                        kill(&mut map, c);
                        if holds(&a, c) {
                            a = None;
                        }
                    }
                }
                _ => {
                    let imm_ok = IMM_OK.contains(&op.as_str());
                    if let Some(c) = plain(arg) {
                        if let Some(v) = map.get(&c)
                            && (imm_ok || (op == "bit" && !v.0.starts_with('#')))
                        {
                            *arg = v.0.clone();
                            changed = true;
                        }
                    } else if imm_ok
                        && let Some((base, off)) = sym_off(arg)
                        && base.len() > 1
                        && base.starts_with('A')
                        && base[1..].bytes().all(|b| b.is_ascii_digit())
                        && let Some(v) = konst(base, off)
                    {
                        *arg = v;
                        changed = true;
                    }
                    match op.as_str() {
                        "lda" => {
                            a = if arg.starts_with('#') {
                                Some((arg.clone(), None))
                            } else {
                                plain(arg).map(|c| (arg.clone(), Some(c)))
                            };
                        }
                        "ldx" | "ldy" | "cmp" | "cpx" | "cpy" | "bit" | "tax" | "tay" | "inx"
                        | "iny" | "dex" | "dey" | "clc" | "sec" | "cli" | "sei" | "cld" | "sed"
                        | "clv" | "pha" | "php" | "nop" | "txs" | "tsx" | "bne" | "beq" | "bcc"
                        | "bcs" | "bpl" | "bmi" | "bvc" | "bvs" | "jmp" | "rts" | "rti" => {}
                        "jsr" | "adc" | "sbc" | "and" | "ora" | "eor" | "pla" | "txa" | "tya"
                        | "plp" => a = None,
                        _ => {
                            // unknown: forget everything
                            a = None;
                            map.clear();
                        }
                    }
                }
            }
        }

        // 2. stores nobody reads
        let mut read: HashSet<u64> = HashSet::new();
        for l in &lines {
            if let FLine::Ins(op, arg) = l
                && !matches!(op.as_str(), "sta" | "stx" | "sty")
                && let Some(c) = plain(arg)
            {
                read.insert(c);
            }
        }
        for l in lines.iter_mut() {
            if let FLine::Ins(op, arg) = l
                && matches!(op.as_str(), "sta" | "stx" | "sty")
                && plain(arg).is_some_and(|c| c >= args_end && !read.contains(&c))
            {
                *l = FLine::Removed;
                changed = true;
            }
        }

        // 3. a load of A that the next instruction replaces (not a read of
        // a numeric address: that may be an I/O register)
        let code: Vec<usize> = (0..lines.len())
            .filter(|&i| matches!(&lines[i], FLine::Ins(..) | FLine::Barrier(_)) && !soft[i])
            .collect();
        for w in code.windows(2) {
            let (FLine::Ins(op, arg), FLine::Ins(op2, _)) = (&lines[w[0]], &lines[w[1]]) else {
                continue;
            };
            let simple = arg.starts_with('#') || !(arg.contains('$') || arg.contains('(') || arg.contains(','));
            if op == "lda" && simple && matches!(op2.as_str(), "lda" | "pla" | "txa" | "tya") {
                lines[w[0]] = FLine::Removed;
                changed = true;
            }
        }
        lines.retain(|l| !matches!(l, FLine::Removed));
        if !changed {
            break;
        }
    }
    let mut out = String::with_capacity(text.len());
    for l in &lines {
        match l {
            FLine::Ins(op, arg) if arg.is_empty() => {
                let _ = writeln!(out, "        {op}");
            }
            FLine::Ins(op, arg) => {
                let _ = writeln!(out, "        {op} {arg}");
            }
            FLine::Barrier(s) | FLine::Other(s) => {
                out.push_str(s);
                out.push('\n');
            }
            FLine::Removed => {}
        }
    }
    out
}

/// Removes stores to argument bytes nobody reads, in `code`: the argument
/// slots of functions (`L<id>_<n>`) and the argument areas of assembly
/// functions (`<sym>__args`). `others` is the rest of the program (library
/// assembly, wrappers), which may read them too. `frame` gives the frame,
/// offset and size of a frame name (see [`FrameLayout`]). A byte counts as
/// read if any instruction but a plain store names it; a name used in any
/// other way (indexed, as an address, in a directive) reads all its bytes.
/// A load of A that is replaced by the next instruction goes too.
pub fn drop_dead_args(
    code: &str,
    others: &[&str],
    frame: &dyn Fn(&str) -> Option<(usize, u64, u64)>,
) -> String {
    use std::collections::HashSet;
    // A byte: (frame or name, offset).
    let byte = |base: &str, off: u64| -> (String, u64) {
        match frame(base) {
            Some((id, o, _)) => (format!("F{id}f"), o + off),
            None => (base.to_owned(), off),
        }
    };
    let is_arg_store = |base: &str| -> bool {
        if base.ends_with("__args") {
            return true;
        }
        base.strip_prefix('L')
            .and_then(|r| r.split_once('_'))
            .is_some_and(|(id, n)| {
                !id.is_empty()
                    && id.bytes().all(|b| b.is_ascii_digit())
                    && n.parse::<usize>().is_ok_and(|n| n >= 1)
            })
    };
    let ins = |l: &str| -> Option<(String, String)> {
        let t = l.split(';').next().unwrap_or("").trim();
        if t.is_empty() || !l.starts_with(' ') || t.starts_with('.') || t.starts_with('@') {
            return None;
        }
        let (op, arg) = t.split_once(' ').unwrap_or((t, ""));
        Some((op.to_ascii_lowercase(), arg.trim().to_owned()))
    };
    let mut read: HashSet<(String, u64)> = HashSet::new();
    // names of which every byte is read (not frame names: their bytes are
    // put into `read`)
    let mut all: HashSet<String> = HashSet::new();
    let mut whole = |w: &str, read: &mut HashSet<(String, u64)>| match frame(w) {
        Some((id, o, n)) => {
            for c in o..o + n {
                read.insert((format!("F{id}f"), c));
            }
        }
        None => {
            all.insert(w.to_owned());
        }
    };
    for text in std::iter::once(&code).chain(others) {
        for l in text.lines() {
            let t = l.split(';').next().unwrap_or("");
            if t.trim().is_empty() {
                continue;
            }
            if let Some((op, arg)) = ins(l) {
                if let Some((b, o)) = sym_off(&arg) {
                    if !matches!(op.as_str(), "sta" | "stx" | "sty") {
                        read.insert(byte(b, o));
                    }
                    continue;
                }
                for w in arg.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
                    whole(w, &mut read);
                }
            } else if !t.trim_end().ends_with(':') || t.trim().contains(' ') {
                // a directive, an equate or a label with code after it
                let t = match t.split_once(':') {
                    Some((_, rest)) => rest,
                    None => t,
                };
                // an equate defines its name: only the right side is a use
                let t = match t.split_once('=') {
                    Some((_, rest)) => rest,
                    None => t,
                };
                for w in t.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
                    whole(w, &mut read);
                }
            }
        }
    }
    let mut lines: Vec<Option<&str>> = code.lines().map(Some).collect();
    for l in lines.iter_mut() {
        let Some((op, arg)) = ins(l.unwrap()) else { continue };
        if matches!(op.as_str(), "sta" | "stx" | "sty")
            && let Some((b, o)) = sym_off(&arg)
            && is_arg_store(b)
            && !all.contains(b)
            && !read.contains(&byte(b, o))
        {
            *l = None;
        }
    }
    // loads of A that the next instruction replaces
    loop {
        let mut changed = false;
        let live: Vec<usize> = (0..lines.len())
            .filter(|&i| {
                lines[i].is_some_and(|l| {
                    let t = l.trim();
                    !t.is_empty() && !t.starts_with(';')
                })
            })
            .collect();
        for w in live.windows(2) {
            let (Some((op, arg)), Some((op2, _))) = (ins(lines[w[0]].unwrap()), ins(lines[w[1]].unwrap())) else {
                continue;
            };
            let simple = arg.starts_with('#') || !(arg.contains('$') || arg.contains('(') || arg.contains(','));
            if op == "lda" && simple && matches!(op2.as_str(), "lda" | "pla" | "txa" | "tya") {
                lines[w[0]] = None;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut out = String::with_capacity(code.len());
    for l in lines.into_iter().flatten() {
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// The size of an instruction or directive line, given which symbols are
/// in the zero page: exact for what the code generator emits, otherwise an
/// upper bound (a huge one for directives that are not understood).
pub fn line_size(op: &str, arg: &str, in_zp: &dyn Fn(&str) -> bool) -> i64 {
    match op {
        "bne" | "beq" | "bpl" | "bmi" | "bcc" | "bcs" | "bvc" | "bvs" => return 2,
        "jmp" | "jsr" => return 3,
        ".byte" => {
            let mut n = 0;
            let mut in_str = false;
            let mut item = false;
            for c in arg.chars() {
                match c {
                    '"' => {
                        in_str = !in_str;
                        item = false;
                    }
                    _ if in_str => n += 1,
                    ',' => item = false,
                    c if !c.is_whitespace() && !item => {
                        item = true;
                        n += 1;
                    }
                    _ => {}
                }
            }
            return n;
        }
        ".word" => return 2 * arg.split(',').count() as i64,
        _ if op.starts_with('.') => return 100_000,
        _ => {}
    }
    if arg.is_empty() || arg == "a" {
        return 1;
    }
    if arg.starts_with('#') || arg.starts_with('(') {
        return 2;
    }
    let (base, idx) = match arg.rsplit_once(',') {
        Some((b, i)) => (b.trim(), i.trim().to_ascii_lowercase()),
        None => (arg, String::new()),
    };
    if idx == "y" && !matches!(op, "ldx" | "stx") {
        return 3;
    }
    let name = base.split('+').next().unwrap_or("").trim();
    let zp = match name.strip_prefix('$') {
        Some(h) => base == name && u32::from_str_radix(h, 16).is_ok_and(|v| v < 256),
        None => in_zp(name),
    };
    if zp { 2 } else { 3 }
}
