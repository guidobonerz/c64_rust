//! The WiC64, a WiFi module for the user port: HTTP requests, TCP
//! connections and information about the network.
//!
//! ```ignore
//! use c64_wic64 as wic;
//!
//! if wic::present() {
//!     let n = wic::http_get("http://example.org/hello.txt", &mut buf)?;
//!     c64::text::print_petscii(&buf[..n]);
//! }
//! ```
//!
//! Every exchange is a request and a response: a command byte with a
//! payload goes out, a status and data come back. [`command`] does one
//! exchange; the functions for the usual commands are built on it. For a
//! response that is larger than your buffer, use [`request`] and then
//! [`read`] piece by piece ([`remaining`] says how much is left).
//!
//! Text is sent and comes back as it is: URLs are ASCII, and what a server
//! sends is not converted.
#![no_std]

/// An error: the status of the WiC64, or [`Error::NO_DEVICE`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Error(pub u8);

impl Error {
    /// The WiC64 could not do it (see [`status_message`]).
    pub const INTERNAL: Error = Error(1);
    /// The request was wrong: unknown command, bad URL, ...
    pub const CLIENT: Error = Error(2);
    /// No WiFi connection.
    pub const CONNECTION: Error = Error(3);
    /// The network or the remote host did not answer in time.
    pub const NETWORK: Error = Error(4);
    /// The server answered with an error (HTTP status outside 200-299).
    pub const SERVER: Error = Error(5);
    /// No WiC64 answered.
    pub const NO_DEVICE: Error = Error(255);
}

/// The command bytes ([`command`], [`request`]).
pub mod cmd {
    pub const VERSION: u8 = 0x00;
    pub const HTTP_GET: u8 = 0x01;
    pub const IP: u8 = 0x06;
    pub const SET_SERVER: u8 = 0x08;
    pub const SSID: u8 = 0x10;
    pub const RSSI: u8 = 0x11;
    pub const SERVER: u8 = 0x12;
    pub const MAC: u8 = 0x14;
    pub const LOCAL_TIME: u8 = 0x15;
    pub const TCP_OPEN: u8 = 0x21;
    pub const TCP_READ: u8 = 0x22;
    pub const TCP_WRITE: u8 = 0x23;
    pub const VERSION_NUMBERS: u8 = 0x26;
    pub const POST_URL: u8 = 0x28;
    pub const STATUS_MESSAGE: u8 = 0x2a;
    pub const POST_DATA: u8 = 0x2b;
    pub const IS_CONNECTED: u8 = 0x2c;
    pub const TCP_CLOSE: u8 = 0x2e;
    pub const TCP_AVAILABLE: u8 = 0x30;
    pub const REMOTE_TIMEOUT: u8 = 0x32;
    pub const ECHO: u8 = 0xfe;
}

/// How long a response may take, in units of 0.85 seconds.
static mut WAIT: u8 = 12;

#[cfg(target_os = "c64")]
c64::rat_source!(WIC64_RAT, "wic64.rat");

#[cfg(target_os = "c64")]
unsafe extern "C" {
    fn c64_wic64_request(command: u8, payload: *const u8, len: u16, wait: u8) -> u8;
    fn c64_wic64_read(buf: *mut u8, max: u16) -> u16;
    fn c64_wic64_remaining() -> u16;
}

/// Sets how long this crate waits for a response, in seconds (the default
/// is about 10). The WiC64 itself gives up on a server after 5 seconds;
/// see [`set_remote_timeout`].
pub fn set_wait(seconds: u8) {
    // 0.85 s per unit: seconds * 1.17, at least one.
    let units = seconds as u16 + (seconds as u16 >> 3) + 1;
    unsafe { WAIT = if units > 255 { 255 } else { units as u8 } };
}

/// Sends a request and receives the status of the response. Returns the
/// length of the response data, which [`read`] then delivers.
pub fn request(command: u8, payload: &[u8]) -> Result<u16, Error> {
    #[cfg(target_os = "c64")]
    {
        let st = unsafe { c64_wic64_request(command, payload.as_ptr(), payload.len() as u16, WAIT) };
        if st == 0 { Ok(remaining()) } else { Err(Error(st)) }
    }
    #[cfg(not(target_os = "c64"))]
    {
        let _ = (command, payload);
        Err(Error::NO_DEVICE)
    }
}

/// Reads the next bytes of the response into `buf` and returns how many
/// (fewer than `buf.len()` at the end of the response).
pub fn read(buf: &mut [u8]) -> usize {
    #[cfg(target_os = "c64")]
    {
        (unsafe { c64_wic64_read(buf.as_mut_ptr(), buf.len() as u16) }) as usize
    }
    #[cfg(not(target_os = "c64"))]
    {
        let _ = buf;
        0
    }
}

/// The bytes of the response that have not been read. The next request
/// drops them.
pub fn remaining() -> u16 {
    #[cfg(target_os = "c64")]
    unsafe {
        c64_wic64_remaining()
    }
    #[cfg(not(target_os = "c64"))]
    0
}

/// One exchange: sends `command` with `payload` and reads the response
/// into `out` (what does not fit is dropped). Returns the number of bytes.
pub fn command(command: u8, payload: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    request(command, payload)?;
    Ok(read(out))
}

/// Like [`command`], for responses that are text ending with a zero byte:
/// the length is that of the text.
fn text(c: u8, payload: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    let n = command(c, payload, out)?;
    Ok(if n > 0 && out[n - 1] == 0 { n - 1 } else { n })
}

/// Whether a WiC64 answers. Waits about a second if there is none.
pub fn present() -> bool {
    #[cfg(target_os = "c64")]
    {
        (unsafe { c64_wic64_request(cmd::VERSION, core::ptr::null(), 0, 1) }) == 0
    }
    #[cfg(not(target_os = "c64"))]
    false
}

/// The firmware version as text (`"2.1.0"`).
pub fn version(out: &mut [u8]) -> Result<usize, Error> {
    text(cmd::VERSION, &[], out)
}

/// Whether the WiC64 is connected to a WiFi network.
pub fn is_connected() -> bool {
    request(cmd::IS_CONNECTED, &[]).is_ok()
}

/// The IP address as text (`"192.168.1.23"`).
pub fn ip(out: &mut [u8]) -> Result<usize, Error> {
    text(cmd::IP, &[], out)
}

/// The MAC address as text.
pub fn mac(out: &mut [u8]) -> Result<usize, Error> {
    text(cmd::MAC, &[], out)
}

/// The name of the WiFi network.
pub fn ssid(out: &mut [u8]) -> Result<usize, Error> {
    text(cmd::SSID, &[], out)
}

/// The signal strength as text (`"-61dBm"`).
pub fn rssi(out: &mut [u8]) -> Result<usize, Error> {
    text(cmd::RSSI, &[], out)
}

/// The local time as text: `"HH:MM:SS DD-MM-YYYY"`.
pub fn local_time(out: &mut [u8]) -> Result<usize, Error> {
    text(cmd::LOCAL_TIME, &[], out)
}

/// The text of the last error (up to 40 bytes).
pub fn status_message(out: &mut [u8]) -> Result<usize, Error> {
    text(cmd::STATUS_MESSAGE, &[1], out)
}

/// Sets how long the WiC64 waits for a server, in seconds, for the next
/// request (then it is 5 seconds again).
pub fn set_remote_timeout(seconds: u8) -> Result<(), Error> {
    request(cmd::REMOTE_TIMEOUT, &[seconds]).map(|_| ())
}

/// The default server: a URL that starts with `!` is relative to it.
pub fn server(out: &mut [u8]) -> Result<usize, Error> {
    text(cmd::SERVER, &[], out)
}

/// Sets the default server (`"http://example.org/c64/"`).
pub fn set_server(url: &str) -> Result<(), Error> {
    request(cmd::SET_SERVER, url.as_bytes()).map(|_| ())
}

/// Fetches a URL (`http://`) and reads the body into `out`. Returns the
/// number of bytes read; if the body is larger, use
/// `request(cmd::HTTP_GET, url)` and [`read`] it in pieces.
pub fn http_get(url: &str, out: &mut [u8]) -> Result<usize, Error> {
    command(cmd::HTTP_GET, url.as_bytes(), out)
}

/// Posts `data` to a URL and reads the answer into `out`.
pub fn http_post(url: &str, data: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    request(cmd::POST_URL, url.as_bytes())?;
    command(cmd::POST_DATA, data, out)
}

/// Opens the TCP connection (there is one at a time): `"example.org:80"`.
pub fn tcp_open(host_and_port: &str) -> Result<(), Error> {
    request(cmd::TCP_OPEN, host_and_port.as_bytes()).map(|_| ())
}

/// The number of bytes that have arrived on the TCP connection.
pub fn tcp_available() -> Result<u16, Error> {
    let mut n = [0u8; 2];
    command(cmd::TCP_AVAILABLE, &[], &mut n)?;
    Ok(u16::from_le_bytes(n))
}

/// Reads what has arrived on the TCP connection.
/// `Err(Error::NETWORK)`: the connection is closed.
pub fn tcp_read(out: &mut [u8]) -> Result<usize, Error> {
    command(cmd::TCP_READ, &[], out)
}

/// Sends data on the TCP connection.
pub fn tcp_write(data: &[u8]) -> Result<(), Error> {
    request(cmd::TCP_WRITE, data).map(|_| ())
}

/// Closes the TCP connection.
pub fn tcp_close() -> Result<(), Error> {
    request(cmd::TCP_CLOSE, &[]).map(|_| ())
}

/// Sends `data` and gets it back: a test of the connection to the WiC64.
pub fn echo(data: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    command(cmd::ECHO, data, out)
}
