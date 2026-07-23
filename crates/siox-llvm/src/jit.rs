//! In-process JIT execution of the emitted module (stage B3).
//!
//! Builds the module for a `Design`, JIT-compiles it, and exposes the
//! `sx_*` C ABI as safe method calls. All LLVM objects live inside
//! [`with_jit`] so lifetimes stay sound; the caller drives the design
//! through the [`Jit`] handle within the closure.

use inkwell::context::Context;
use inkwell::execution_engine::JitFunction;
use inkwell::OptimizationLevel;

use siox::ir::Design;

use crate::emit::build_module;

type ResetFn = unsafe extern "C" fn();
type SetFn = unsafe extern "C" fn(u32, u64);
type ReadFn = unsafe extern "C" fn(u32) -> u64;
type SettleFn = unsafe extern "C" fn();

/// A JIT-compiled design. Drive it like the interpreter: `set` inputs,
/// `settle`, `read` outputs.
pub struct Jit<'ctx> {
    reset: JitFunction<'ctx, ResetFn>,
    set: JitFunction<'ctx, SetFn>,
    read: JitFunction<'ctx, ReadFn>,
    settle: JitFunction<'ctx, SettleFn>,
}

impl Jit<'_> {
    pub fn reset(&self) {
        unsafe { self.reset.call() }
    }
    pub fn set(&self, sig: u32, value: u64) {
        unsafe { self.set.call(sig, value) }
    }
    pub fn read(&self, sig: u32) -> u64 {
        unsafe { self.read.call(sig) }
    }
    pub fn settle(&self) {
        unsafe { self.settle.call() }
    }
}

/// JIT-compile `design` and run `f` against it. Everything LLVM lives for the
/// duration of the call.
pub fn with_jit<R>(design: &Design, f: impl FnOnce(&Jit) -> R) -> R {
    let ctx = Context::create();
    let module = build_module(&ctx, design);
    // Optimize the IR (constant-fold, kill bitcast churn, GVN loads) before the
    // engine codegens it, and let the engine's own codegen run aggressively.
    if let Ok(tm) = crate::aot::host_target_machine() {
        let _ = crate::emit::optimize_module(&module, &tm);
    }
    let ee = module
        .create_jit_execution_engine(OptimizationLevel::Aggressive)
        .expect("failed to create JIT engine");
    // SAFETY: the emitted signatures match the ABI types above; the engine
    // outlives the closure.
    let jit = unsafe {
        Jit {
            reset: ee.get_function("sx_reset").unwrap(),
            set: ee.get_function("sx_set").unwrap(),
            read: ee.get_function("sx_read").unwrap(),
            settle: ee.get_function("sx_settle").unwrap(),
        }
    };
    jit.reset();
    f(&jit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use siox::ir::{BinOp, Driver, Expr, Signal, SignalId};

    fn sig(path: &str, width: u32) -> Signal {
        Signal { path: path.into(), width, real: false, char: false, range: None, init: 0, enum_type: None }
    }

    #[test]
    fn jit_runs_a_combinational_adder() {
        // y (2) = a (0) + b (1), width 8 (wraps).
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
            new_defaults: Default::default(),
            base_dir: Default::default(),
            meta_of: Default::default(),
        };
        with_jit(&design, |jit| {
            jit.set(0, 30);
            jit.set(1, 12);
            jit.settle();
            assert_eq!(jit.read(2), 42, "30 + 12");
            jit.set(0, 200);
            jit.set(1, 100);
            jit.settle();
            assert_eq!(jit.read(2), (300 % 256), "wraps at width 8");
        });
    }
}
