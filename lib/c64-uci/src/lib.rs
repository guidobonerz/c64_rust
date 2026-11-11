//! The Ultimate command interface (UCI) of the 1541 Ultimate-II(+) and the
//! Ultimate 64: files on the Ultimate's storage, TCP and UDP sockets, and
//! control of the machine, through four I/O registers at `$DF1C`.
//!
//! The interface must be switched on in the Ultimate's menu ("Command
//! Interface: Enabled"). Check with [`present`] first: the other functions
//! wait for the Ultimate's answer and never return without one.
//!
//! ```ignore
//! use c64_uci::{dos, net};
//!
//! if c64_uci::present() {
//!     dos::open("/Usb0/scores.txt", dos::READ)?;
//!     let n = dos::read(&mut buf)?;
//!     dos::close()?;
//!
//!     let s = net::tcp_connect("example.org", 80)?;
//!     net::write(s, b"GET / HTTP/1.0\r\n\r\n")?;
//!     let n = net::read(s, &mut buf)?;
//!     net::close(s)?;
//! }
//! ```
//!
//! Names and paths are ASCII, as the Ultimate shows them.
//!
//! Every command follows the same steps, which are also public for commands
//! this crate has no function for: [`begin`], parameters with [`put`] /
//! [`put_u16`] / [`put_bytes`], [`push`], then [`read`] and [`status`] (or
//! [`code`]) and [`accept`], which says whether another packet follows.
//! [`request`] does all of that for a command with one answer.
#![no_std]

/// The targets of commands.
pub mod target {
    /// Files (the first of two independent file interfaces).
    pub const DOS1: u8 = 1;
    pub const DOS2: u8 = 2;
    pub const NETWORK: u8 = 3;
    pub const CONTROL: u8 = 4;
}

/// An error: the number at the start of the Ultimate's status text
/// (`"82,FILE NOT FOUND"` is `Error(82)`; see the constants), or
/// [`Error::REJECTED`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Error(pub u8);

impl Error {
    pub const UNKNOWN_COMMAND: Error = Error(21);
    pub const FILE_NOT_FOUND: Error = Error(82);
    pub const NO_SUCH_DIRECTORY: Error = Error(83);
    pub const NO_FILE_TO_CLOSE: Error = Error(84);
    pub const NO_FILE_OPEN: Error = Error(85);
    pub const CANNOT_READ_DIRECTORY: Error = Error(86);
    pub const NO_INFORMATION: Error = Error(88);
    pub const PROHIBITED: Error = Error(98);
    pub const NOT_IMPLEMENTED: Error = Error(99);
    /// The interface did not take the command.
    pub const REJECTED: Error = Error(255);
}

#[cfg(target_os = "c64")]
c64::rat_source!(UCI_RAT, "uci.rat");

#[cfg(target_os = "c64")]
unsafe extern "C" {
    fn c64_uci_begin(target: u8, command: u8);
    fn c64_uci_accept() -> u8;
    fn c64_uci_put_bytes(data: *const u8, len: u16);
    fn c64_uci_push() -> u8;
    fn c64_uci_read(buf: *mut u8, max: u16) -> u16;
    fn c64_uci_status(buf: *mut u8, max: u8) -> u8;
    fn c64_uci_code() -> u8;
}

/// Whether the command interface is there and switched on.
pub fn present() -> bool {
    #[cfg(target_os = "c64")]
    {
        let id: u8;
        unsafe { c64::asm!("lda $df1d", "sta {id}", id = out(reg) id) };
        id == 0xc9
    }
    #[cfg(not(target_os = "c64"))]
    false
}

/// Starts a command for a [`target`]. What is left of the command before
/// (unread data, more packets) is dropped.
pub fn begin(target: u8, command: u8) {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_uci_begin(target, command)
    }
    #[cfg(not(target_os = "c64"))]
    let _ = (target, command);
}

/// Adds a parameter byte to the command.
pub fn put(b: u8) {
    #[cfg(target_os = "c64")]
    unsafe {
        c64::asm!("lda {b}", "sta $df1d", b = in(reg) b)
    }
    #[cfg(not(target_os = "c64"))]
    let _ = b;
}

/// Adds a 16-bit parameter (low byte first).
pub fn put_u16(v: u16) {
    #[cfg(target_os = "c64")]
    unsafe {
        c64::asm!("lda {v}", "sta $df1d", "lda {v}+1", "sta $df1d", v = in(reg) v)
    }
    #[cfg(not(target_os = "c64"))]
    let _ = v;
}

/// Adds bytes to the command (a name, data to write). A command can have
/// 896 bytes in all.
pub fn put_bytes(data: &[u8]) {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_uci_put_bytes(data.as_ptr(), data.len() as u16)
    }
    #[cfg(not(target_os = "c64"))]
    let _ = data;
}

/// Runs the command and waits for the answer.
pub fn push() -> Result<(), Error> {
    #[cfg(target_os = "c64")]
    {
        if unsafe { c64_uci_push() } & 0x08 != 0 { Err(Error::REJECTED) } else { Ok(()) }
    }
    #[cfg(not(target_os = "c64"))]
    Err(Error::REJECTED)
}

/// Reads the data of the current packet into `buf` and returns the number
/// of bytes. Data that does not fit can be read with another call;
/// [`accept`] drops it.
pub fn read(buf: &mut [u8]) -> usize {
    #[cfg(target_os = "c64")]
    {
        (unsafe { c64_uci_read(buf.as_mut_ptr(), buf.len() as u16) }) as usize
    }
    #[cfg(not(target_os = "c64"))]
    {
        let _ = buf;
        0
    }
}

/// Reads the status text of the current packet (`"00,OK"`) and returns its
/// length.
pub fn status(buf: &mut [u8]) -> usize {
    #[cfg(target_os = "c64")]
    {
        let max = if buf.len() > 255 { 255 } else { buf.len() as u8 };
        (unsafe { c64_uci_status(buf.as_mut_ptr(), max) }) as usize
    }
    #[cfg(not(target_os = "c64"))]
    {
        let _ = buf;
        0
    }
}

/// The number at the start of the status text; 0 if the packet has no
/// status (data packets of a longer answer). Uses up the status text.
pub fn code() -> u8 {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_uci_code()
    }
    #[cfg(not(target_os = "c64"))]
    0
}

/// Accepts the current packet. Returns whether another packet follows.
pub fn accept() -> bool {
    #[cfg(target_os = "c64")]
    {
        (unsafe { c64_uci_accept() }) != 0
    }
    #[cfg(not(target_os = "c64"))]
    false
}

/// After [`push`]: collects the data of all packets in `out` (what does
/// not fit is dropped) and returns its length, or the first error.
pub fn finish(out: &mut [u8]) -> Result<usize, Error> {
    let mut n = 0;
    let mut err = 0;
    loop {
        n += read(&mut out[n..]);
        let c = code();
        if err == 0 {
            err = c;
        }
        if !accept() {
            break;
        }
    }
    if err == 0 { Ok(n) } else { Err(Error(err)) }
}

/// Runs a whole command: `params` after the target and command bytes, the
/// answer in `out`. Returns the length of the answer.
pub fn request(target: u8, command: u8, params: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    begin(target, command);
    put_bytes(params);
    push()?;
    finish(out)
}

/// Files and directories on the Ultimate's storage (target
/// [`target::DOS1`]). One file can be open at a time.
pub mod dos {
    use super::{Error, begin, finish, push, put, put_bytes, put_u16, target::DOS1};

    /// Flags of [`open`].
    pub const READ: u8 = 0x01;
    pub const WRITE: u8 = 0x02;
    /// Create the file; fail if it exists.
    pub const CREATE_NEW: u8 = 0x04;
    /// Create the file; replace it if it exists.
    pub const CREATE_ALWAYS: u8 = 0x08;

    fn run(command: u8, name: &str, out: &mut [u8]) -> Result<usize, Error> {
        begin(DOS1, command);
        put_bytes(name.as_bytes());
        push()?;
        finish(out)
    }

    /// The name and version of the file interface
    /// (`"ULTIMATE-II DOS V1.2"`).
    pub fn identify(out: &mut [u8]) -> Result<usize, Error> {
        run(0x01, "", out)
    }

    /// Opens a file: `dos::open("/Usb0/data.bin", dos::READ)`,
    /// `dos::open("new.txt", dos::WRITE | dos::CREATE_ALWAYS)`.
    pub fn open(name: &str, flags: u8) -> Result<(), Error> {
        begin(DOS1, 0x02);
        put(flags);
        put_bytes(name.as_bytes());
        push()?;
        finish(&mut []).map(|_| ())
    }

    /// Closes the open file.
    pub fn close() -> Result<(), Error> {
        run(0x03, "", &mut []).map(|_| ())
    }

    /// Reads up to `out.len()` bytes from the open file and returns how
    /// many there were (fewer at the end of the file).
    pub fn read(out: &mut [u8]) -> Result<usize, Error> {
        begin(DOS1, 0x04);
        put_u16(if out.len() > 0xffff { 0xffff } else { out.len() as u16 });
        push()?;
        finish(out)
    }

    /// Writes `data` to the open file.
    pub fn write(data: &[u8]) -> Result<(), Error> {
        // A command holds 896 bytes, four of them the header.
        let mut rest = data;
        while !rest.is_empty() {
            let n = if rest.len() > 512 { 512 } else { rest.len() };
            let (now, later) = rest.split_at(n);
            begin(DOS1, 0x05);
            put_u16(0);
            put_bytes(now);
            push()?;
            finish(&mut [])?;
            rest = later;
        }
        Ok(())
    }

    /// Moves to a position in the open file.
    pub fn seek(pos: u32) -> Result<(), Error> {
        begin(DOS1, 0x06);
        put_bytes(&pos.to_le_bytes());
        push()?;
        finish(&mut []).map(|_| ())
    }

    /// Information about the open file: size (4 bytes, low byte first),
    /// date and time (2 bytes each, FAT format), extension (3 bytes),
    /// attributes (1 byte), then the name.
    pub fn file_info(out: &mut [u8]) -> Result<usize, Error> {
        run(0x07, "", out)
    }

    /// The size of the open file.
    pub fn file_size() -> Result<u32, Error> {
        let mut info = [0u8; 12];
        file_info(&mut info)?;
        Ok(u32::from_le_bytes([info[0], info[1], info[2], info[3]]))
    }

    /// Deletes a file.
    pub fn delete(name: &str) -> Result<(), Error> {
        run(0x09, name, &mut []).map(|_| ())
    }

    /// Changes the current directory.
    pub fn cd(path: &str) -> Result<(), Error> {
        run(0x11, path, &mut []).map(|_| ())
    }

    /// The current directory.
    pub fn path(out: &mut [u8]) -> Result<usize, Error> {
        run(0x12, "", out)
    }

    /// Creates a directory.
    pub fn mkdir(name: &str) -> Result<(), Error> {
        run(0x16, name, &mut []).map(|_| ())
    }

    /// Opens the current directory for [`read_dir`].
    pub fn open_dir() -> Result<(), Error> {
        run(0x13, "", &mut []).map(|_| ())
    }

    /// After [`open_dir`]: starts reading the entries. Each packet is one
    /// entry, an attribute byte (bit 4: directory) and the name: read it
    /// with [`super::read`], go on with [`super::accept`] while that
    /// returns true.
    pub fn read_dir() -> Result<(), Error> {
        begin(DOS1, 0x14);
        push()
    }

    /// Mounts a disk image on the Ultimate's drive with IEC number `drive`
    /// (0: the drive that was used last).
    pub fn mount(drive: u8, image: &str) -> Result<(), Error> {
        begin(DOS1, 0x23);
        put(drive);
        put_bytes(image.as_bytes());
        push()?;
        finish(&mut []).map(|_| ())
    }

    /// Removes the disk from a drive.
    pub fn unmount(drive: u8) -> Result<(), Error> {
        begin(DOS1, 0x24);
        put(drive);
        push()?;
        finish(&mut []).map(|_| ())
    }

    /// The date and time of the Ultimate's clock: `"2026/11/11 12:34:56"`.
    pub fn time(out: &mut [u8]) -> Result<usize, Error> {
        run(0x26, "", out)
    }
}

/// TCP and UDP through the Ultimate's network (target
/// [`target::NETWORK`]).
pub mod net {
    use super::{Error, begin, finish, push, put, put_bytes, put_u16, target::NETWORK};

    /// The name and version of the network interface.
    pub fn identify(out: &mut [u8]) -> Result<usize, Error> {
        begin(NETWORK, 0x01);
        push()?;
        finish(out)
    }

    /// The number of network interfaces.
    pub fn interface_count() -> Result<u8, Error> {
        let mut n = [0u8; 1];
        begin(NETWORK, 0x02);
        push()?;
        finish(&mut n)?;
        Ok(n[0])
    }

    /// The addresses of interface `interface` (0 = the first): IP address,
    /// netmask and gateway, 4 bytes each.
    pub fn ip_address(interface: u8) -> Result<[u8; 12], Error> {
        let mut a = [0u8; 12];
        begin(NETWORK, 0x05);
        put(interface);
        push()?;
        finish(&mut a)?;
        Ok(a)
    }

    fn connect(command: u8, host: &str, port: u16) -> Result<u8, Error> {
        let mut s = [0u8; 1];
        begin(NETWORK, command);
        put_u16(port);
        put_bytes(host.as_bytes());
        put(0);
        push()?;
        finish(&mut s)?;
        Ok(s[0])
    }

    /// Opens a TCP connection and returns the socket.
    pub fn tcp_connect(host: &str, port: u16) -> Result<u8, Error> {
        connect(0x07, host, port)
    }

    /// Opens a UDP socket to a host.
    pub fn udp_connect(host: &str, port: u16) -> Result<u8, Error> {
        connect(0x08, host, port)
    }

    /// Closes a socket.
    pub fn close(socket: u8) -> Result<(), Error> {
        begin(NETWORK, 0x09);
        put(socket);
        push()?;
        finish(&mut []).map(|_| ())
    }

    /// Reads what has arrived on a socket, up to `out.len()` bytes (and at
    /// most 510 at a time), and returns how many bytes. `Err(Error(1))`:
    /// the other side closed the connection; `Err(Error(2))`: nothing has
    /// arrived yet.
    pub fn read(socket: u8, out: &mut [u8]) -> Result<usize, Error> {
        let want = if out.len() > 510 { 510 } else { out.len() as u16 };
        begin(NETWORK, 0x10);
        put(socket);
        put_u16(want);
        push()?;
        // The answer starts with the number of bytes.
        let mut count = [0u8; 2];
        super::read(&mut count);
        let n = u16::from_le_bytes(count) as usize;
        let n = if n > out.len() { out.len() } else { n };
        super::read(&mut out[..n]);
        let c = super::code();
        while super::accept() {}
        if c == 0 { Ok(n) } else { Err(Error(c)) }
    }

    /// Sends `data` on a socket and returns how many bytes were taken.
    pub fn write(socket: u8, data: &[u8]) -> Result<usize, Error> {
        let mut sent = 0;
        let mut rest = data;
        while !rest.is_empty() {
            let n = if rest.len() > 512 { 512 } else { rest.len() };
            let (now, later) = rest.split_at(n);
            let mut count = [0u8; 2];
            begin(NETWORK, 0x11);
            put(socket);
            put_bytes(now);
            push()?;
            finish(&mut count)?;
            sent += u16::from_le_bytes(count) as usize;
            rest = later;
        }
        Ok(sent)
    }
}

/// The machine itself (target [`target::CONTROL`]).
pub mod control {
    use super::{Error, begin, finish, push, put, target::CONTROL};

    /// The name and version of the control interface.
    pub fn identify(out: &mut [u8]) -> Result<usize, Error> {
        begin(CONTROL, 0x01);
        push()?;
        finish(out)
    }

    /// The name of the hardware (`"ULTIMATE 64"`).
    pub fn hardware(out: &mut [u8]) -> Result<usize, Error> {
        begin(CONTROL, 0x28);
        put(0);
        push()?;
        finish(out)
    }

    /// Restarts the C64. Does not return if it works.
    pub fn reboot() -> Result<(), Error> {
        begin(CONTROL, 0x06);
        push()
    }
}
