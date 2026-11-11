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
        if size > 16 {
            if let Val::Mem(s) = src {
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
        self.setup_val(src, 0);
        self.setup(dst, 2);
        for i in 0..size {
            self.lda(src, i, 0);
            self.sta(dst, i, 2);
        }
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
    for (i, l) in lines.iter().enumerate() {
        let Some((op, arg)) = parse(l) else {
            if !l.trim().is_empty() && !l.trim().starts_with(';') {
                a.clear(); // a label
                y = None;
                x = None;
                nz_a = false;
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
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}

/// An upper bound of the size of an instruction line in bytes.
fn max_size(op: &str, arg: &str) -> i64 {
    match op {
        "bne" | "beq" | "bpl" | "bmi" | "bcc" | "bcs" | "bvc" | "bvs" => 2,
        _ if arg.is_empty() || arg == "a" => 1,
        _ if arg.starts_with('#') => 2,
        _ => 3,
    }
}

fn opposite(branch: &str) -> Option<&'static str> {
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
/// * labels nothing refers to any more are removed (the peephole pass then
///   sees through them). Only local labels (`_L…`, `F…_b…`) are touched.
pub fn relax(text: &str) -> String {
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
                Line::Ins(op, arg) => at += max_size(op, arg),
                Line::Other(_) => {}
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
            if (-110..=110).contains(&d) {
                let target = target.clone();
                lines[i] = Line::Ins(opp.to_owned(), target);
                lines[j] = Line::Other(String::new());
                changed = true;
            }
        }
        // 4. unreferenced local labels
        let mut used = std::collections::HashSet::new();
        for l in &lines {
            if let Line::Ins(_, arg) = l {
                used.insert(arg.clone());
            }
        }
        for l in lines.iter_mut() {
            if let Line::Label(n) = l
                && n.starts_with("_L")
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
            if !t.is_empty() {
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
    let mut keep = vec![false; chunks.len()];
    let mut work: Vec<usize> = idents(program)
        .chain(idents(&preamble))
        .filter_map(|t| index.get(t).copied())
        .collect();
    while let Some(i) = work.pop() {
        if std::mem::replace(&mut keep[i], true) {
            continue;
        }
        work.extend(idents(&chunks[i].text).filter_map(|t| index.get(t).copied()));
        if chunks[i].falls_through && i + 1 < chunks.len() {
            work.push(i + 1);
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
/// is removed and the following `(zp_pN),y` accesses become `(P),y`, when P
/// is in the zero page (`in_zp`), is not written before the scratch pointer
/// is set up again, and nothing else (a runtime routine) reads the scratch
/// pointer in between.
pub fn zp_direct(text: &str, in_zp: impl Fn(&str) -> bool) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let ins = |l: &str| -> Option<(String, String)> {
        let t = l.trim();
        if t.is_empty() || t.starts_with(';') || !l.starts_with(' ') {
            return None;
        }
        let (op, arg) = t.split_once(' ').unwrap_or((t, ""));
        Some((op.to_owned(), arg.trim().to_owned()))
    };
    let mut i = 0;
    while i + 3 < lines.len() {
        let quad: Vec<Option<(String, String)>> = (0..4).map(|k| ins(&lines[i + k])).collect();
        let (Some((o0, p)), Some((o1, z)), Some((o2, p1)), Some((o3, z1))) =
            (&quad[0], &quad[1], &quad[2], &quad[3])
        else {
            i += 1;
            continue;
        };
        if !(o0 == "lda"
            && o1 == "sta"
            && o2 == "lda"
            && o3 == "sta"
            && ZP.contains(&z.as_str())
            && *p1 == format!("{p}+1")
            && *z1 == format!("{z}+1")
            && in_zp(p))
        {
            i += 1;
            continue;
        }
        let (p, z) = (p.clone(), z.clone());
        let through = format!("({z}),y");
        // The region in which the scratch pointer holds P.
        let mut end = i + 4;
        let mut ok = true;
        while end < lines.len() {
            let Some((op, arg)) = ins(&lines[end]) else {
                let t = lines[end].trim();
                if t.is_empty() || t.starts_with(';') {
                    end += 1;
                    continue;
                }
                break; // a label
            };
            if op == "sta" && (arg == z || arg == format!("{z}+1")) {
                break; // the next setup
            }
            if matches!(op.as_str(), "rts" | "jmp") {
                break;
            }
            let generated = arg.strip_prefix('F').is_some_and(|r| r.chars().all(|c| c.is_ascii_digit()));
            if (op == "jsr" && !generated) || (arg.contains(z.as_str()) && arg != through) {
                ok = false;
                break;
            }
            let writes = matches!(
                op.as_str(),
                "sta" | "stx" | "sty" | "inc" | "dec" | "asl" | "lsr" | "rol" | "ror"
            );
            if writes && (arg == p || arg.starts_with(&format!("{p}+"))) {
                ok = false;
                break;
            }
            end += 1;
        }
        // The removed loads also set A and the flags: what follows must load
        // A again before using either.
        if ok {
            ok = false;
            for l in &lines[i + 4..] {
                let Some((op, _)) = ins(l) else {
                    let t = l.trim();
                    if t.is_empty() || t.starts_with(';') {
                        continue;
                    }
                    break; // a label
                };
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
        if !ok {
            i += 1;
            continue;
        }
        let direct = format!("({p}),y");
        for l in &mut lines[i + 4..end] {
            if l.contains(&through) {
                *l = l.replace(&through, &direct);
            }
        }
        lines.drain(i..i + 4);
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Whether the program text can change machine state that the exit code
/// restores for BASIC: the interrupt setup, the memory configuration, the
/// VIC mode and bank, the SID.
///
/// The answer is "no" only if every write is to a place that is known and
/// harmless: a label of the program, or an absolute address outside the
/// CPU port, the interrupt vectors and the I/O area (the border and
/// background colours `$D020`-`$D02E` are not restored and do not count).
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
