//! Rust functions in raster interrupts.
//!
//! ```ignore
//! irq::add(50, top);       // at raster line 50
//! irq::add(200, bottom);   // at line 200
//! ```
//!
//! Handlers run in the order they were added and must be added with
//! ascending lines. After the last handler of each frame the KERNAL's
//! keyboard scan and jiffy clock run. If a handler runs past the next
//! handler's line, that handler follows immediately. The CIA timer
//! interrupt is off while handlers are installed.
//!
//! Handlers can use any Rust code: functions reached from them are compiled
//! as separate copies with their own static frames, and the dispatcher saves
//! the runtime registers they use. Statics are shared with the main program,
//! as usual.

/// The number of raster handlers.
pub const MAX_HANDLERS: usize = 8;

#[unsafe(export_name = "c64_irq_lines")]
static mut LINES: [u8; MAX_HANDLERS] = [0; MAX_HANDLERS];
#[unsafe(export_name = "c64_irq_handlers")]
static mut HANDLERS: [Option<fn()>; MAX_HANDLERS] = [None; MAX_HANDLERS];
#[unsafe(export_name = "c64_irq_count")]
static mut COUNT: u8 = 0;
#[unsafe(export_name = "c64_irq_cur")]
static mut CUR: u8 = 0;

/// Installs `f` at raster line `line` (0-255). Ignored when
/// [`MAX_HANDLERS`] are installed.
pub fn add(line: u8, f: fn()) {
    unsafe {
        if COUNT as usize == MAX_HANDLERS {
            return;
        }
        crate::sys::sei();
        LINES[COUNT as usize] = line;
        HANDLERS[COUNT as usize] = Some(f);
        COUNT += 1;
        install();
    }
}

/// Runs `f` once per frame at the bottom of the screen (line 251).
pub fn on_frame(f: fn()) {
    add(251, f);
}

/// Removes all handlers and restores the KERNAL interrupt.
pub fn stop() {
    uninstall();
    unsafe {
        COUNT = 0;
        CUR = 0;
    }
}

/// The number of installed handlers.
pub fn count() -> u8 {
    unsafe { COUNT }
}

/// Runs `f` with interrupts disabled ([`crate::sys::without_interrupts`]).
pub fn critical_section<R>(f: impl FnOnce() -> R) -> R {
    crate::sys::without_interrupts(f)
}

fn install() {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_irq_install()
    }
}

fn uninstall() {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_irq_uninstall()
    }
}

#[cfg(target_os = "c64")]
crate::rat_source!(IRQ_RAT, "rat/irq.rat");

#[cfg(target_os = "c64")]
unsafe extern "C" {
    fn c64_irq_install();
    fn c64_irq_uninstall();
}
