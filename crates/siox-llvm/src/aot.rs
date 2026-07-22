//! Ahead-of-time object emission (stage B5).
//!
//! Emits the design module as a native object file via `TargetMachine`. The
//! object exports the same `sx_*` C ABI the JIT uses, so a runtime `main`
//! (generated from the testbench, or hand-written) links against it to form a
//! standalone native simulator. Compiling the testbench stimulus into that
//! `main` is the follow-on increment.

use std::path::Path;

use inkwell::context::Context;
use inkwell::targets::{CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine};
use inkwell::OptimizationLevel;

use siox::ir::Design;

use crate::emit::build_module;

/// Emit `design` as a native object file at `path` (`.o`). The object exports
/// `sx_reset`/`sx_set`/`sx_read`/`sx_settle`.
pub fn emit_object(design: &Design, path: &Path) -> Result<(), String> {
    Target::initialize_native(&InitializationConfig::default())
        .map_err(|e| format!("target init failed: {e}"))?;

    let triple = TargetMachine::get_default_triple();
    let target = Target::from_triple(&triple).map_err(|e| e.to_string())?;
    let tm = target
        .create_target_machine(
            &triple,
            TargetMachine::get_host_cpu_name().to_str().unwrap_or("generic"),
            TargetMachine::get_host_cpu_features().to_str().unwrap_or(""),
            OptimizationLevel::Default,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or("failed to create target machine")?;

    let ctx = Context::create();
    let module = build_module(&ctx, design);
    tm.write_to_file(&module, FileType::Object, path)
        .map_err(|e| format!("object emission failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use siox::ir::{BinOp, Driver, Expr, Signal, SignalId};
    use std::process::Command;

    fn sig(path: &str, width: u32) -> Signal {
        Signal { path: path.into(), width, real: false, char: false, range: None, init: 0, enum_type: None }
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
                ctx: 0,
                target: SignalId(2),
                cond: None,
                expr: Expr::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(Expr::Current(SignalId(0))),
                    rhs: Box::new(Expr::Current(SignalId(1))),
                },
            }],
            event_blocks: vec![],
            enum_syms: Default::default(),
            base_dir: Default::default(),
        };

        let dir = std::env::temp_dir().join(format!("siox_aot_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let obj = dir.join("design.o");
        let main_c = dir.join("main.c");
        let bin = dir.join("sim");

        emit_object(&design, &obj).unwrap();
        assert!(obj.exists() && std::fs::metadata(&obj).unwrap().len() > 0, "empty object");

        std::fs::write(
            &main_c,
            r#"
extern void sx_reset(void);
extern void sx_set(unsigned, unsigned long long);
extern unsigned long long sx_read(unsigned);
extern void sx_settle(void);
int main(void) {
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
            .args([main_c.to_str().unwrap(), obj.to_str().unwrap(), "-o", bin.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(link.status.success(), "link failed: {}", String::from_utf8_lossy(&link.stderr));

        let run = Command::new(&bin).status().unwrap();
        assert!(run.success(), "native sim returned {:?}", run.code());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
