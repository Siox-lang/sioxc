//! Ahead-of-time object emission (stage B5).
//!
//! Emits the design module as a native object file via `TargetMachine`. The
//! object exports the `sx_*` C ABI, so a runtime `main` (generated from the
//! testbench, or hand-written) links against it to form a
//! standalone native simulator. Compiling the testbench stimulus into that
//! `main` is the follow-on increment.

use std::path::Path;

use inkwell::context::Context;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::OptimizationLevel;

use siox::ir::Design;

use super::emit::build_module;

/// The `(cpu, features)` the target machine is built for. With the `simd`
/// feature it is the host's own CPU and native feature set — so the backend may
/// use the widest vector registers the machine has (AVX / AVX-512 → 256 / 512-
/// bit). Without it, a portable baseline (`generic` x86-64, SSE2 128-bit), so
/// objects run anywhere.
fn target_cpu_features() -> (String, String) {
    if cfg!(feature = "simd") {
        (
            TargetMachine::get_host_cpu_name()
                .to_str()
                .unwrap_or("generic")
                .to_string(),
            TargetMachine::get_host_cpu_features()
                .to_str()
                .unwrap_or("")
                .to_string(),
        )
    } else {
        ("generic".to_string(), String::new())
    }
}

/// A `TargetMachine` for codegen, tuned per the `simd` feature (see
/// [`target_cpu_features`]).
pub fn host_target_machine() -> Result<TargetMachine, String> {
    Target::initialize_native(&InitializationConfig::default())
        .map_err(|e| format!("target init failed: {e}"))?;
    let triple = TargetMachine::get_default_triple();
    let target = Target::from_triple(&triple).map_err(|e| e.to_string())?;
    let (cpu, features) = target_cpu_features();
    target
        .create_target_machine(
            &triple,
            &cpu,
            &features,
            OptimizationLevel::Default,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or_else(|| "failed to create target machine".to_string())
}

/// Emit `design` as a native object file at `path` (`.o`). The object exports
/// `sx_reset`/`sx_set`/`sx_read`/`sx_settle`.
pub fn emit_object(design: &Design, path: &Path) -> Result<(), String> {
    let tm = host_target_machine()?;
    let ctx = Context::create();
    let module = build_module(&ctx, design)?;
    super::emit::optimize_module(&module, &tm)?;
    tm.write_to_file(&module, FileType::Object, path)
        .map_err(|e| format!("object emission failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use siox::ir::{BinOp, Driver, Expr, Signal, SignalId};
    use std::process::Command;

    fn sig(path: &str, width: u32) -> Signal {
        Signal {
            path: path.into(),
            declaration_span: siox::diag::Span::new(siox::diag::FileId(0), 0..0),
            width,
            real: false,
            integer: false,
            char: false,
            range: None,
            init: vec![0],
            enum_type: None,
        }
    }

    /// Emit an adder to a native object, link a C `main` that drives it, and
    /// run — proving AOT object emission + linking + native execution.
    #[test]
    fn object_links_and_runs() {
        // clang is required to link/run; skip cleanly if it is absent.
        if Command::new("clang").arg("--version").output().is_err() {
            eprintln!("skipping object_links_and_runs: clang not found");
            return;
        }

        let design = Design {
            signals: vec![sig("E.a", 8), sig("E.b", 8), sig("E.y", 8)],
            drivers: vec![Driver {
                span: None,
                ctx: 0,
                target: SignalId(2),
                cond: None,
                expr: Expr::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(Expr::Current(SignalId(0))),
                    rhs: Box::new(Expr::Current(SignalId(1))),
                },
                meta: None,
            }],
            event_blocks: vec![],
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            vector_element_enums: Default::default(),
            vector_element_of_family: Default::default(),
            source_layouts: Default::default(),
        };

        let dir = std::env::temp_dir().join(format!("siox_aot_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let obj = dir.join("design.o");
        let main_c = dir.join("main.c");
        let bin = dir.join("sim");

        emit_object(&design, &obj).unwrap();
        assert!(
            obj.exists() && std::fs::metadata(&obj).unwrap().len() > 0,
            "empty object"
        );

        std::fs::write(
            &main_c,
            r#"
extern void sx_reset(void);
extern void sx_set(unsigned, unsigned long long);
extern unsigned long long sx_read(unsigned);
extern void sx_settle(void);
signed main(void) {
    sx_reset();
    sx_set(0, 30); sx_set(1, 12); sx_settle();
    if (sx_read(2) != 42) return 1;
    sx_set(0, 200); sx_set(1, 100); sx_settle();   /* wraps at 8 bits */
    if (sx_read(2) != (300 % 256)) return 2;
    return 0;
}
"#,
        )
        .unwrap();

        let link = Command::new("clang")
            .args([
                main_c.to_str().unwrap(),
                obj.to_str().unwrap(),
                "-o",
                bin.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            link.status.success(),
            "link failed: {}",
            String::from_utf8_lossy(&link.stderr)
        );

        let run = Command::new(&bin).status().unwrap();
        assert!(run.success(), "native sim returned {:?}", run.code());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(not(feature = "bitpack"))]
    fn eight_word_object_links_and_carries() {
        if Command::new("clang").arg("--version").output().is_err() {
            eprintln!("skipping eight_word_object_links_and_carries: clang not found");
            return;
        }

        let design = Design {
            signals: vec![sig("E.a", 512), sig("E.b", 512), sig("E.y", 512)],
            drivers: vec![Driver {
                span: None,
                ctx: 0,
                target: SignalId(2),
                cond: None,
                expr: Expr::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(Expr::Current(SignalId(0))),
                    rhs: Box::new(Expr::Current(SignalId(1))),
                },
                meta: None,
            }],
            event_blocks: vec![],
            enum_syms: Default::default(),
            enum_bases: Default::default(),
            new_defaults: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
            vector_element_enums: Default::default(),
            vector_element_of_family: Default::default(),
            source_layouts: Default::default(),
        };

        let dir = std::env::temp_dir().join(format!("siox_aot_wide_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let obj = dir.join("design.o");
        let main_c = dir.join("main.c");
        let bin = dir.join("sim");
        emit_object(&design, &obj).unwrap();
        std::fs::write(
            &main_c,
            r#"
extern void sx_reset(void);
extern void sx_set_word(unsigned, unsigned, unsigned long long);
extern unsigned long long sx_read_word(unsigned, unsigned);
extern void sx_settle(void);
signed main(void) {
    sx_reset();
    sx_set_word(0, 6, ~0ULL);
    sx_set_word(0, 7, 0x0123456789abcdefULL);
    sx_set_word(1, 6, 1);
    sx_settle();
    if (sx_read_word(2, 6) != 0) return 1;
    if (sx_read_word(2, 7) != 0x0123456789abcdf0ULL) return 2;
    return 0;
}
"#,
        )
        .unwrap();
        let link = Command::new("clang")
            .args([
                main_c.to_str().unwrap(),
                obj.to_str().unwrap(),
                "-o",
                bin.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            link.status.success(),
            "link failed: {}",
            String::from_utf8_lossy(&link.stderr)
        );
        let run = Command::new(&bin).status().unwrap();
        assert!(run.success(), "native wide sim returned {:?}", run.code());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
