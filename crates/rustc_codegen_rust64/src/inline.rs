//! Inlining at the end: the finished assembly of a function replaces the
//! calls of it.
//!
//! rustc's own inlining works on the MIR and is done when the code
//! generator starts. What it leaves are functions that only show to be
//! small, or to have a single caller, in the finished program: a function
//! whose arguments are constants in all callers, a copy of a generic
//! function per context, a wrapper around an assembly routine. A call costs
//! four bytes and twelve cycles (`jsr`, `rts`) and hides what the function
//! does with the registers from the caller.
//!
//! This module only works on the text: it finds the calls (`sites`), tells
//! whether the text of a function can stand in another function (`body`)
//! and writes it there (`paste`). The decisions, and the passes over the
//! callers afterwards, are in `codegen.rs` (`inline_fns`).
//!
//! Frames are static, so the function's frame stays where it is: the
//! callers store the arguments there as before, and the text that is now
//! in their middle uses it.

use std::collections::{HashMap, HashSet};

use crate::asm::{fresh_label, line_size};

/// The assembly of one function: `lines[0]` is its label `F<id>:`. `pre`
/// are the comment lines in front of it.
pub struct Func {
    pub id: usize,
    pub pre: Vec<String>,
    pub lines: Vec<String>,
    /// Still a function of the program (not inlined everywhere).
    pub live: bool,
}

struct Parsed<'a> {
    label: Option<&'a str>,
    /// Lower case; empty if the line has no instruction.
    op: String,
    arg: &'a str,
}

/// A line as label, instruction and operand; `None` if it is not
/// understood.
fn parse(l: &str) -> Option<Parsed<'_>> {
    let code = l.split(';').next().unwrap_or("");
    let mut rest = code.trim();
    let mut label = None;
    if !rest.is_empty() && !l.starts_with([' ', '\t']) {
        let (name, r) = rest.split_once(':')?;
        let name = name.trim();
        if name.is_empty() || name.contains(char::is_whitespace) {
            return None;
        }
        label = Some(name);
        rest = r.trim();
    }
    let (op, arg) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    Some(Parsed { label, op: op.to_ascii_lowercase(), arg: arg.trim() })
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// The function a label `F<id>` names.
fn fn_id(name: &str) -> Option<usize> {
    let d = name.strip_prefix('F')?;
    if digits(d) { d.parse().ok() } else { None }
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).filter(|w| !w.is_empty())
}

/// The functions that `text` names.
pub fn named(text: &str, out: &mut HashSet<usize>) {
    for line in text.lines() {
        out.extend(words(line.split(';').next().unwrap_or("")).filter_map(fn_id));
    }
}

/// The label `F<id>_<kind><n>` at the start (`i`) or end (`e`) of an
/// `asm!` block.
fn asm_label(name: &str, id: usize, kind: u8) -> bool {
    name.strip_prefix(&format!("F{id}_")).is_some_and(|r| r.len() > 1 && r.as_bytes()[0] == kind && digits(&r[1..]))
}

fn is_branch(op: &str) -> bool {
    matches!(op, "bcc" | "bcs" | "beq" | "bne" | "bpl" | "bmi" | "bvc" | "bvs")
}

/// A call of a function of the program: `jsr F<n>`, or `jmp F<n>` at the
/// end (`tail`).
pub struct Site {
    pub line: usize,
    pub callee: usize,
    pub tail: bool,
}

/// The calls in `f`. A function that the text names in any other way (its
/// address, a call in an `asm!` block) goes to `pinned`.
pub fn sites(f: &Func, pinned: &mut HashSet<usize>) -> Vec<Site> {
    let mut found = Vec::new();
    let mut in_asm = false;
    for (line, l) in f.lines.iter().enumerate().skip(1) {
        let Some(p) = parse(l) else {
            named(l, pinned);
            continue;
        };
        if let Some(name) = p.label {
            if asm_label(name, f.id, b'i') {
                in_asm = true;
            } else if asm_label(name, f.id, b'e') {
                in_asm = false;
            }
        }
        match fn_id(p.arg) {
            Some(callee) if matches!(p.op.as_str(), "jsr" | "jmp") && !in_asm && callee != f.id => {
                found.push(Site { line, callee, tail: p.op == "jmp" });
            }
            _ => pinned.extend(words(p.arg).filter_map(fn_id)),
        }
    }
    found
}

/// The text of a function that can replace a call of it.
pub struct Body {
    id: usize,
    lines: Vec<(Option<String>, Option<(String, String)>)>,
    labels: HashSet<String>,
    /// The index in `lines` of the last instruction.
    last: usize,
    /// Bytes of the function.
    pub size: i64,
    /// Bytes of the text that replaces a `jsr`.
    pub inline_size: i64,
}

impl Body {
    /// The instruction leaves the function for another routine that
    /// returns for it (`jsr x` / `rts` written as `jmp x`).
    fn leaves(&self, op: &str, arg: &str) -> bool {
        op == "jmp" && !self.labels.contains(arg) && arg != "rt_panic"
    }
}

/// The text of `f` if it can stand in the middle of another function:
/// nothing but instructions and labels of its own, every branch stays
/// inside, and no part of it is called (then every `rts` returns from the
/// function, and every jump out of it is a call at the end).
pub fn body(f: &Func) -> Option<Body> {
    let entry = parse(f.lines.first()?)?;
    if entry.label != Some(format!("F{}", f.id).as_str()) || !entry.op.is_empty() {
        return None;
    }
    let mut b =
        Body { id: f.id, lines: Vec::new(), labels: HashSet::new(), last: 0, size: 0, inline_size: 0 };
    for l in &f.lines[1..] {
        let p = parse(l)?;
        if let Some(name) = p.label {
            let own = name.starts_with('@')
                || name.strip_prefix("_L").or_else(|| name.strip_prefix("_C")).is_some_and(digits)
                || name
                    .strip_prefix(&format!("F{}_", f.id))
                    .is_some_and(|r| r.len() > 1 && matches!(r.as_bytes()[0], b'b' | b'i' | b'e') && digits(&r[1..]));
            if !own || !b.labels.insert(name.to_owned()) {
                return None;
            }
        }
        if p.op.is_empty() {
            if let Some(name) = p.label {
                b.lines.push((Some(name.to_owned()), None));
            }
            continue;
        }
        if !crate::opt::MNEMONICS.contains(&p.op.as_str()) || matches!(p.op.as_str(), "rti" | "tsx" | "txs") {
            return None;
        }
        b.last = b.lines.len();
        b.lines.push((p.label.map(str::to_owned), Some((p.op, p.arg.to_owned()))));
    }
    if !b.lines.iter().any(|(_, ins)| ins.is_some()) {
        return None;
    }
    for (i, (_, ins)) in b.lines.iter().enumerate() {
        let Some((op, arg)) = ins else { continue };
        let local = b.labels.contains(arg.as_str());
        if (is_branch(op) && !local) || (op == "jsr" && local) || (op == "jmp" && arg.starts_with('(')) {
            return None;
        }
        let size = line_size(op, arg, &|name| name.starts_with("zp_"));
        b.size += size;
        b.inline_size += match (op.as_str(), i == b.last) {
            ("rts", true) => 0,
            ("rts", false) => 3,
            _ if b.leaves(op, arg) && i != b.last => 6,
            _ => size,
        };
    }
    Some(b)
}

/// `arg` with the labels in `map` renamed.
fn rename(arg: &str, map: &HashMap<&str, String>) -> String {
    let mut out = String::with_capacity(arg.len() + 4);
    let mut start = None;
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    for (i, c) in arg.char_indices().chain(std::iter::once((arg.len(), ' '))) {
        if is_word(c) {
            start.get_or_insert(i);
            continue;
        }
        if let Some(s) = start.take() {
            let w = &arg[s..i];
            let local = arg[..s].ends_with('@');
            match map.get(w) {
                Some(new) if !local => out.push_str(new),
                _ => out.push_str(w),
            }
        }
        if i < arg.len() {
            out.push(c);
        }
    }
    out
}

/// The copy of the result that follows the call of `callee` at line `at`
/// of `lines`, the text of `caller`:
///
/// ```text
///     jsr F5
///     ldx #23
/// _C7:
///     lda L5_0-1,x
///     sta F1v14-1,x
///     dex
///     bne _C7
/// ```
///
/// if the text of the function can write its result to that place itself
/// once it stands there: the copy takes all `size` bytes of the result to
/// a variable of the caller that no pointer exists to at the call (`free`
/// tells so for a variable; otherwise no address of it is taken anywhere
/// in the caller), and nothing else in the caller names the result. The
/// variable is not in use while the function runs (the copy overwrites all
/// of it, and the function does not know the caller's variables), and it
/// lies above everything the function calls, as the caller's whole frame
/// does.
///
/// Returns the place and the number of lines of the copy.
pub fn result_copy(
    lines: &[String],
    at: usize,
    callee: usize,
    caller: usize,
    size: u64,
    free: &dyn Fn(&str) -> bool,
) -> Option<(String, usize)> {
    let ins = |i: usize| -> Option<(&str, &str)> {
        let l = lines.get(i)?;
        if !l.starts_with([' ', '\t']) {
            return None;
        }
        let t = l.split(';').next().unwrap_or("").trim();
        Some(t.split_once(' ').map_or((t, ""), |(op, arg)| (op, arg.trim())))
    };
    let (Some(("ldx", n)), Some(("lda", src)), Some(("sta", dst)), Some(("dex", "")), Some(("bne", back))) =
        (ins(at + 1), ins(at + 3), ins(at + 4), ins(at + 5), ins(at + 6))
    else {
        return None;
    };
    let n = n.strip_prefix('#')?;
    let n = match n.strip_prefix('$') {
        Some(h) => u64::from_str_radix(h, 16).ok()?,
        None => n.parse().ok()?,
    };
    if n != size || lines[at + 2].trim() != format!("{back}:") {
        return None;
    }
    let result = [format!("L{callee}_0"), format!("F{callee}v0")];
    let src = src.strip_suffix("-1,x")?;
    let src = src.strip_suffix("+0").unwrap_or(src);
    let dst = dst.strip_suffix("-1,x")?;
    if !result.iter().any(|r| r == src) {
        return None;
    }
    // a variable or a temporary of the caller
    let own = [format!("F{caller}v"), format!("F{caller}t")];
    let base = dst.split('+').next().unwrap_or(dst);
    if !own.iter().any(|o| base.strip_prefix(o.as_str()).is_some_and(|r| r.bytes().all(|b| b.is_ascii_digit())))
        || dst.contains(['-', '(', ','])
    {
        return None;
    }
    let free = free(base);
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let words = |l: &str, w: &str| -> usize {
        l.match_indices(w)
            .filter(|&(i, _)| {
                !l[..i].chars().next_back().is_some_and(is_word) && !l[i + w.len()..].chars().next().is_some_and(is_word)
            })
            .count()
    };
    for (i, l) in lines.iter().enumerate() {
        let l = l.split(';').next().unwrap_or("");
        // (the result: only in the copy)
        if i != at + 3 && result.iter().any(|r| words(l, r) > 0) {
            return None;
        }
        // (the address of the variable)
        if !free && words(l, base) > 0 && (l.contains("#<") || l.contains("#>")) {
            return None;
        }
    }
    Some((dst.to_owned(), 6))
}

/// The text of a function that is pasted (`paste`), writing its result to
/// `place` (see `result_copy`), and what the copy left in the registers.
pub fn with_result_at(text: Vec<String>, callee: usize, place: &str) -> Vec<String> {
    let (l, f) = (format!("L{callee}_0"), format!("F{callee}v0"));
    let mut map: HashMap<&str, String> = HashMap::new();
    map.insert(&l, place.to_owned());
    map.insert(&f, place.to_owned());
    let mut out: Vec<String> = text.iter().map(|line| rename(line, &map)).collect();
    out.push(format!("        lda {place}"));
    out.push("        ldx #$00".to_owned());
    out
}

/// The lines that replace a call of `b` in function `caller`: the text
/// with labels of its own. In place of a `jsr` every way out of the
/// function leads to the end of the text; in place of a `jmp` at the end
/// of the caller (`tail`) the text returns as it did.
pub fn paste(b: &Body, caller: usize, tail: bool) -> Vec<String> {
    let mut map: HashMap<&str, String> = HashMap::new();
    for (label, _) in &b.lines {
        let Some(name) = label else { continue };
        if name.starts_with('@') {
            continue;
        }
        let new = fresh_label();
        let n = &new[2..];
        let new = if name.starts_with("_C") {
            format!("_C{n}")
        } else if asm_label(name, b.id, b'i') {
            format!("F{caller}_i{n}")
        } else if asm_label(name, b.id, b'e') {
            format!("F{caller}_e{n}")
        } else {
            new
        };
        map.insert(name, new);
    }
    let end = fresh_label();
    let mut end_used = false;
    let mut out = Vec::new();
    for (i, (label, ins)) in b.lines.iter().enumerate() {
        if let Some(name) = label {
            out.push(format!("{}:", map.get(name.as_str()).unwrap_or(name)));
        }
        let Some((op, arg)) = ins else { continue };
        let last = i == b.last;
        if !tail && op == "rts" {
            if !last {
                out.push(format!("        jmp {end}"));
                end_used = true;
            }
        } else if !tail && b.leaves(op, arg) {
            out.push(format!("        jsr {arg}"));
            if !last {
                out.push(format!("        jmp {end}"));
                end_used = true;
            }
        } else if arg.is_empty() {
            out.push(format!("        {op}"));
        } else {
            out.push(format!("        {op} {}", rename(arg, &map)));
        }
    }
    if end_used {
        out.push(format!("{end}:"));
    }
    out
}
