//! The runtime (`runtime.rat`) and the call into rat.

use rat_asm::{MemVfs, Options};

pub const RUNTIME: &str = include_str!("runtime.rat");

/// Assembles a complete program (rat dialect, C64 platform) to a `.prg`
/// (`format` "prg") or a disk image with the program as `name` ("d64").
pub fn assemble(source: &str, format: &str, name: &str) -> Result<Vec<u8>, String> {
    let mut vfs = MemVfs::new();
    vfs.add("main.rat", source.as_bytes().to_vec());
    let opts = Options {
        main: "main.rat".into(),
        platform: Some("c64".into()),
        format: Some(format.into()),
        output: Some(format!("{name}.{format}")),
        ..Options::default()
    };
    let res = rat_asm::assemble(&opts, &vfs);
    if !res.ok || res.errors() > 0 {
        return Err(res.render_diagnostics(false));
    }
    res.outputs
        .into_iter()
        .next()
        .map(|o| o.data)
        .ok_or_else(|| "rat produced no output".to_owned())
}
