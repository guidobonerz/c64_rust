//! Program control: exit to BASIC, the interrupt flag, calls into machine
//! code.

/// CPU registers for [`jsr`]. `p` is the status register after the call
/// (carry = bit 0, zero = bit 1, ...).
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Regs {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub p: u8,
}

impl Regs {
    /// The carry flag after the call (KERNAL routines report errors with it).
    pub fn carry(&self) -> bool {
        self.p & 1 != 0
    }
}

/// Returns to BASIC from anywhere.
pub fn exit() -> ! {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_sys_exit()
    }
    #[cfg(not(target_os = "c64"))]
    std::process::exit(0)
}

/// Stops the program like a failed check: red border, back to BASIC.
pub fn panic() -> ! {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_sys_panic()
    }
    #[cfg(not(target_os = "c64"))]
    panic!("c64::sys::panic")
}

/// Disables interrupts.
pub fn sei() {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_sys_sei()
    }
}

/// Enables interrupts.
pub fn cli() {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_sys_cli()
    }
}

/// Runs `f` with interrupts disabled and restores the previous state.
pub fn without_interrupts<R>(f: impl FnOnce() -> R) -> R {
    #[cfg(target_os = "c64")]
    {
        let p = unsafe { c64_sys_irq_save() };
        let r = f();
        unsafe { c64_sys_irq_restore(p) };
        r
    }
    #[cfg(not(target_os = "c64"))]
    f()
}

/// Calls the machine code routine at `addr` with A, X and Y from `regs`
/// and returns the registers (and flags) it ends with.
///
/// # Safety
/// `addr` must be a routine that ends with `rts` and leaves the program's
/// memory alone.
pub unsafe fn jsr(addr: u16, regs: Regs) -> Regs {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_sys_jsr(addr, regs.a, regs.x, regs.y)
    }
    #[cfg(not(target_os = "c64"))]
    {
        let _ = addr;
        regs
    }
}

#[cfg(target_os = "c64")]
crate::rat_source!(SYS_RAT, "rat/sys.rat");

#[cfg(target_os = "c64")]
unsafe extern "C" {
    fn c64_sys_exit() -> !;
    fn c64_sys_panic() -> !;
    fn c64_sys_sei();
    fn c64_sys_cli();
    fn c64_sys_irq_save() -> u8;
    fn c64_sys_irq_restore(p: u8);
    fn c64_sys_jsr(addr: u16, a: u8, x: u8, y: u8) -> Regs;
}
