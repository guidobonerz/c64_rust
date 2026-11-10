//! Text output for rat assembly and the memory operands of the generated code.

use std::fmt::Write;

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
}

impl Addr {
    pub fn offset(&self, n: u64) -> Addr {
        match self {
            Addr::Abs(e) if n == 0 => Addr::Abs(e.clone()),
            Addr::Abs(e) => Addr::Abs(format!("{e}+{n}")),
            Addr::Ind { ptr, off } => Addr::Ind { ptr: ptr.clone(), off: off + n },
        }
    }
}

/// A value: bytes in memory or immediate bytes (`#$05`, `#<label`).
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
}

/// An assembly text buffer with a label counter.
#[derive(Default)]
pub struct Asm {
    pub text: String,
    labels: usize,
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
        self.labels += 1;
        format!("_L{}", self.labels)
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
