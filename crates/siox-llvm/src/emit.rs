//! The inkwell emitter (behind the `llvm` feature).

use std::collections::HashMap;

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::values::{IntValue, PointerValue};
use inkwell::IntPredicate;

use siox::ir::{BinOp, Design, Expr, ProcessKind, SignalId, UnOp};

/// Build the LLVM module for `design` and return its textual IR (`.ll`).
/// This is what `siox build --emit-llvm` prints and what golden tests diff.
pub fn emit_module_ir(design: &Design) -> String {
    let ctx = Context::create();
    let module = build_module(&ctx, design);
    module.print_to_string().to_string()
}

/// Build and verify the LLVM module for `design` in `ctx`. Shared by the
/// textual emitter and the JIT.
pub(crate) fn build_module<'ctx>(ctx: &'ctx Context, design: &Design) -> Module<'ctx> {
    // Reject IR a backend can't compile (bad ids, Unknown, unknown widths)
    // with a clear message rather than emitting malformed LLVM (B0).
    let issues = design.validate();
    if !issues.is_empty() {
        panic!("cannot codegen invalid IR:\n  - {}", issues.join("\n  - "));
    }
    // This backend is i64-word-based; signals wider than 64 bits would silently
    // truncate, so reject them rather than miscompile. Wide-word codegen lands
    // with the type-narrowing work.
    if let Some(s) = design.signals.iter().find(|s| s.width > 64) {
        panic!(
            "signal `{}` is {} bits wide; the LLVM backend is 64-bit-word only \
             (wide-word codegen is not yet implemented)",
            s.path, s.width
        );
    }
    let cg = Codegen::new(ctx, design);
    cg.build();
    // LLVM's own verifier — a well-formedness net beyond textual checks.
    if let Err(e) = cg.module.verify() {
        panic!("emitted invalid LLVM module:\n{}\n--- IR ---\n{}", e, cg.module.print_to_string());
    }
    cg.module
}

struct Codegen<'ctx, 'd> {
    ctx: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    design: &'d Design,
    n: u32,
}

impl<'ctx, 'd> Codegen<'ctx, 'd> {
    fn new(ctx: &'ctx Context, design: &'d Design) -> Self {
        let module = ctx.create_module("design");
        Codegen { ctx, module, builder: ctx.create_builder(), design, n: design.signals.len() as u32 }
    }

    fn i64t(&self) -> inkwell::types::IntType<'ctx> {
        self.ctx.i64_type()
    }

    fn build(&self) {
        self.state_globals();
        self.accessors();
        self.settle();
    }

    // --- state ------------------------------------------------------------

    fn state_globals(&self) {
        let arr = self.i64t().array_type(self.n);
        // `snap` holds each delta's entry values, so `old` can advance to them
        // and internally-generated edges fire in the next delta (cascaded
        // event domains / derived clocks).
        for name in ["cur", "old", "event", "snap"] {
            let g = self.module.add_global(arr, None, name);
            g.set_initializer(&arr.const_zero());
            g.set_linkage(Linkage::Internal);
        }
    }

    fn array_ptr(&self, name: &str) -> PointerValue<'ctx> {
        self.module.get_global(name).unwrap().as_pointer_value()
    }

    /// Pointer to `@<arr>[id]`.
    fn slot_ptr(&self, arr: &str, id: SignalId) -> PointerValue<'ctx> {
        let zero = self.i64t().const_zero();
        let idx = self.i64t().const_int(id.0 as u64, false);
        unsafe {
            self.builder
                .build_in_bounds_gep(
                    self.i64t().array_type(self.n),
                    self.array_ptr(arr),
                    &[zero, idx],
                    "slot",
                )
                .unwrap()
        }
    }

    fn load(&self, arr: &str, id: SignalId) -> IntValue<'ctx> {
        self.builder.build_load(self.i64t(), self.slot_ptr(arr, id), "v").unwrap().into_int_value()
    }

    fn store(&self, arr: &str, id: SignalId, v: IntValue<'ctx>) {
        self.builder.build_store(self.slot_ptr(arr, id), v).unwrap();
    }

    // --- accessors: sx_set / sx_read / sx_reset ---------------------------

    fn accessors(&self) {
        let i64 = self.i64t();
        let i32 = self.ctx.i32_type();
        let void = self.ctx.void_type();

        // void sx_reset(void): signals take their declared initial values
        // (VHDL-style); events clear.
        let f = self.module.add_function("sx_reset", void.fn_type(&[], false), None);
        self.builder.position_at_end(self.ctx.append_basic_block(f, "e"));
        for id in 0..self.n {
            let init = i64.const_int(self.design.signals[id as usize].init, false);
            self.store("cur", SignalId(id), init);
            self.store("old", SignalId(id), init);
            self.store("event", SignalId(id), i64.const_zero());
        }
        self.builder.build_return(None).unwrap();

        // void sx_set(i32 sig, i64 val): cur[sig] = val  (bounded switch).
        let f = self.module.add_function("sx_set", void.fn_type(&[i32.into(), i64.into()], false), None);
        let entry = self.ctx.append_basic_block(f, "e");
        self.builder.position_at_end(entry);
        let sig = f.get_nth_param(0).unwrap().into_int_value();
        let val = f.get_nth_param(1).unwrap().into_int_value();
        let done = self.ctx.append_basic_block(f, "done");
        let cases: Vec<_> = (0..self.n)
            .map(|id| {
                let bb = self.ctx.append_basic_block(f, "s");
                (i32.const_int(id as u64, false), bb)
            })
            .collect();
        self.builder.position_at_end(entry);
        self.builder.build_switch(sig, done, &cases).unwrap();
        for (id, (_, bb)) in cases.iter().enumerate() {
            self.builder.position_at_end(*bb);
            // Mask to the signal's width, exactly like the interpreter's
            // `set` — outside writers (runner, native harness, FFI) may hand
            // in a value wider than the signal.
            let w = self.design.signals[id].width;
            let stored = if w > 0 && w < 64 {
                let m = i64.const_int((1u64 << w) - 1, false);
                self.builder.build_and(val, m, "m").unwrap()
            } else {
                val
            };
            self.store("cur", SignalId(id as u32), stored);
            self.builder.build_unconditional_branch(done).unwrap();
        }
        self.builder.position_at_end(done);
        self.builder.build_return(None).unwrap();

        // i64 sx_read(i32 sig).
        let f = self.module.add_function("sx_read", i64.fn_type(&[i32.into()], false), None);
        let entry = self.ctx.append_basic_block(f, "e");
        self.builder.position_at_end(entry);
        let sig = f.get_nth_param(0).unwrap().into_int_value();
        let ret = self.ctx.append_basic_block(f, "ret");
        let cases: Vec<_> = (0..self.n)
            .map(|id| (i32.const_int(id as u64, false), self.ctx.append_basic_block(f, "r")))
            .collect();
        self.builder.position_at_end(entry);
        self.builder.build_switch(sig, ret, &cases).unwrap();
        // Each case loads and jumps to ret; a phi selects the value.
        let mut incoming: Vec<(IntValue<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> = Vec::new();
        for (id, (_, bb)) in cases.iter().enumerate() {
            self.builder.position_at_end(*bb);
            let v = self.load("cur", SignalId(id as u32));
            incoming.push((v, *bb));
            self.builder.build_unconditional_branch(ret).unwrap();
        }
        self.builder.position_at_end(ret);
        let phi = self.builder.build_phi(i64, "v").unwrap();
        let zero = i64.const_zero();
        // default (unmatched sig) yields 0.
        phi.add_incoming(&[(&zero, entry)]);
        for (v, bb) in &incoming {
            phi.add_incoming(&[(v as &dyn inkwell::values::BasicValue, *bb)]);
        }
        self.builder.build_return(Some(&phi.as_basic_value().into_int_value())).unwrap();
    }

    // --- sx_settle: combinational processes in dependency order -----------

    /// Emit `sx_settle` as a bounded **delta-cycle loop** so internally-generated
    /// edges propagate (derived clocks, clock dividers, ripple counters).
    ///
    /// Each delta: (1) `event[i] = cur[i] != old[i]` — changes since the last
    /// delta — and `snap[i] = cur[i]`; if nothing changed, we're stable and
    /// return. (2) combinational settle; (3+4) event blocks compute next-state
    /// from the *pre-commit* state (so simultaneous updates don't see each
    /// other) and commit; (5) re-settle combinational; (6) advance `old <- snap`
    /// so this delta's changes appear as edges in the *next* delta — and only
    /// then, so each edge fires exactly once. A delta cap bounds the loop
    /// against a zero-delay oscillation.
    fn settle(&self) {
        let void = self.ctx.void_type();
        let i64 = self.i64t();
        let i1 = self.ctx.bool_type();
        let f = self.module.add_function("sx_settle", void.fn_type(&[], false), None);
        let entry = self.ctx.append_basic_block(f, "entry");
        let body = self.ctx.append_basic_block(f, "body");
        let done = self.ctx.append_basic_block(f, "done");

        // entry: a delta counter for the oscillation cap; run the body at least
        // once (so combinational logic always settles even with no events).
        self.builder.position_at_end(entry);
        let dcount = self.builder.build_alloca(i64, "dcount").unwrap();
        self.builder.build_store(dcount, i64.const_zero()).unwrap();
        self.builder.build_unconditional_branch(body).unwrap();

        // body — one delta cycle, looping while it keeps producing changes.
        self.builder.position_at_end(body);
        // 1. combinational settle first, so a comb-driven clock (a port
        // connection, `C.clk <- T.clk`) has its new value in `cur` *before* we
        // detect its edge below.
        self.emit_comb_pass();
        // 2. event[i] = (cur != old): changes since the previous delta. `snap`
        // captures this delta's (post-comb) values for the `old` advance below.
        let mut any = i1.const_zero();
        for i in 0..self.n {
            let id = SignalId(i);
            let cur = self.load("cur", id);
            let ne = self.builder
                .build_int_compare(IntPredicate::NE, cur, self.load("old", id), "ev")
                .unwrap();
            self.store("event", id, self.zext(ne));
            self.store("snap", id, cur);
            any = self.builder.build_or(any, ne, "any").unwrap();
        }
        // 3+4. event blocks: stage guards/values from the pre-commit state (so
        // simultaneous updates don't see each other), then commit.
        let mut staged: Vec<(SignalId, IntValue<'ctx>, IntValue<'ctx>)> = Vec::new();
        for eb in &self.design.event_blocks {
            let fired = self.as_i1(&eb.condition);
            for u in &eb.updates {
                let guard = match &u.cond {
                    Some(c) => self.builder.build_and(fired, self.as_i1(c), "g").unwrap(),
                    None => fired,
                };
                let w = self.design.signals[u.target.0 as usize].width;
                let val = self.mask(self.emit(&u.expr), w);
                staged.push((u.target, guard, val));
            }
        }
        let committed = !staged.is_empty();
        for (target, guard, val) in staged {
            let prev = self.load("cur", target);
            let next = self.builder.build_select(guard, val, prev, "next").unwrap().into_int_value();
            self.store("cur", target, next);
            self.mark_event(target, prev, next);
        }
        // 5. re-settle combinational after commits.
        if committed {
            self.emit_comb_pass();
        }
        // 6. advance old <- snap, so changes made *in* this delta appear as
        // edges in the next one — and only then, so each edge fires once.
        for i in 0..self.n {
            let id = SignalId(i);
            self.store("old", id, self.load("snap", id));
        }
        // Loop while this delta had events (there may be more to propagate) and
        // the delta cap — comfortably past any real cascade depth — is not hit.
        let cap = i64.const_int(self.n as u64 + 64, false);
        let dc = self.builder.build_load(i64, dcount, "dc").unwrap().into_int_value();
        let inc = self.builder.build_int_add(dc, i64.const_int(1, false), "inc").unwrap();
        self.builder.build_store(dcount, inc).unwrap();
        let under = self.builder.build_int_compare(IntPredicate::ULT, inc, cap, "under").unwrap();
        let cont = self.builder.build_and(any, under, "cont").unwrap();
        self.builder.build_conditional_branch(cont, body, done).unwrap();

        // done: clear event flags and return.
        self.builder.position_at_end(done);
        for i in 0..self.n {
            self.store("event", SignalId(i), self.c(0));
        }
        self.builder.build_return(None).unwrap();
    }

    /// One combinational settle pass over the processes in dependency order.
    fn emit_comb_pass(&self) {
        let comb = self.comb();
        for pi in self.topo_order() {
            self.emit_comb(&comb[pi]);
        }
    }

    /// `event[target] |= (next != prev)` — a change flags the signal.
    fn mark_event(&self, target: SignalId, prev: IntValue<'ctx>, next: IntValue<'ctx>) {
        let ch = self.builder.build_int_compare(IntPredicate::NE, next, prev, "ch").unwrap();
        let ev = self.builder.build_or(self.load("event", target), self.zext(ch), "ev2").unwrap();
        self.store("event", target, ev);
    }

    /// Combinational processes (target + source-ordered driver indices).
    fn comb(&self) -> Vec<(SignalId, Vec<usize>)> {
        self.design
            .processes()
            .into_iter()
            .filter_map(|p| match p.kind {
                ProcessKind::Comb { target, drivers } => Some((target, drivers)),
                ProcessKind::Event { .. } => None,
            })
            .collect()
    }

    /// Topologically order combinational processes so each runs after the
    /// processes producing the signals it reads (single-pass settle for
    /// acyclic logic). A cyclic remainder is appended in index order.
    fn topo_order(&self) -> Vec<usize> {
        let procs = self.design.processes();
        let comb: Vec<_> = procs
            .iter()
            .enumerate()
            .filter(|(_, p)| matches!(p.kind, ProcessKind::Comb { .. }))
            .collect();
        // map: signal -> the comb process (local index) that writes it.
        let mut writer: HashMap<SignalId, usize> = HashMap::new();
        let mut local: Vec<usize> = Vec::new(); // local index -> comb() index
        for (li, (_, p)) in comb.iter().enumerate() {
            if let ProcessKind::Comb { target, .. } = &p.kind {
                writer.insert(*target, li);
            }
            local.push(li);
        }
        let m = comb.len();
        let mut deps: Vec<Vec<usize>> = vec![Vec::new(); m];
        let mut indeg = vec![0usize; m];
        for (li, (_, p)) in comb.iter().enumerate() {
            for r in &p.reads {
                if let Some(&w) = writer.get(r) {
                    if w != li {
                        deps[w].push(li);
                        indeg[li] += 1;
                    }
                }
            }
        }
        let mut queue: Vec<usize> = (0..m).filter(|&i| indeg[i] == 0).collect();
        let mut order = Vec::new();
        let mut seen = vec![false; m];
        while let Some(x) = queue.pop() {
            if seen[x] {
                continue;
            }
            seen[x] = true;
            order.push(x);
            for &y in &deps[x] {
                indeg[y] -= 1;
                if indeg[y] == 0 {
                    queue.push(y);
                }
            }
        }
        // Any cyclic remainder in index order.
        for i in 0..m {
            if !seen[i] {
                order.push(i);
            }
        }
        order
    }

    /// Resolve a combinational target: fold its drivers in source order
    /// (`value = cond ? expr : value`), mask, store to `cur`.
    fn emit_comb(&self, p: &(SignalId, Vec<usize>)) {
        let (target, drivers) = p;
        let prev = self.load("cur", *target);
        let mut val = prev;
        for &di in drivers {
            let d = &self.design.drivers[di];
            let e = self.emit(&d.expr);
            val = match &d.cond {
                Some(c) => {
                    let cond = self.as_i1(c);
                    self.builder.build_select(cond, e, val, "drv").unwrap().into_int_value()
                }
                None => e,
            };
        }
        let w = self.design.signals[target.0 as usize].width;
        let masked = self.mask(val, w);
        self.store("cur", *target, masked);
        self.mark_event(*target, prev, masked);
    }

    // --- expressions ------------------------------------------------------

    fn c(&self, v: u64) -> IntValue<'ctx> {
        self.i64t().const_int(v, false)
    }

    /// Truncate to a signal's width by AND-masking (0 / >=64 => unchanged).
    fn mask(&self, v: IntValue<'ctx>, width: u32) -> IntValue<'ctx> {
        if width == 0 || width >= 64 {
            return v;
        }
        let m = (1u64 << width) - 1;
        self.builder.build_and(v, self.c(m), "mask").unwrap()
    }

    /// Evaluate a condition to an `i1` (nonzero).
    fn as_i1(&self, e: &Expr) -> IntValue<'ctx> {
        let v = self.emit(e);
        self.builder.build_int_compare(IntPredicate::NE, v, self.c(0), "nz").unwrap()
    }

    /// zext an `i1` back to the i64 word domain.
    fn zext(&self, b: IntValue<'ctx>) -> IntValue<'ctx> {
        self.builder.build_int_z_extend(b, self.i64t(), "z").unwrap()
    }

    fn emit(&self, e: &Expr) -> IntValue<'ctx> {
        match e {
            Expr::Const(v) => self.c(*v),
            Expr::Real(x) => self.c(x.to_bits()),
            // IR lowering resolves every logic literal to a `Const` (its
            // position in std's logic type), so none reach the backend.
            Expr::Logic(ch) => unreachable!("unresolved logic literal '{ch}' reached the backend"),
            Expr::Current(id) => self.load("cur", *id),
            Expr::Old(id) => self.load("old", *id),
            Expr::Event(id) => self.load("event", *id),
            Expr::Unary { op, rhs } => {
                let a = self.emit(rhs);
                match op {
                    UnOp::Not => {
                        let z = self.builder.build_int_compare(IntPredicate::EQ, a, self.c(0), "not").unwrap();
                        self.zext(z)
                    }
                    UnOp::Neg => self.builder.build_int_neg(a, "neg").unwrap(),
                }
            }
            Expr::Binary { op, lhs, rhs } => self.emit_binary(*op, lhs, rhs),
            Expr::Slice { base, hi, lo } => {
                let b = self.emit(base);
                let sh = self.builder.build_right_shift(b, self.c(*lo as u64), false, "sh").unwrap();
                self.mask(sh, hi - lo + 1)
            }
            Expr::Select { cond, then, els } => {
                let c = self.as_i1(cond);
                let t = self.emit(then);
                let e = self.emit(els);
                self.builder.build_select(c, t, e, "sel").unwrap().into_int_value()
            }
            Expr::CCall { name, args, f64_args, f64_ret } => {
                // Foreign C call: `real` params are doubles (bit-cast from the
                // word), everything else i64. JIT resolves the symbol from the
                // process; native from the linked libraries.
                use inkwell::types::BasicMetadataTypeEnum as MT;
                use inkwell::values::BasicMetadataValueEnum as MV;
                let f64t = self.ctx.f64_type();
                let mut ptypes: Vec<MT> = Vec::new();
                let mut vals: Vec<MV> = Vec::new();
                for (i, a) in args.iter().enumerate() {
                    let v = self.emit(a);
                    if f64_args.get(i).copied().unwrap_or(false) {
                        ptypes.push(f64t.into());
                        vals.push(
                            self.builder
                                .build_bit_cast(v, f64t, "farg")
                                .unwrap()
                                .into_float_value()
                                .into(),
                        );
                    } else {
                        ptypes.push(self.i64t().into());
                        vals.push(v.into());
                    }
                }
                let f = self.module.get_function(name).unwrap_or_else(|| {
                    let fnty = if *f64_ret {
                        f64t.fn_type(&ptypes, false)
                    } else {
                        self.i64t().fn_type(&ptypes, false)
                    };
                    self.module.add_function(name, fnty, Some(inkwell::module::Linkage::External))
                });
                let r = match self
                    .builder
                    .build_call(f, &vals, "ccall")
                    .unwrap()
                    .try_as_basic_value()
                {
                    inkwell::values::ValueKind::Basic(v) => v,
                    _ => panic!("extern fn returns a value"),
                };
                if *f64_ret {
                    self.builder
                        .build_bit_cast(r.into_float_value(), self.i64t(), "fbits")
                        .unwrap()
                        .into_int_value()
                } else {
                    r.into_int_value()
                }
            }
            Expr::Unknown => self.c(0),
        }
    }

    fn emit_binary(&self, op: BinOp, lhs: &Expr, rhs: &Expr) -> IntValue<'ctx> {
        // Float ops reinterpret the i64 words as f64.
        if matches!(op, BinOp::FAdd | BinOp::FSub | BinOp::FMul | BinOp::FDiv) {
            let f = self.ctx.f64_type();
            let a = self.builder.build_bit_cast(self.emit(lhs), f, "fa").unwrap().into_float_value();
            let b = self.builder.build_bit_cast(self.emit(rhs), f, "fb").unwrap().into_float_value();
            let r = match op {
                BinOp::FAdd => self.builder.build_float_add(a, b, "fadd").unwrap(),
                BinOp::FSub => self.builder.build_float_sub(a, b, "fsub").unwrap(),
                BinOp::FMul => self.builder.build_float_mul(a, b, "fmul").unwrap(),
                _ => self.builder.build_float_div(a, b, "fdiv").unwrap(),
            };
            return self.builder.build_bit_cast(r, self.i64t(), "fbits").unwrap().into_int_value();
        }

        let a = self.emit(lhs);
        let b = self.emit(rhs);
        let cmp = |p: IntPredicate, s: &str| {
            let c = self.builder.build_int_compare(p, a, b, s).unwrap();
            self.zext(c)
        };
        match op {
            BinOp::Add => self.builder.build_int_add(a, b, "add").unwrap(),
            BinOp::Sub => self.builder.build_int_sub(a, b, "sub").unwrap(),
            BinOp::Mul => self.builder.build_int_mul(a, b, "mul").unwrap(),
            BinOp::Div => {
                // Match the interpreter: divide-by-zero yields 0 (B0 formalizes).
                let is0 = self.builder.build_int_compare(IntPredicate::EQ, b, self.c(0), "d0").unwrap();
                let safe = self.builder.build_select(is0, self.c(1), b, "den").unwrap().into_int_value();
                let q = self.builder.build_int_unsigned_div(a, safe, "div").unwrap();
                self.builder.build_select(is0, self.c(0), q, "divz").unwrap().into_int_value()
            }
            BinOp::Shl => self.builder.build_left_shift(a, b, "shl").unwrap(),
            BinOp::Shr => self.builder.build_right_shift(a, b, false, "shr").unwrap(),
            // Core logical operators; for boolean 0/1 operands these match
            // their scalar reading, and vectors apply them per bit.
            // operands this matches the logical reading.
            BinOp::And => self.builder.build_and(a, b, "and").unwrap(),
            BinOp::Or => self.builder.build_or(a, b, "or").unwrap(),
            BinOp::Eq => cmp(IntPredicate::EQ, "eq"),
            BinOp::Ne => cmp(IntPredicate::NE, "ne"),
            BinOp::Lt => cmp(IntPredicate::ULT, "lt"),
            BinOp::Le => cmp(IntPredicate::ULE, "le"),
            BinOp::Gt => cmp(IntPredicate::UGT, "gt"),
            BinOp::Ge => cmp(IntPredicate::UGE, "ge"),
            BinOp::FAdd | BinOp::FSub | BinOp::FMul | BinOp::FDiv => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use siox::ir::{Design, Driver, Signal};

    fn sig(path: &str, width: u32) -> Signal {
        Signal { path: path.into(), width, real: false, char: false, range: None, init: 0, enum_type: None }
    }

    #[test]
    fn emits_combinational_adder() {
        // y (id 2) = a (0) + b (1), width 8.
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
        };
        let ll = emit_module_ir(&design);
        // State layout, accessors, settle, and the add+mask are present.
        assert!(ll.contains("@cur = internal global [3 x i64]"), "{ll}");
        assert!(ll.contains("define void @sx_settle()"), "{ll}");
        assert!(ll.contains("define void @sx_set(i32"), "{ll}");
        assert!(ll.contains("define i64 @sx_read(i32"), "{ll}");
        assert!(ll.contains("add i64"), "{ll}");
        assert!(ll.contains("and i64") && ll.contains("255"), "mask to width 8:\n{ll}");
    }

    #[test]
    #[should_panic(expected = "64-bit-word only")]
    fn rejects_signals_wider_than_64_bits() {
        // A uint[128] signal would truncate in an i64 slot — reject it.
        let design = Design {
            signals: vec![sig("E.a", 128)],
            drivers: vec![Driver {
                ctx: 0,
                target: SignalId(0),
                cond: None,
                expr: Expr::Const(1),
            }],
            event_blocks: vec![],
            enum_syms: Default::default(),
            new_defaults: Default::default(),
            base_dir: Default::default(),
        };
        emit_module_ir(&design);
    }

    #[test]
    fn topo_orders_a_chain() {
        // Drivers declared out of dependency order: y=c, c=b, b=a. The emitted
        // settle must compute b, then c, then y (each after its input).
        let design = Design {
            signals: vec![sig("E.a", 8), sig("E.b", 8), sig("E.c", 8), sig("E.y", 8)],
            drivers: vec![
                Driver { target: SignalId(3), cond: None, expr: Expr::Current(SignalId(2)), ctx: 0 }, // y=c
                Driver { target: SignalId(2), cond: None, expr: Expr::Current(SignalId(1)), ctx: 0 }, // c=b
                Driver { target: SignalId(1), cond: None, expr: Expr::Current(SignalId(0)), ctx: 0 }, // b=a
            ],
            event_blocks: vec![],
            enum_syms: Default::default(),
            new_defaults: Default::default(),
            base_dir: Default::default(),
        };
        let ll = emit_module_ir(&design);
        // In the settle body, the store to b's slot precedes the store to y's.
        let body = ll.split("@sx_settle()").nth(1).unwrap();
        let store_b = body.find("i64 0, i64 1").expect("b store"); // gep index 1 = b
        let store_y = body.find("i64 0, i64 3").expect("y store"); // gep index 3 = y
        assert!(store_b < store_y, "b must settle before y:\n{body}");
    }
}
