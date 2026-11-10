//! `cargo c64`: builds Rust programs for the Commodore 64 with the rust64
//! codegen backend.
//!
//! ```text
//! cargo c64 build [--release] [-o FILE] [cargo args...]
//! cargo c64 run   [--release] [cargo args...]     build and start mimic
//! cargo c64 asm   [--release] [cargo args...]     build and print the assembly
//! ```
//!
//! Tools: the backend (`RUST64_BACKEND`, default: next to this executable),
//! mimic (`RUST64_MIMIC`, default: `mimic` on the PATH).

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The nightly the backend is built against (rust-toolchain.toml).
const TOOLCHAIN: &str = "nightly-2026-11-10";
const TARGET_SPEC: &str = include_str!("../../../target-spec/mos-c64.json");
const TARGET_NAME: &str = "mos-c64";

fn main() -> ExitCode {
    let mut args: Vec<String> = env::args().skip(1).collect();
    // `cargo c64 ...` passes "c64" as the first argument.
    if args.first().map(String::as_str) == Some("c64") {
        args.remove(0);
    }
    let Some(cmd) = args.first().cloned() else {
        eprintln!("usage: cargo c64 <build|run|asm> [--release] [-o FILE] [cargo args...]");
        return ExitCode::from(2);
    };
    args.remove(0);
    match cmd.as_str() {
        "build" | "run" | "asm" => match build(&cmd, args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        "-h" | "--help" | "help" => {
            println!("cargo c64 <build|run|asm> [--release] [-o FILE] [cargo args...]");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("error: unknown command `{other}`");
            ExitCode::from(2)
        }
    }
}

fn backend() -> Result<PathBuf, String> {
    if let Some(p) = env::var_os("RUST64_BACKEND") {
        return Ok(PathBuf::from(p));
    }
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().unwrap();
    let name = if cfg!(windows) {
        "rustc_codegen_rust64.dll"
    } else if cfg!(target_os = "macos") {
        "librustc_codegen_rust64.dylib"
    } else {
        "librustc_codegen_rust64.so"
    };
    let p = dir.join(name);
    if p.is_file() {
        Ok(p)
    } else {
        Err(format!("backend {} not found (set RUST64_BACKEND)", p.display()))
    }
}

/// Finds the package root (directory of the nearest Cargo.toml).
fn package_root() -> Result<PathBuf, String> {
    let mut dir = env::current_dir().map_err(|e| e.to_string())?;
    loop {
        if dir.join("Cargo.toml").is_file() {
            return Ok(dir);
        }
        if !dir.pop() {
            return Err("no Cargo.toml found".into());
        }
    }
}

fn package_name(root: &Path) -> Option<String> {
    let toml = std::fs::read_to_string(root.join("Cargo.toml")).ok()?;
    let mut in_package = false;
    for line in toml.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_package = l == "[package]";
        } else if in_package && l.starts_with("name") {
            return l.split('=').nth(1).map(|v| v.trim().trim_matches('"').to_owned());
        }
    }
    None
}

fn build(cmd: &str, mut args: Vec<String>) -> Result<(), String> {
    let mut output = None;
    if let Some(i) = args.iter().position(|a| a == "-o") {
        output = Some(PathBuf::from(args.get(i + 1).ok_or("-o needs a file")?));
        args.drain(i..i + 2);
    }
    let release = args.iter().any(|a| a == "--release" || a == "-r");
    let root = package_root()?;
    let target_dir = root.join("target");
    let spec_dir = target_dir.join("rust64");
    std::fs::create_dir_all(&spec_dir).map_err(|e| e.to_string())?;
    let spec = spec_dir.join(format!("{TARGET_NAME}.json"));
    if std::fs::read_to_string(&spec).ok().as_deref() != Some(TARGET_SPEC) {
        std::fs::write(&spec, TARGET_SPEC).map_err(|e| e.to_string())?;
    }

    let backend = backend()?;
    let name = package_name(&root).ok_or("no package name in Cargo.toml")?;
    let profile = if release { "release" } else { "debug" };
    rebuild_if_backend_changed(&root, &spec, &backend, &name, profile, release)?;
    let mut rustflags = format!(
        "-Zcodegen-backend={} -Zalways-encode-mir -Zinline-mir",
        backend.display()
    );
    if let Ok(extra) = env::var("RUSTFLAGS") {
        rustflags = format!("{extra} {rustflags}");
    }

    let status = Command::new("cargo")
        .env("RUSTUP_TOOLCHAIN", TOOLCHAIN)
        .env("RUSTFLAGS", rustflags)
        .current_dir(&root)
        .args(["build", "-Zbuild-std=core", "-Zjson-target-spec", "--target"])
        .arg(&spec)
        .args(&args)
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if !status.success() {
        return Err("build failed".into());
    }

    let out_dir = target_dir.join(TARGET_NAME).join(profile);
    let prg = out_dir.join(format!("{name}.prg"));
    let prg = match output {
        Some(o) => {
            std::fs::copy(&prg, &o).map_err(|e| format!("cannot copy {}: {e}", prg.display()))?;
            o
        }
        None => prg,
    };
    eprintln!("    wrote {}", prg.display());

    match cmd {
        "asm" => {
            let asm = find_asm(&out_dir, &name).ok_or("no assembly listing found")?;
            print!("{}", std::fs::read_to_string(asm).map_err(|e| e.to_string())?);
        }
        "run" => {
            let mimic = env::var("RUST64_MIMIC").unwrap_or_else(|_| "mimic".into());
            Command::new(&mimic)
                .args(["--machine", "c64", "--autostart", "--prg"])
                .arg(&prg)
                .status()
                .map_err(|e| format!("cannot start {mimic}: {e} (set RUST64_MIMIC)"))?;
        }
        _ => {}
    }
    Ok(())
}

/// Cargo does not notice a new backend (same path in RUSTFLAGS), so the
/// program's own crate is cleaned when the backend file changed. The
/// libraries (core, c64) only carry MIR and need no rebuild.
fn rebuild_if_backend_changed(
    root: &Path,
    spec: &Path,
    backend: &Path,
    name: &str,
    profile: &str,
    release: bool,
) -> Result<(), String> {
    let stamp = root.join("target").join("rust64").join(format!("backend-{profile}.stamp"));
    let meta = std::fs::metadata(backend).map_err(|e| e.to_string())?;
    let id = format!(
        "{:?} {}",
        meta.modified().map_err(|e| e.to_string())?,
        meta.len()
    );
    if std::fs::read_to_string(&stamp).ok().as_deref() == Some(id.as_str()) {
        return Ok(());
    }
    let mut c = Command::new("cargo");
    c.env("RUSTUP_TOOLCHAIN", TOOLCHAIN)
        .current_dir(root)
        .args(["clean", "-q", "-Zjson-target-spec", "-p", name, "--target"])
        .arg(spec);
    if release {
        c.arg("--release");
    }
    let _ = c.status();
    std::fs::write(&stamp, id).map_err(|e| e.to_string())
}

/// The newest `<name>.rat` the backend wrote below `dir`.
fn find_asm(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().and_then(|f| f.to_str()) == Some(&format!("{name}.rat")) {
                let t = e.metadata().ok()?.modified().ok()?;
                if best.as_ref().is_none_or(|(bt, _)| t > *bt) {
                    best = Some((t, p));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}
