//! Raw memory access: `peek`, `poke`, fast `copy` and `fill`, and slices
//! over any address range.

/// A pointer to the C64 address `a` (on the host: into the simulated
/// memory).
#[inline(always)]
pub fn addr(a: u16) -> *mut u8 {
    #[cfg(target_os = "c64")]
    {
        a as usize as *mut u8
    }
    #[cfg(not(target_os = "c64"))]
    {
        unsafe { (&raw mut host::MEM).cast::<u8>().add(a as usize) }
    }
}

/// Reads a byte.
#[inline(always)]
pub fn peek(a: u16) -> u8 {
    unsafe { core::ptr::read_volatile(addr(a)) }
}

/// Writes a byte.
#[inline(always)]
pub fn poke(a: u16, v: u8) {
    unsafe { core::ptr::write_volatile(addr(a), v) }
}

/// Reads a little-endian 16-bit word.
pub fn peek_word(a: u16) -> u16 {
    peek(a) as u16 | (peek(a.wrapping_add(1)) as u16) << 8
}

/// Writes a little-endian 16-bit word.
pub fn poke_word(a: u16, v: u16) {
    poke(a, v as u8);
    poke(a.wrapping_add(1), (v >> 8) as u8);
}

/// The `n` bytes at `a` as a slice.
///
/// # Safety
/// The memory must not be accessed through other references while the
/// slice is in use (aliasing rules), and the range must not wrap.
pub unsafe fn bytes(a: u16, n: u16) -> &'static mut [u8] {
    unsafe { core::slice::from_raw_parts_mut(addr(a), n as usize) }
}

/// Copies `n` bytes from `src` to `dst`, front to back (so moving data to a
/// lower address is fine even when the ranges overlap).
pub fn copy(dst: u16, src: u16, n: u16) {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_mem_copy(dst, src, n)
    }
    #[cfg(not(target_os = "c64"))]
    for i in 0..n {
        poke(dst.wrapping_add(i), peek(src.wrapping_add(i)));
    }
}

/// Sets `n` bytes at `dst` to `v`.
pub fn fill(dst: u16, n: u16, v: u8) {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_mem_fill(dst, n, v)
    }
    #[cfg(not(target_os = "c64"))]
    for i in 0..n {
        poke(dst.wrapping_add(i), v);
    }
}

#[cfg(target_os = "c64")]
crate::rat_source!(MEM_RAT, "rat/mem.rat");

#[cfg(target_os = "c64")]
unsafe extern "C" {
    fn c64_mem_copy(dst: u16, src: u16, n: u16);
    fn c64_mem_fill(dst: u16, n: u16, v: u8);
}

#[cfg(not(target_os = "c64"))]
pub(crate) mod host {
    /// The simulated C64 memory.
    pub static mut MEM: [u8; 0x10000] = [0; 0x10000];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_fill_word() {
        fill(0xc000, 4, 7);
        assert_eq!(peek(0xc003), 7);
        poke_word(0xc000, 0x1234);
        copy(0xc010, 0xc000, 4);
        assert_eq!(peek_word(0xc010), 0x1234);
        assert_eq!(peek(0xc012), 7);
    }
}
