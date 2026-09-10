//! Semantic lints and lowering diagnostics that require finished design facts.

use super::*;

impl<'a> Lowering<'a> {
    /// Combinational-loop lint (W-P010): a combinational signal whose value
    /// depends on itself through only combinational drivers is a zero-delay
    /// cycle with no register to break it — it has no well-defined settled
    /// value (the engines stop it at an arbitrary point). Event-block
    /// (sequential) targets break a cycle, so only comb→comb edges count.
    pub(super) fn lint_combinational_loops(&mut self) {
        use std::collections::{BTreeSet, HashMap, HashSet};
        let procs = self.out.processes();
        // Signals driven combinationally, and for each its comb dependencies
        // (reads that are themselves combinational targets).
        let comb_targets: HashSet<u32> = procs
            .iter()
            .filter_map(|p| match p.kind {
                ProcessKind::Comb { target, .. } => Some(target.0),
                _ => None,
            })
            .collect();
        let mut deps: HashMap<u32, Vec<u32>> = HashMap::new();
        for p in &procs {
            if let ProcessKind::Comb { target, .. } = p.kind {
                let e = deps.entry(target.0).or_default();
                for r in &p.reads {
                    if comb_targets.contains(&r.0) {
                        e.push(r.0);
                    }
                }
            }
        }
        // A signal on a cycle can reach itself. Report each such signal once.
        let reaches_self = |start: u32| -> bool {
            let mut stack = deps.get(&start).cloned().unwrap_or_default();
            let mut seen: HashSet<u32> = HashSet::new();
            while let Some(n) = stack.pop() {
                if n == start {
                    return true;
                }
                if seen.insert(n) {
                    if let Some(next) = deps.get(&n) {
                        stack.extend(next.iter().copied());
                    }
                }
            }
            false
        };
        let mut looped: BTreeSet<u32> = BTreeSet::new();
        for &t in &comb_targets {
            if reaches_self(t) {
                looped.insert(t);
            }
        }
        for t in looped {
            let signal = &self.out.signals[t as usize];
            let path = signal.path.clone();
            self.sink.emit(
                crate::diag::Diagnostic::warning(format!(
                    "`{path}` is in a combinational loop — its value depends on itself \
                     with no register in the path, so it has no settled value"
                ))
                .with_code(crate::diag::codes::COMBINATIONAL_LOOP)
                .at(signal.declaration_span)
                .help("break the loop with a clocked register, or an unconditional default"),
            );
        }
    }

    /// Undriven-output lint (W-P011): a plain `out` port (non-bus, non-`inout`,
    /// non-extern) with no combinational driver and no event-block update is
    /// never driven inside its entity — its value is stuck at the reset default.
    pub(super) fn lint_undriven_outputs(&mut self) {
        let mut driven: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for d in &self.out.drivers {
            driven.insert(d.target.0);
        }
        for eb in &self.out.event_blocks {
            for u in &eb.updates {
                driven.insert(u.target.0);
            }
        }
        let ports = std::mem::take(&mut self.plain_out_ports);
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for sig in ports {
            if !seen.insert(sig.0) || driven.contains(&sig.0) {
                continue;
            }
            let signal = &self.out.signals[sig.0 as usize];
            let path = signal.path.clone();
            self.sink.emit(
                crate::diag::Diagnostic::warning(format!("output port `{path}` is never driven"))
                    .with_code(crate::diag::codes::UNDRIVEN_OUTPUT)
                    .at(signal.declaration_span)
                    .help("drive it inside the entity, or make it an `in`/`inout` port"),
            );
        }
        // Internal value-less `let` signals that are never driven — a forgotten
        // assignment (they read `0` forever).
        let lets = std::mem::take(&mut self.undriven_lets);
        for sig in lets {
            if !seen.insert(sig.0) || driven.contains(&sig.0) {
                continue;
            }
            let signal = &self.out.signals[sig.0 as usize];
            let path = signal.path.clone();
            self.sink.emit(
                crate::diag::Diagnostic::warning(format!("signal `{path}` is never driven"))
                    .with_code(crate::diag::codes::UNDRIVEN_OUTPUT)
                    .at(signal.declaration_span)
                    .help("assign it, give it an initial value, or remove it"),
            );
        }
    }

    /// Unused-signal lint (W-P003): an internal component local that no
    /// combinational or sequential process reads contributes no observable
    /// behavior. Root locals are excluded because an external harness
    /// reads them outside the hardware IR.
    pub(super) fn lint_unused_signals(&mut self) {
        let processes = self.out.processes();
        let read: std::collections::HashSet<u32> = processes
            .iter()
            .flat_map(|process| process.reads.iter().map(|id| id.0))
            .collect();
        let driven: std::collections::HashSet<u32> = self
            .out
            .drivers
            .iter()
            .map(|driver| driver.target.0)
            .chain(
                self.out
                    .event_blocks
                    .iter()
                    .flat_map(|block| block.updates.iter().map(|update| update.target.0)),
            )
            .collect();
        let mut seen = std::collections::HashSet::new();
        for signal in std::mem::take(&mut self.unused_lets) {
            if !seen.insert(signal.0) || read.contains(&signal.0) || !driven.contains(&signal.0) {
                continue;
            }
            let signal = &self.out.signals[signal.0 as usize];
            let path = signal.path.clone();
            self.sink.emit(
                crate::diag::Diagnostic::warning(format!("signal `{path}` is never read"))
                    .with_code(crate::diag::codes::UNUSED_SIGNAL)
                    .at(signal.declaration_span)
                    .help("remove it, or use its value in observable logic"),
            );
        }
    }

    /// Possible-latch lint (W-P002): a *combinational* signal that is only ever
    /// assigned under a condition keeps its previous value when no condition
    /// holds — an inferred latch. We flag the clean case: a single driver
    /// context whose drivers are all conditional. Event-block (sequential)
    /// signals hold by design, and multi-context signals go through `Resolve`,
    /// so both are excluded to avoid false positives.
    pub(super) fn lint_possible_latches(&mut self) {
        use std::collections::{BTreeMap, BTreeSet};
        // Sequential state: any signal a clocked block updates.
        let mut sequential: BTreeSet<u32> = BTreeSet::new();
        for eb in &self.out.event_blocks {
            for u in &eb.updates {
                sequential.insert(u.target.0);
            }
        }
        // Per signal: its driver contexts, and whether any driver is a default.
        let mut ctxs: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
        let mut has_default: BTreeMap<u32, bool> = BTreeMap::new();
        for d in &self.out.drivers {
            ctxs.entry(d.target.0).or_default().insert(d.ctx);
            let e = has_default.entry(d.target.0).or_insert(false);
            *e |= d.cond.is_none();
        }
        for (t, default) in &has_default {
            if *default
                || sequential.contains(t)
                || ctxs[t].len() > 1
                || self.lint_defaulted.contains(t)
            {
                continue;
            }
            let signal = &self.out.signals[*t as usize];
            let path = signal.path.clone();
            self.sink.emit(
                crate::diag::Diagnostic::warning(format!(
                    "`{path}` is only assigned under a condition, so it holds its \
                     previous value otherwise (inferred latch)"
                ))
                .with_code(crate::diag::codes::POSSIBLE_LATCH)
                .at(signal.declaration_span)
                .help("give it an unconditional default assignment"),
            );
        }
    }

    /// Report value names that matched no signal, constant or parameter.
    /// Resolution leaves plain value identifiers alone by design, so this is
    /// the first stage with the whole picture — and it used to drop them into
    /// an `Unknown` that made `check` pass and a build fail with a driver
    /// index instead of the name.
    pub(super) fn report_unresolved_names(&mut self) {
        let mut names = std::mem::take(&mut *self.unresolved_names.borrow_mut());
        names.sort_by_key(|(name, span)| (span.start, name.clone()));
        names.dedup();
        for (name, span) in names {
            self.sink.emit(
                crate::diag::Diagnostic::error(format!("no value named `{name}` is in scope"))
                    .with_code(crate::diag::codes::UNKNOWN_NAME)
                    .at(span)
                    .help(
                        "a value has to be a port, a `let` signal or local, a \
                         constant, or a parameter of the enclosing entity",
                    ),
            );
        }
    }

    /// Report a reference to an in-range instance-array slot that concrete
    /// generate elaboration omitted. This is distinct from an out-of-bounds
    /// index: the slot belongs to the declaration, but no child instance (and
    /// therefore no port signals) exists for this parameter set.
    pub(super) fn report_unelaborated_instance_uses(&mut self) {
        let mut uses = std::mem::take(&mut *self.unelaborated_instance_uses.borrow_mut());
        uses.sort_by_key(|use_| {
            (
                use_.use_span.file.0,
                use_.use_span.start,
                use_.slot.clone(),
                use_.parent_path.clone(),
            )
        });
        uses.dedup_by(|left, right| {
            left.use_span == right.use_span
                && left.slot == right.slot
                && left.parent_path == right.parent_path
        });
        for use_ in uses {
            self.sink.emit(
                crate::diag::Diagnostic::error(format!(
                    "instance `{}` was not elaborated in `{}`",
                    use_.slot, use_.parent_path
                ))
                .with_code(crate::diag::codes::INSTANCE_NOT_ELABORATED)
                .at(use_.use_span)
                .label(
                    use_.declaration_span,
                    format!("instance array `{}` declared here", use_.slot_root()),
                )
                .help(
                    "this slot was omitted by the active generate conditions; \
                     guard the reference with the same condition, or construct \
                     the slot for this parameter set",
                ),
            );
        }
    }

    /// Record `e` when its flattened path begins with a declared-but-unbuilt
    /// instance slot in the body currently being lowered.
    pub(super) fn record_unelaborated_instance_use(&self, e: &ast::Expr) -> bool {
        let Some(path) = self.folded_elem_path(e).or_else(|| expr_path(e)) else {
            return false;
        };
        let Some(facts) = self.instance_array_facts.get(&self.cur_instance_path) else {
            return false;
        };
        for fact in facts {
            for &index in &fact.declared {
                if fact.built.contains(&index) {
                    continue;
                }
                let slot = format!("{}[{index}]", fact.name);
                if path == slot
                    || path
                        .strip_prefix(&slot)
                        .is_some_and(|rest| rest.starts_with('.'))
                {
                    self.unelaborated_instance_uses
                        .borrow_mut()
                        .push(UnelaboratedInstanceUse {
                            slot,
                            parent_path: self.cur_instance_path.clone(),
                            use_span: ast::expr_span(e),
                            declaration_span: fact.span,
                        });
                    return true;
                }
            }
        }
        false
    }

    /// A generic entity analysed without a concrete parameter set may not yet
    /// know either its instance-array bounds or which generate branches build
    /// its slots. Do not turn that uncertainty into E-P017; concrete
    /// instantiations carry a hierarchy fact and are handled above.
    pub(super) fn is_unresolved_instance_array_reference(&self, e: &ast::Expr) -> bool {
        let Some(path) = self.folded_elem_path(e).or_else(|| expr_path(e)) else {
            return false;
        };
        let Some((root, _)) = path.split_once('[') else {
            return false;
        };
        self.instance_arrays.contains(root)
            && !self
                .instance_array_facts
                .get(&self.cur_instance_path)
                .is_some_and(|facts| facts.iter().any(|fact| fact.name == root))
    }

    /// An assignment whose left side lowering cannot place. Two different
    /// mistakes arrive here and used to share one message that carried no
    /// span, no code and no help: an index form with no lowering contract, and
    /// an expression that is not a place at all.
    pub(super) fn report_bad_assign_target(&mut self, target: &ast::Expr) {
        let text = crate::syntax::pretty::expr_string(target);
        let span = ast::expr_span(target);
        let diag = if matches!(target, ast::Expr::Index { .. }) {
            crate::diag::Diagnostic::error(format!("cannot assign to `{text}`"))
                .with_code(crate::diag::codes::UNSUPPORTED_EXPR)
                .help(
                    "runtime indices may traverse declared arrays and packed vectors; \
                     packed-vector slice bounds must be constant, and a custom container needs \
                     an `IndexAssign` implementation",
                )
        } else {
            crate::diag::Diagnostic::error(format!("`{text}` cannot be assigned to"))
                .with_code(crate::diag::codes::INVALID_ASSIGN_TARGET)
                .help(
                    "an assignment target is a signal, a field, an index, a slice, \
                     or a concatenation of those",
                )
        };
        self.sink.emit(diag.at(span));
    }

    /// Report field/index expressions that reached no hardware form. From the
    /// IR these are anonymous `Unknown`s, and validation could only say which
    /// signal's driver held one; here the source spelling is still available.
    pub(super) fn report_unsupported_exprs(&mut self) {
        let mut exprs = std::mem::take(&mut *self.unsupported_exprs.borrow_mut());
        exprs.sort_by_key(|(text, span)| (span.start, text.clone()));
        exprs.dedup();
        for (text, span) in exprs {
            self.sink.emit(
                crate::diag::Diagnostic::error(format!("`{text}` has no hardware form"))
                    .with_code(crate::diag::codes::UNSUPPORTED_EXPR)
                    .at(span)
                    .help(
                        "runtime indices may traverse declared arrays and packed vectors; \
                         packed-vector slice bounds must be constant, and a custom container needs \
                         an `Index` implementation",
                    ),
            );
        }
    }

    /// Report a constant index that falls outside the array it indexes, at the
    /// point where every index has finally become constant.
    ///
    /// `types` reports this already (`E-P003`), but only for an index that is a
    /// *literal in the source*. An index that becomes constant later — a
    /// generate loop's unrolled variable, or an entity parameter substituted at
    /// elaboration — arrived here unchecked, and lowering had no complaint to
    /// make: a write to an element that does not exist found no signal and was
    /// dropped, and a read of one clamped to the last element. Both silent.
    /// `for i in 0..3 { v[i] = .. }` on a 4-element `v` is the same statement
    /// as `v[4] = ..` after unrolling, and only one of the two was an error.
    ///
    /// The shape that motivates it: ranges are directional (`2..1` counts
    /// down), so a parameterized `for i in 0..(N - 1)` with `N = 0` iterates
    /// `0, -1` and quietly drove element -1.
    #[allow(clippy::type_complexity)]
    pub(super) fn report_bad_indices(
        &mut self,
        found: Vec<(
            String,
            i64,
            i64,
            i64,
            usize,
            &'static str,
            crate::diag::Span,
        )>,
    ) {
        for (name, value, lo, hi, len, noun, span) in found {
            if !self.reported_oob.insert((name.clone(), value, span.start)) {
                continue;
            }
            // Worded exactly as the `types` check words it, so the two routes
            // to the same mistake read the same.
            let unit = if noun == "bit" { "vector" } else { "array" };
            self.sink.emit(
                crate::diag::Diagnostic::error(format!(
                    "{noun} {value} is outside `{lo}..{hi}` of this {len}-{noun} {unit}"
                ))
                .with_code(crate::diag::codes::TYPE_MISMATCH)
                .at(span)
                .help(
                    "the index is constant after elaboration — check the generate \
                     range or the parameter it came from against the declared size",
                ),
            );
        }
    }

    /// Walk `e` for indexed reads/writes whose index const-folds in the current
    /// environment and lands outside the base array. Reported through the
    /// caller so the walk itself can stay behind `&self`.
    #[allow(clippy::type_complexity)]
    pub(super) fn collect_bad_indices(
        &self,
        e: &ast::Expr,
        out: &mut Vec<(
            String,
            i64,
            i64,
            i64,
            usize,
            &'static str,
            crate::diag::Span,
        )>,
    ) {
        use ast::Expr as E;
        match e {
            E::Index { base, index, span } => {
                self.collect_bad_indices(base, out);
                self.collect_bad_indices(index, out);
                // A runtime index (`mem[addr]`) does not fold and is not this
                // check's business. A *slice* is: both its bounds fold the same
                // way a scalar index does, and an unchecked one either reads
                // the missing bits as 0 (`x[i+8..i]` on an 8-bit `x`) or wraps
                // negative through a `u32` and surfaces as an internal
                // "slice bounds lo 4294967295 > hi 1" with no source location.
                let Some(path) = expr_path(base) else {
                    return;
                };
                let bounds: Vec<i64> = match index.as_ref() {
                    E::Range { lo, hi, .. } => [lo, hi]
                        .iter()
                        .filter_map(|b| self.eval_const(b, &self.cur_env))
                        .collect(),
                    // An omitted bound is supplied by the vector itself, so
                    // only the written one can be wrong.
                    E::PartialRange { lo, hi, .. } => [lo, hi]
                        .iter()
                        .filter_map(|b| b.as_deref())
                        .filter_map(|b| self.eval_const(b, &self.cur_env))
                        .collect(),
                    other => self.eval_const(other, &self.cur_env).into_iter().collect(),
                };
                for v in bounds {
                    self.check_one_index(&path, v, *span, out);
                }
            }

            E::Field { base, .. } | E::SysAttr { base, .. } | E::Unary { rhs: base, .. } => {
                self.collect_bad_indices(base, out)
            }
            E::Binary { lhs, rhs, .. }
            | E::Range {
                lo: lhs, hi: rhs, ..
            } => {
                self.collect_bad_indices(lhs, out);
                self.collect_bad_indices(rhs, out);
            }
            E::PartialRange { lo, hi, .. } => {
                if let Some(lo) = lo {
                    self.collect_bad_indices(lo, out);
                }
                if let Some(hi) = hi {
                    self.collect_bad_indices(hi, out);
                }
            }
            E::IfExpr {
                cond, then, els, ..
            } => {
                self.collect_bad_indices(cond, out);
                self.collect_bad_indices(then, out);
                self.collect_bad_indices(els, out);
            }
            E::Match {
                scrutinee, arms, ..
            } => {
                self.collect_bad_indices(scrutinee, out);
                for a in arms {
                    for s in &a.body.stmts {
                        self.collect_stmt_bad_indices(s, out);
                    }
                }
            }
            E::Call { callee, args, .. } => {
                self.collect_bad_indices(callee, out);
                for a in args {
                    self.collect_bad_indices(a, out);
                }
            }
            E::Construct { args, spread, .. } => {
                for a in args {
                    if let Some(v) = &a.value {
                        self.collect_bad_indices(v, out);
                    }
                }
                if let Some(s) = spread {
                    self.collect_bad_indices(s, out);
                }
            }
            E::Concat { parts: es, .. } | E::Array { elems: es, .. } => {
                for x in es {
                    self.collect_bad_indices(x, out);
                }
            }
            E::Int { .. }
            | E::SuffixLit { .. }
            | E::BitStrLit { .. }
            | E::CharLit { .. }
            | E::StrLit { .. }
            | E::Path(_) => {}
        }
    }

    /// Check one folded index or slice bound against `path`'s declared bounds.
    ///
    /// An element array carries its own index list, so a declared descending
    /// range (`Bit[7..0]` -> 7, 6, .., 0) needs no special case. Anything else
    /// falls back to the declared range, which covers packed vectors indexed
    /// by bit.
    #[allow(clippy::type_complexity)]
    pub(super) fn check_one_index(
        &self,
        path: &str,
        v: i64,
        span: crate::diag::Span,
        out: &mut Vec<(
            String,
            i64,
            i64,
            i64,
            usize,
            &'static str,
            crate::diag::Span,
        )>,
    ) {
        if let Some(indices) = self.local_array.get(path) {
            if !indices.contains(&v) {
                let (lo, hi) = (
                    indices.iter().copied().min().unwrap_or(0),
                    indices.iter().copied().max().unwrap_or(0),
                );
                let noun = if self.instance_arrays.contains(path) {
                    "instance"
                } else {
                    "element"
                };
                out.push((path.to_string(), v, lo, hi, indices.len(), noun, span));
            }
        } else if let Some((left, right)) = self.persisted_range(path) {
            let (lo, hi) = (left.min(right), left.max(right));
            if v < lo || v > hi {
                let len = (hi - lo + 1) as usize;
                out.push((path.to_string(), v, lo, hi, len, "bit", span));
            }
        }
    }

    /// The statement form of [`Self::collect_bad_indices`]. A nested block is
    /// walked too: a `for` body is re-dispatched here once per iteration with
    /// its index substituted, and until then the loop variable does not fold,
    /// so the untaken walk finds nothing and costs nothing.
    #[allow(clippy::type_complexity)]
    pub(super) fn collect_stmt_bad_indices(
        &self,
        s: &ast::Stmt,
        out: &mut Vec<(
            String,
            i64,
            i64,
            i64,
            usize,
            &'static str,
            crate::diag::Span,
        )>,
    ) {
        match s {
            ast::Stmt::Assign {
                target,
                value,
                after,
                ..
            } => {
                self.collect_bad_indices(target, out);
                self.collect_bad_indices(value, out);
                if let Some(a) = after {
                    self.collect_bad_indices(a, out);
                }
            }
            ast::Stmt::Let(l) => {
                if let Some(v) = &l.value {
                    self.collect_bad_indices(v, out);
                }
            }
            ast::Stmt::Expr(e) => self.collect_bad_indices(e, out),
            ast::Stmt::Return { value: Some(v), .. } => self.collect_bad_indices(v, out),
            ast::Stmt::Return { .. } => {}
            ast::Stmt::If(iff) => self.collect_if_bad_indices(iff, out),
            ast::Stmt::Match(m) => {
                self.collect_bad_indices(&m.scrutinee, out);
                for a in &m.arms {
                    for s in &a.body.stmts {
                        self.collect_stmt_bad_indices(s, out);
                    }
                }
            }
            ast::Stmt::For { range, body, .. } => {
                self.collect_bad_indices(range, out);
                for s in &body.stmts {
                    self.collect_stmt_bad_indices(s, out);
                }
            }
        }
    }

    #[allow(clippy::type_complexity)]
    /// Collect constant indices outside their declared bounds from an `if`
    /// chain, so each is reported once.
    pub(super) fn collect_if_bad_indices(
        &self,
        iff: &ast::IfStmt,
        out: &mut Vec<(
            String,
            i64,
            i64,
            i64,
            usize,
            &'static str,
            crate::diag::Span,
        )>,
    ) {
        self.collect_bad_indices(&iff.cond, out);
        // A condition that const-folds selects one branch at elaboration and
        // the other is never built, so its indices are not the design's. The
        // idiom this protects is the ordinary one for a generated chain —
        // `if i == 0 { s[0] = d; } else { s[i] = s[i - 1]; }` — whose `else`
        // reads `s[-1]` on the very iteration that does not take it.
        let taken = self.eval_const(&iff.cond, &self.cur_env).map(|v| v != 0);
        if taken != Some(false) {
            for s in &iff.then.stmts {
                self.collect_stmt_bad_indices(s, out);
            }
        }
        if taken == Some(true) {
            return;
        }
        match iff.else_.as_deref() {
            Some(ast::ElseBranch::Block(b)) => {
                for s in &b.stmts {
                    self.collect_stmt_bad_indices(s, out);
                }
            }
            Some(ast::ElseBranch::If(inner)) => self.collect_if_bad_indices(inner, out),
            None => {}
        }
    }

    /// Report a binary operator with no impl for its right operand's type.
    pub(super) fn report_bad_operators(&mut self) {
        let mut items = std::mem::take(&mut *self.bad_operators.borrow_mut());
        items.sort_by_key(|(o, l, r, sp)| (sp.start, o.clone(), l.clone(), r.clone()));
        items.dedup();
        for (op, lhs, rhs, span) in items {
            let with = match &rhs {
                Some(r) => format!("a right operand of type `{r}`"),
                None => "this right operand".to_string(),
            };
            self.sink.emit(
                crate::diag::Diagnostic::error(format!(
                    "no `{op}` operator for `{lhs}` with {with}"
                ))
                .with_code(crate::diag::codes::TYPE_MISMATCH)
                .at(span)
                .help(
                    "an operator is `impl Operator<\"<sym>\", Rhs, Out> for T`, and the \
                     right operand has to match `Rhs` — a bare integer literal only \
                     matches an `Rhs` of the same type as the left operand, so convert \
                     it explicitly (`x * unsigned[8](3)`)",
                ),
            );
        }
    }

    /// Report a conversion with no route from its argument's type.
    ///
    /// `T(x)` on a named type dispatches to `impl From<S> for T` or to a total
    /// derivation (spec 3.17/3.28). With neither, lowering left an `Unknown`
    /// and the failure surfaced after every stage had reported success:
    /// "the driver for `T.e.y`: contains an Unknown (unlowered) expression",
    /// naming a signal rather than the expression, with no code and no span.
    /// `Bit(l)` on a `Logic` is the shape that finds this — narrowing away
    /// `'X'`/`'Z'` is exactly what std declines to provide a `From` for, and
    /// it is what anyone writes when shifting a bit out of a vector.
    pub(super) fn report_bad_conversions(&mut self) {
        let mut items = std::mem::take(&mut *self.bad_conversions.borrow_mut());
        items.sort_by_key(|(t, s, span)| (span.start, t.clone(), s.clone()));
        items.dedup();
        for (target, src, span) in items {
            let from = match &src {
                Some(s) => format!("`{s}`"),
                None => "this argument's type".to_string(),
            };
            self.sink.emit(
                crate::diag::Diagnostic::error(format!("no conversion from {from} to `{target}`"))
                    .with_code(crate::diag::codes::TYPE_MISMATCH)
                    .at(span)
                    .help(
                        "`T(x)` needs an `impl From<S> for T`, or a derivation \
                     chain between the two types; conversions are never implicit",
                    ),
            );
        }
    }

    /// Report any function whose inlining hit the depth guard. Recursion in
    /// hardware has to terminate at elaboration — either the arguments
    /// const-fold, or the recursion is unbounded and there is no finite circuit
    /// for it. Without this the bail-out silently leaves `Unknown` mid-driver.
    pub(super) fn report_depth_exceeded(&mut self) {
        let mut calls = std::mem::take(&mut *self.depth_exceeded.borrow_mut());
        calls.sort_by_key(|(name, span)| (span.file.0, span.start, name.clone()));
        calls.dedup();
        for (name, span) in calls {
            self.sink.emit(
                crate::diag::Diagnostic::error(format!(
                    "`{name}` recursed deeper than the inline limit, so it has no \
                     finite hardware form"
                ))
                .with_code(crate::diag::codes::UNBOUNDED_RECURSION)
                .at(span)
                .help(
                    "recursion must terminate at compile time — give it a \
                     constant-foldable argument, or rewrite it as a loop",
                ),
            );
        }
    }
}
