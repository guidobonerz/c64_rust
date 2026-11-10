//! Proc macros of the `c64` crate (use them through `c64::...`).
//!
//! Written on plain `proc_macro` (no syn/quote): the inputs are simple.

use proc_macro::{Delimiter, Literal, Span, TokenStream, TokenTree};

fn error(msg: &str, span: Span) -> TokenStream {
    let mut t: TokenStream = format!("::core::compile_error!({msg:?});").parse().unwrap();
    t = t
        .into_iter()
        .map(|mut tt| {
            tt.set_span(span);
            tt
        })
        .collect();
    t
}

fn parse_int(s: &str) -> Option<u64> {
    let s = s.replace('_', "");
    // integer suffixes such as u16 / usize
    let s = ["u16", "u32", "usize", "u8"]
        .iter()
        .find_map(|suf| s.strip_suffix(suf))
        .unwrap_or(&s);
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix('$')) {
        u64::from_str_radix(h, 16).ok()
    } else {
        s.parse().ok()
    }
}

/// Marks the program's entry point:
///
/// ```ignore
/// #[c64::main]
/// fn main() { ... }
/// ```
#[proc_macro_attribute]
pub fn main(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return error("#[c64::main] takes no arguments", Span::call_site());
    }
    let mut name = None;
    let mut it = item.clone().into_iter();
    while let Some(t) = it.next() {
        if let TokenTree::Ident(i) = &t
            && i.to_string() == "fn"
        {
            if let Some(TokenTree::Ident(n)) = it.next() {
                name = Some(n.to_string());
            }
            break;
        }
    }
    let Some(name) = name else {
        return error("#[c64::main] goes on a function", Span::call_site());
    };
    let mut out = item;
    out.extend(
        format!(
            "#[doc(hidden)] #[unsafe(export_name = \"main\")] \
             pub extern \"C\" fn __c64_main() {{ {name}() }}"
        )
        .parse::<TokenStream>()
        .unwrap(),
    );
    out
}

/// Places a static at a fixed address:
///
/// ```ignore
/// #[c64::at(0x2000)]
/// static SPRITES: [u8; 128] = *include_bytes!("sprites.bin");   // loaded there
///
/// #[c64::at(0xc000)]
/// static mut BUFFER: MaybeUninit<[u8; 1024]> = MaybeUninit::uninit();  // only placed
/// ```
///
/// A static with data becomes part of the program file at that address
/// (`$0801` or above, above the program); an uninitialized one
/// (`MaybeUninit::uninit()`) only gets the address.
#[proc_macro_attribute]
pub fn at(attr: TokenStream, item: TokenStream) -> TokenStream {
    let s = attr.to_string();
    let Some(addr) = parse_int(s.trim()).filter(|&a| a <= 0xffff) else {
        return error("#[c64::at(ADDRESS)] needs an address 0..=0xffff", Span::call_site());
    };
    let mut out: TokenStream =
        format!("#[unsafe(link_section = \"at:${addr:04x}\")]").parse().unwrap();
    out.extend(item);
    out
}

/// Places a static in the zero page (`$02-$8f`, saved and restored around
/// the program): fast and usable as a `(zp),y` pointer from assembly.
///
/// ```ignore
/// #[c64::zp]
/// static mut COUNTER: u8 = 0;
/// ```
#[proc_macro_attribute]
pub fn zp(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return error("#[c64::zp] takes no arguments", Span::call_site());
    }
    let mut out: TokenStream = "#[unsafe(link_section = \"zp\")]".parse().unwrap();
    out.extend(item);
    out
}

/// Includes a PSID tune: the data is placed at its load address and a
/// `c64::sound::Tune` static is defined with the header's init and play
/// addresses:
///
/// ```ignore
/// c64::sid!(MUSIC, "music.sid");     // path relative to Cargo.toml
/// MUSIC.start(0);
/// ```
#[proc_macro]
pub fn sid(input: TokenStream) -> TokenStream {
    let toks: Vec<TokenTree> = input.into_iter().collect();
    let mut vis = String::new();
    let mut i = 0;
    while let Some(TokenTree::Ident(id)) = toks.get(i) {
        let s = id.to_string();
        if s == "pub" {
            vis.push_str("pub ");
            if let Some(TokenTree::Group(g)) = toks.get(i + 1)
                && g.delimiter() == Delimiter::Parenthesis
            {
                vis = format!("pub{} ", g);
                i += 1;
            }
            i += 1;
        } else {
            break;
        }
    }
    let (Some(TokenTree::Ident(name)), Some(TokenTree::Punct(comma)), Some(TokenTree::Literal(path))) =
        (toks.get(i), toks.get(i + 1), toks.get(i + 2))
    else {
        return error("usage: c64::sid!(NAME, \"file.sid\")", Span::call_site());
    };
    if comma.as_char() != ',' {
        return error("usage: c64::sid!(NAME, \"file.sid\")", comma.span());
    }
    let lit = path.to_string();
    let Some(rel) = lit.strip_prefix('"').and_then(|l| l.strip_suffix('"')) else {
        return error("the file name must be a string literal", path.span());
    };
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let file = std::path::Path::new(&dir).join(rel);
    let bytes = match std::fs::read(&file) {
        Ok(b) => b,
        Err(e) => return error(&format!("cannot read {}: {e}", file.display()), path.span()),
    };
    let sid = match parse_sid(&bytes) {
        Ok(s) => s,
        Err(e) => return error(&format!("{rel}: {e}"), path.span()),
    };
    let data: Vec<String> = sid.data.iter().map(|b| format!("{b}u8")).collect();
    let abs = Literal::string(&file.to_string_lossy()).to_string();
    let code = format!(
        "#[doc(hidden)] #[allow(non_upper_case_globals)] \
         #[unsafe(link_section = \"at:${loc:04x}\")] \
         static {name}__DATA: [u8; {n}] = [{data}]; \
         const _: &[u8] = include_bytes!({abs}); \
         {vis}static {name}: ::c64::sound::Tune = ::c64::sound::Tune {{ \
             init: {init}, play: {play}, songs: {songs}, start_song: {start}, \
             data: &{name}__DATA }};",
        loc = sid.location,
        n = sid.data.len(),
        data = data.join(","),
        init = sid.init,
        play = sid.play,
        songs = sid.songs.min(255),
        start = sid.start_song.saturating_sub(1).min(255),
    );
    code.parse().unwrap()
}

struct Sid {
    location: u16,
    init: u16,
    play: u16,
    songs: u16,
    start_song: u16,
    data: Vec<u8>,
}

fn parse_sid(b: &[u8]) -> Result<Sid, String> {
    if b.len() < 0x76 || (&b[..4] != b"PSID" && &b[..4] != b"RSID") {
        return Err("not a PSID/RSID file".into());
    }
    let be = |o: usize| u16::from_be_bytes([b[o], b[o + 1]]);
    let off = be(6) as usize;
    if off > b.len() {
        return Err("data offset beyond the file".into());
    }
    let mut location = be(8);
    let mut data = &b[off..];
    if location == 0 {
        if data.len() < 2 {
            return Err("data too short".into());
        }
        location = u16::from_le_bytes([data[0], data[1]]);
        data = &data[2..];
    }
    if location < 0x0801 {
        return Err(format!("the tune loads at ${location:04x}, below the program"));
    }
    let mut init = be(10);
    if init == 0 {
        init = location;
    }
    let play = be(12);
    if play == 0 {
        return Err("the tune installs its own interrupt (play address 0); not supported".into());
    }
    Ok(Sid { location, init, play, songs: be(14), start_song: be(16), data: data.to_vec() })
}
