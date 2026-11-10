#![no_std]
#![no_main]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[unsafe(no_mangle)]
pub extern "C" fn main() {
    unsafe { core::ptr::write_volatile(0xd020 as *mut u8, 5) };
}
