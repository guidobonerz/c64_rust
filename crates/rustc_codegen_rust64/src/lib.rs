//! rustc codegen backend for the Commodore 64.
//!
//! Library crates (`core`, `c64`, ...) produce no code: their MIR is kept in
//! the crate metadata (`-Zalways-encode-mir`). When rustc compiles the final
//! executable, [`codegen::build_program`] walks the program from `main`
//! through the MIR of every crate, translates each reachable function to
//! 6510 assembly in the rat dialect and assembles it with rat into a `.prg`.

#![feature(rustc_private)]

extern crate rustc_abi;
extern crate rustc_ast;
extern crate rustc_codegen_ssa;
extern crate rustc_data_structures;
extern crate rustc_hir;
extern crate rustc_index;
extern crate rustc_metadata;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;
extern crate rustc_structures;
extern crate rustc_target;

// Makes rustc_driver's dylib the single provider of the rustc crates.
#[allow(unused_extern_crates)]
extern crate rustc_driver;

mod asm;
mod codegen;
mod runtime;

use std::any::Any;
use std::sync::Mutex;

use rustc_codegen_ssa::traits::CodegenBackend;
use rustc_codegen_ssa::{CompiledModules, CrateInfo, TargetConfig};
use rustc_middle::dep_graph::WorkProductMap;
use rustc_middle::ty::TyCtxt;
use rustc_session::config::{OutputFilenames, OutputType};
use rustc_structures::CrateType;
use rustc_session::{EarlySession, IncrCompSession, Session};

/// The result of compiling an executable: the assembly and the program.
struct Built {
    asm: String,
    prg: Vec<u8>,
    d64: Vec<u8>,
}

pub struct Rust64Backend {
    built: Mutex<Option<Built>>,
}

impl CodegenBackend for Rust64Backend {
    fn name(&self) -> &'static str {
        "rust64"
    }

    fn target_config(&self, _sess: &EarlySession) -> TargetConfig {
        TargetConfig {
            internal_target_features: Default::default(),
            has_reliable_f16: false,
            has_reliable_f16_math: false,
            has_reliable_f16b: false,
            has_reliable_f128: false,
            has_reliable_f128_math: false,
        }
    }

    fn supported_crate_types(&self, _sess: &Session) -> Vec<CrateType> {
        vec![CrateType::Executable, CrateType::Rlib, CrateType::ProcMacro]
    }

    fn target_cpu(&self, _sess: &Session) -> String {
        "6510".to_owned()
    }

    fn codegen_crate<'tcx>(&self, tcx: TyCtxt<'tcx>) -> Box<dyn Any> {
        if tcx.crate_types().contains(&CrateType::Executable) {
            let asm = codegen::build_program(tcx);
            let name = tcx.crate_name(rustc_hir::def_id::LOCAL_CRATE).as_str().replace('_', "-");
            let res = runtime::assemble(&asm, "prg", &name)
                .and_then(|prg| Ok((prg, runtime::assemble(&asm, "d64", &name)?)));
            match res {
                Ok((prg, d64)) => *self.built.lock().unwrap() = Some(Built { asm, prg, d64 }),
                Err(msg) => {
                    // Keep the assembly for inspection.
                    let out = tcx.output_filenames(());
                    let path = out.with_extension("rat");
                    let _ = std::fs::write(&path, &asm);
                    tcx.dcx().fatal(format!(
                        "rat failed on the generated assembly ({}):\n{msg}",
                        path.display()
                    ));
                }
            }
        }
        Box::new(())
    }

    fn join_codegen(
        &self,
        _ongoing_codegen: Box<dyn Any>,
        _sess: &Session,
        _incr_comp_session: Option<&IncrCompSession>,
        _outputs: &OutputFilenames,
        _crate_info: &CrateInfo,
    ) -> (CompiledModules, WorkProductMap) {
        (CompiledModules { modules: vec![], allocator_module: None }, WorkProductMap::default())
    }

    fn link(
        &self,
        sess: &Session,
        compiled_modules: CompiledModules,
        crate_info: CrateInfo,
        metadata: rustc_metadata::EncodedMetadata,
        outputs: &OutputFilenames,
    ) {
        let built = self.built.lock().unwrap().take();
        if let Some(built) = built {
            let crate_name = crate_info.local_crate_name;
            let out = rustc_session::output::out_filename(
                sess,
                CrateType::Executable,
                outputs,
                crate_name,
            );
            let path = out.as_path().to_path_buf();
            if let Err(e) = std::fs::write(&path, &built.prg) {
                sess.dcx().fatal(format!("cannot write {}: {e}", path.display()));
            }
            let _ = std::fs::write(path.with_extension("rat"), &built.asm);
            if let Err(e) = std::fs::write(path.with_extension("d64"), &built.d64) {
                sess.dcx().fatal(format!("cannot write the disk image: {e}"));
            }
            if outputs.outputs.contains_key(&OutputType::Assembly) {
                let asm_path = outputs.path(OutputType::Assembly);
                let _ = std::fs::write(asm_path.as_path(), &built.asm);
            }
            return;
        }
        // Libraries: an rlib with the metadata (and no object code).
        rustc_codegen_ssa::back::link::link_binary(
            sess,
            &rustc_codegen_ssa::back::archive::ArArchiveBuilderBuilder,
            compiled_modules,
            crate_info,
            metadata,
            outputs,
            self.name(),
        );
    }
}

/// Entry point for `-Zcodegen-backend=<this dylib>`.
#[unsafe(no_mangle)]
pub fn __rustc_codegen_backend() -> Box<dyn CodegenBackend> {
    Box::new(Rust64Backend { built: Mutex::new(None) })
}
