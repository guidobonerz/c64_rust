//! End-to-end tests: every program in `tests/e2e/*` is built with
//! `cargo c64` (debug and release) and run in mimic's headless C64. A
//! program checks itself and turns the border green (all passed) or red
//! (a check failed or it panicked); the screen shows the number of the
//! first failing check at the top left.
//!
//! A test that needs an emulated device (SD2IEC, the Ultimate's command
//! interface, a WiC64) lists the options for mimic in a file `mimic.args`.
//!
//! Tools: `RUST64_MIMIC_HEADLESS` (default: ../mimic/target/release next to
//! this repository) and the C64 ROMs (`EMU_ROM_DIR` or ~/.mimic/roms).

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn mimic() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("RUST64_MIMIC_HEADLESS") {
        return Some(p.into());
    }
    let exe = if cfg!(windows) { "mimic-headless.exe" } else { "mimic-headless" };
    let p = repo().join("../mimic/target/release").join(exe);
    p.is_file().then_some(p)
}

/// The colour of the top-left pixel (in the border) of a PNG snapshot.
fn border_rgb(png_path: &Path) -> (u8, u8, u8) {
    let decoder = png::Decoder::new(std::fs::File::open(png_path).unwrap());
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).unwrap();
    let px = info.color_type.samples();
    (buf[0], buf[1], buf[px.min(2)])
}

fn run_program(dir: &Path, release: bool) -> Result<(), String> {
    let mut build = Command::new(env!("CARGO_BIN_EXE_cargo-c64"));
    build.arg("build").current_dir(dir);
    if release {
        build.arg("--release");
    }
    let out = build.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("build failed:\n{}", String::from_utf8_lossy(&out.stderr)));
    }
    let name = dir.file_name().unwrap().to_str().unwrap();
    let profile = if release { "release" } else { "debug" };
    let prg = dir.join(format!("target/mos-c64/{profile}/{name}-test.prg"));
    let snap = std::env::temp_dir().join(format!("rust64-e2e-{name}-{profile}.png"));
    let _ = std::fs::remove_file(&snap);
    let mut run = Command::new(mimic().unwrap());
    run.args(["--machine", "c64", "--autostart", "--frames", "600", "--prg"]).arg(&prg);
    run.arg("--snapshot").arg(&snap);
    // Devices the test needs: more options for mimic, one per line in
    // `mimic.args` (`{dir}` is the directory of the test).
    if let Ok(extra) = std::fs::read_to_string(dir.join("mimic.args")) {
        let here = dir.to_str().unwrap().trim_start_matches(r"\\?\");
        for line in extra.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            run.arg(line.replace("{dir}", here));
        }
    }
    let status = run.status().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("mimic-headless failed".into());
    }
    let (r, g, b) = border_rgb(&snap);
    if g > r && g > b {
        Ok(())
    } else {
        Err(format!("border is not green (rgb {r},{g},{b}); see {}", snap.display()))
    }
}

#[test]
fn e2e() {
    if mimic().is_none() {
        eprintln!("skipped: mimic-headless not found (set RUST64_MIMIC_HEADLESS)");
        return;
    }
    // The backend must be current; cargo test does not build the dylib.
    let st = Command::new("cargo")
        .args(["build", "-q", "-p", "rustc_codegen_rust64"])
        .current_dir(repo())
        .status()
        .unwrap();
    assert!(st.success(), "building the backend failed");

    let mut failures = Vec::new();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(repo().join("tests/e2e"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("Cargo.toml").is_file())
        .collect();
    dirs.sort();
    for dir in &dirs {
        for release in [false, true] {
            let what = format!(
                "{} ({})",
                dir.file_name().unwrap().to_string_lossy(),
                if release { "release" } else { "debug" }
            );
            match run_program(dir, release) {
                Ok(()) => eprintln!("ok   {what}"),
                Err(e) => failures.push(format!("FAIL {what}: {e}")),
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
