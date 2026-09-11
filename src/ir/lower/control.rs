//! Statement dispatch, conditional control flow, and event-process lowering.

use super::*;

impl<'a> Lowering<'a> {
    /// Lower one statement, with `cur_span` pinned to it for the duration so
    /// the drivers it pushes carry their source line. It is restored on the
    /// way out because `if`/`match` recurse through here: without that, a
    /// driver pushed by the outer statement after an inner one returned would
    /// be attributed to the inner statement's line.
    pub(super) fn lower_stmt(&mut self, stmt: &ast::Stmt, cond: Option<Expr>) {
        let outer = self.cur_span.replace(ast::stmt_span(stmt));
        self.lower_stmt_at(stmt, cond);
        self.cur_span = outer;
    }

    /// Lower one statement under an optional guard, which accumulates as
    /// branches nest.
    pub(super) fn lower_stmt_at(&mut self, stmt: &ast::Stmt, cond: Option<Expr>) {
        // Every index in this statement is as constant as it will ever be: a
        // generate `for` substitutes its variable before re-dispatching here.
        //
        // Only unconditioned statements are walked. A statement arriving with
        // a condition is one branch of an `if` this walk already visited, and
        // visiting it again reports it out of context: the walk applies branch
        // selection, so the dead half of `if i == 0 { s[0] = d } else { s[i] =
        // s[i - 1] }` is skipped at `i = 0`, but on re-entry that `s[i - 1]`
        // arrives as a bare statement with nothing left to skip it.
        if cond.is_none() {
            let mut bad = Vec::new();
            self.collect_stmt_bad_indices(stmt, &mut bad);
            self.report_bad_indices(bad);
        }
        // A sub-instance declared inside a generate block (`for i in .. { let
        // s: Sub = { .. } }`) is lowered structurally by `gather_generate`, so
        // this walk must leave it alone. Only the *assignment* spelling was
        // skipped below, and a connection value with no scalar form -- an
        // element of a struct array, `w[i]` where `w: Beat[N]` -- was then
        // lowered as an ordinary expression and reported "`w[0]` has no
        // hardware form". The same instance written at the entity's root
        // worked, and so did a scalar connection in a loop, which merely
        // lowered to a value nobody used.
        if let ast::Stmt::Let(l) = stmt {
            if instance_let_parts(l, &self.entities, self.resolved).is_some() {
                return;
            }
        }
        // An instance-array element (`stage[i] = Sub { .. }`, Sub an entity) is
        // lowered structurally by `gather_generate`, not as a behavioral driver
        // — skip it so unrolling a `for` doesn't mistake it for an assignment. A
        // struct-construct assignment (`y = Point { .. }`) is real data and
        // flows through normally.
        if let ast::Stmt::Assign {
            value: ast::Expr::Construct { ty: Some(t), .. },
            ..
        } = stmt
        {
            if type_def_id(t, self.resolved).is_some_and(|id| self.entities.contains_key(&id)) {
                return;
            }
        }
        match stmt {
            // `for i in left..right { .. }`: a generate loop — unroll over the static
            // range, substituting the index, so per-iteration drivers (and
            // nested generate-`if`s) are lowered concretely.
            ast::Stmt::For {
                var,
                range: ast::Expr::Range { lo, hi, .. },
                body,
                ..
            } => {
                if let (Some(a), Some(b)) = (
                    self.eval_const(lo, &self.cur_env),
                    self.eval_const(hi, &self.cur_env),
                ) {
                    let saved = self.cur_env.get(&var.text).copied();
                    for i in loop_range(a, b) {
                        self.cur_env.insert(var.text.clone(), i);
                        let unrolled = ast::Block {
                            stmts: body
                                .stmts
                                .iter()
                                .map(|statement| subst_stmt(statement, &var.text, i))
                                .collect(),
                            span: body.span,
                        };
                        self.lower_combinational_block(&unrolled, cond.clone());
                    }
                    match saved {
                        Some(v) => {
                            self.cur_env.insert(var.text.clone(), v);
                        }
                        None => {
                            self.cur_env.remove(&var.text);
                        }
                    }
                }
            }
            ast::Stmt::Assign {
                target,
                value,
                after,
                span,
            } => {
                // `after` delays are testbench stimulus, not synthesizable
                // hardware (Phase 1): reject rather than silently drop.
                if after.is_some() {
                    self.sink.emit(
                        crate::diag::Diagnostic::error(
                            "`after` delays are only allowed in #[test] testbenches (Phase 1)"
                                .to_string(),
                        )
                        .with_code(crate::diag::codes::TYPE_MISMATCH)
                        .at(*span),
                    );
                }
                if self.assign_block_local(target, value, &cond) {
                    return;
                }
                if let ast::Expr::Index { base, index, .. } = target {
                    if expr_path(base)
                        .as_deref()
                        .is_some_and(|path| self.local_struct.contains_key(path))
                    {
                        if let Some(index) = self.index_argument(index) {
                            if self.lower_method_stmt(
                                base,
                                "index_assign",
                                &[index, value.clone()],
                                cond.clone(),
                            ) {
                                return;
                            }
                        }
                    }
                }
                // Strict assignment width: a scalar signal target and a direct
                // signal-reference value must have equal, both-known widths
                // (spec 3.17 — no implicit resize). Arithmetic and conversions
                // are exempt (see `ref_width`); array/struct targets aren't in
                // `locals` so they fall through untouched.
                if let Some(tpath) = expr_path(target) {
                    if let Some(&tid) = self.locals.get(&tpath) {
                        let tw = self.out.signals[tid.0 as usize].width;
                        if let Some(sw) = self.ref_width(value) {
                            // Ranged kernel integers are constraints/subtypes
                            // of one numeric type, not packed-vector families.
                            // Their storage widths may differ across a normal
                            // assignment; range checking (static for constants,
                            // runtime for dynamic values) governs validity.
                            let integer_to_integer = self.out.signals[tid.0 as usize].integer
                                && expr_path(value)
                                    .and_then(|path| self.locals.get(&path))
                                    .is_some_and(|id| self.out.signals[id.0 as usize].integer);
                            if tw > 0 && sw > 0 && tw != sw && !integer_to_integer {
                                self.sink.emit(
                                    crate::diag::Diagnostic::error(format!(
                                        "width mismatch: `{tpath}` is {tw} bits but the \
                                         assigned value is {sw} bits"
                                    ))
                                    .with_code(crate::diag::codes::TYPE_MISMATCH)
                                    .at(*span)
                                    .help(
                                        "widths must match; use a conversion \
                                           (`unsigned[N](x)` / `resize(x, N)`) to change width",
                                    ),
                                );
                            }
                        }
                    }
                }
                // A struct-typed target takes one driver per flattened field
                // (struct copy, struct literal, or an inlined operator impl).
                if let Some(tpath) = expr_path(target) {
                    // Whole-array assignment: a string literal fills a Char
                    // array per element; an array of the same shape copies.
                    if let Some(indices) = self.local_array.get(&tpath).cloned() {
                        if let Some(binding) = self.block_local_binding(value) {
                            if let (Val::Fields(fields), Some((_, source_indices))) = (
                                binding.value,
                                array_of(
                                    &binding.ty,
                                    &self.cur_env,
                                    &self.const_ranges,
                                    &self.array_families,
                                    &self.free_fns,
                                ),
                            ) {
                                for (target_index, source_index) in
                                    indices.iter().zip(source_indices)
                                {
                                    let source = fields
                                        .iter()
                                        .find(|(name, _)| *name == format!("[{source_index}]"));
                                    let target =
                                        self.locals.get(&format!("{tpath}[{target_index}]"));
                                    if let (Some((_, expression)), Some(&target)) = (source, target)
                                    {
                                        self.out.drivers.push(Driver {
                                            span: self.cur_span,
                                            target,
                                            cond: cond.clone(),
                                            expr: self.coerce_to_target(target, expression.clone()),
                                            meta: None,
                                            ctx: self.cur_ctx,
                                        });
                                    }
                                }
                                return;
                            }
                        }
                        // An array-returning call has no array form of its own —
                        // the inliner's result is a scalar or named fields, and
                        // an array is neither — so `g = gives()` reported that
                        // `gives()` had no element-wise form. Reducing the call
                        // to the expression it returns, with the arguments
                        // substituted, hands it to the arms below: the literal
                        // it returns is driven element by element exactly as a
                        // literal written at the assignment would be.
                        let reduced = self.returned_expr_from_call(value);
                        let value = reduced.as_ref().unwrap_or(value);
                        match value {
                            ast::Expr::StrLit { text, .. } => {
                                let chars: Vec<char> = text.chars().collect();
                                if chars.len() != indices.len() {
                                    self.sink.emit(
                                        crate::diag::Diagnostic::error(format!(
                                            "string literal length {} does not match `{tpath}` length {}",
                                            chars.len(),
                                            indices.len()
                                        ))
                                        .with_code(crate::diag::codes::TYPE_MISMATCH)
                                        .at(ast::expr_span(value)),
                                    );
                                    return;
                                }
                                for (c, i) in chars.iter().zip(&indices) {
                                    if let Some(&sig) = self.locals.get(&format!("{tpath}[{i}]")) {
                                        // A char-enum element (`Color[3] = "rgb"`)
                                        // takes the variant's discriminant; a
                                        // plain `Char` array takes the code point.
                                        let val = self.out.signals[sig.0 as usize]
                                            .enum_type
                                            .clone()
                                            .and_then(|en| self.char_disc(*c, &en))
                                            .unwrap_or(*c as u32 as u64);
                                        self.out.drivers.push(Driver {
                                            span: self.cur_span,
                                            target: sig,
                                            cond: cond.clone(),
                                            expr: Expr::Const(val),
                                            meta: None,
                                            ctx: self.cur_ctx,
                                        });
                                    }
                                }
                                return;
                            }
                            // `a = [e0, e1, ...];` drives one element per value.
                            ast::Expr::Array { elems, .. } => {
                                if elems.len() != indices.len() {
                                    self.sink.emit(
                                        crate::diag::Diagnostic::error(format!(
                                            "array literal length {} does not match `{tpath}` length {}",
                                            elems.len(),
                                            indices.len()
                                        ))
                                        .with_code(crate::diag::codes::TYPE_MISMATCH)
                                        .at(ast::expr_span(value)),
                                    );
                                    return;
                                }
                                for (e, i) in elems.iter().zip(&indices) {
                                    if let Some(&sig) = self.locals.get(&format!("{tpath}[{i}]")) {
                                        let expr = self.coerce_to_target(sig, self.lower_expr(e));
                                        self.out.drivers.push(Driver {
                                            span: self.cur_span,
                                            target: sig,
                                            cond: cond.clone(),
                                            expr,
                                            meta: None,
                                            ctx: self.cur_ctx,
                                        });
                                    }
                                }
                                return;
                            }
                            v => {
                                if let Some(vpath) = expr_path(v) {
                                    if let Some(vidx) = self.local_array.get(&vpath).cloned() {
                                        for (ti, vi) in indices.iter().zip(&vidx) {
                                            let t = self.locals.get(&format!("{tpath}[{ti}]"));
                                            let sv = self.locals.get(&format!("{vpath}[{vi}]"));
                                            if let (Some(&t), Some(&sv)) = (t, sv) {
                                                self.out.drivers.push(Driver {
                                                    span: self.cur_span,
                                                    target: t,
                                                    cond: cond.clone(),
                                                    expr: Expr::Current(sv),
                                                    meta: None,
                                                    ctx: self.cur_ctx,
                                                });
                                            }
                                        }
                                        return;
                                    }
                                }
                                // An elementwise operator over arrays
                                // (`y = a and b`, `y = not a`). std declares
                                // these as blanket impls over `T[]`, and
                                // lowering had no form for them: the
                                // assignment fell through to the scalar path,
                                // which reported the *target* as unassignable
                                // even though `y = a` is fine.
                                let mut lowered = Vec::with_capacity(indices.len());
                                for (k, i) in indices.iter().enumerate() {
                                    let element = self.elementwise_at(v, k, indices.len());
                                    let signal = self.locals.get(&format!("{tpath}[{i}]"));
                                    match (element, signal) {
                                        (Some(element), Some(&signal)) => {
                                            let expr = self.coerce_to_target(
                                                signal,
                                                self.lower_expr(&element),
                                            );
                                            lowered.push((signal, expr));
                                        }
                                        // Not elementwise after all; leave the
                                        // existing paths to diagnose it.
                                        _ => {
                                            lowered.clear();
                                            break;
                                        }
                                    }
                                }
                                if !lowered.is_empty() {
                                    for (target, expr) in lowered {
                                        self.out.drivers.push(Driver {
                                            span: self.cur_span,
                                            target,
                                            cond: cond.clone(),
                                            expr,
                                            meta: None,
                                            ctx: self.cur_ctx,
                                        });
                                    }
                                    return;
                                }
                                // `tpath` is a perfectly good target — the
                                // *value* has no array form. Falling through
                                // reached the scalar path, which failed on the
                                // target and reported it as unassignable,
                                // naming the innocent half of the statement.
                                self.sink.emit(
                                    crate::diag::Diagnostic::error(format!(
                                        "`{}` has no element-wise form, so `{tpath}` \
                                         cannot be driven from it",
                                        crate::syntax::pretty::expr_string(v)
                                    ))
                                    .with_code(crate::diag::codes::UNSUPPORTED_EXPR)
                                    .at(ast::expr_span(v))
                                    .help(
                                        "an array is driven by another array, an array \
                                         literal, or an element-wise expression over \
                                         arrays of the same length",
                                    ),
                                );
                                return;
                            }
                        }
                    }
                    if self.local_struct.contains_key(&tpath) {
                        // Same expansion the clocked path uses — one write per
                        // leaf. Keeping two copies is how the clocked one came
                        // to be missing in the first place.
                        for (sig, expr) in
                            self.struct_assign_leaves(target, value).unwrap_or_default()
                        {
                            self.out.drivers.push(Driver {
                                span: self.cur_span,
                                target: sig,
                                cond: cond.clone(),
                                expr,
                                meta: None,
                                ctx: self.cur_ctx,
                            });
                        }
                        return;
                    }
                }
                if let Some(target) = self.target_signal(target) {
                    let expr = self.coerce_to_target(target, self.lower_expr(value));
                    self.out.drivers.push(Driver {
                        span: self.cur_span,
                        target,
                        cond,
                        expr,
                        meta: self.bit_string_meta(value),
                        ctx: self.cur_ctx,
                    });
                } else if let Some(ups) = self.dynamic_write(target, value, &cond, false, &[]) {
                    for u in ups {
                        self.out.drivers.push(Driver {
                            span: self.cur_span,
                            target: u.target,
                            cond: u.cond,
                            expr: u.expr,
                            meta: u.meta,
                            ctx: self.cur_ctx,
                        });
                    }
                } else if let Some(ups) = self.dynamic_struct_write(target, value, &cond) {
                    for u in ups {
                        self.out.drivers.push(Driver {
                            span: self.cur_span,
                            target: u.target,
                            cond: u.cond,
                            expr: u.expr,
                            meta: u.meta,
                            ctx: self.cur_ctx,
                        });
                    }
                } else if let Some((sig, hi, lo)) = self.slice_target(target) {
                    // Partial write: merge over what this context has already
                    // driven (`y = base; y[3..0] = a;`). A resolved signal's
                    // first write starts from `Z` on the unnamed elements.
                    let v = self.lower_expr(value);
                    let width = self.out.signals[sig.0 as usize].width;
                    let base = self.slice_write_base(sig, false, &[]);
                    let merged = self.merge_slice(base, hi, lo, v.clone(), width);
                    let meta = if self.out.array_element_enums.contains_key(&sig.0) {
                        let companion = SignalId(self.driven_companion(sig));
                        let meta_width = self.out.signals[companion.0 as usize].width;
                        // Armed after `driven_companion`, which creates a
                        // signal: the ids these hoists are promised start at
                        // the current signal count.
                        self.arm_meta_temps(
                            self.cur_ctx,
                            self.out.signals[sig.0 as usize].declaration_span,
                        );
                        let meta_base = self.slice_meta_write_base(sig, companion, false, &[]);
                        let slice_width = hi.saturating_sub(lo) + 1;
                        let meta_value = self.partial_write_meta(value, &v, slice_width);
                        self.flush_meta_temps();
                        Some(self.merge_slice(
                            meta_base,
                            hi * 4 + 3,
                            lo * 4,
                            meta_value,
                            meta_width,
                        ))
                    } else {
                        None
                    };
                    // `merged` already folds in every driver this context has
                    // for `sig`, so it may *replace* the last one — but only
                    // when that one is unconditional and this write is too.
                    // Otherwise it has to be a new driver, or a guarded write
                    // would be applied unconditionally.
                    let last = self
                        .out
                        .drivers
                        .iter()
                        .rposition(|d| d.target == sig && d.ctx == self.cur_ctx);
                    match last {
                        Some(i) if cond.is_none() && self.out.drivers[i].cond.is_none() => {
                            self.out.drivers[i].expr = merged;
                            self.out.drivers[i].meta = meta;
                        }
                        _ => self.out.drivers.push(Driver {
                            span: self.cur_span,
                            target: sig,
                            cond,
                            expr: merged,
                            meta,
                            ctx: self.cur_ctx,
                        }),
                    }
                } else if let ast::Expr::Concat { parts, span: cspan } = target {
                    // `{hi, lo} = w;` unpacks the value MSB-first: each part
                    // takes its width's slice of the RHS.
                    self.check_concat_target_width(parts, value, *cspan);
                    let v = self.lower_expr(value);
                    let mut off: u32 = parts.iter().map(|p| self.ast_width(p)).sum();
                    for part in parts {
                        let w = self.ast_width(part);
                        let Some(t) = self.target_signal(part) else {
                            self.sink.emit(
                                crate::diag::Diagnostic::error(
                                    "each part of a concat assignment target must be a signal"
                                        .to_string(),
                                )
                                .with_code(crate::diag::codes::INVALID_ASSIGN_TARGET)
                                .at(ast::expr_span(part)),
                            );
                            continue;
                        };
                        let expr = Expr::Slice {
                            base: Box::new(v.clone()),
                            hi: off - 1,
                            lo: off - w,
                        };
                        self.out.drivers.push(Driver {
                            span: self.cur_span,
                            target: t,
                            cond: cond.clone(),
                            expr,
                            meta: None,
                            ctx: self.cur_ctx,
                        });
                        off -= w;
                    }
                } else if self.record_unelaborated_instance_use(target) {
                    // The concrete child does not exist, so there is no port
                    // signal to drive. The queued E-P022 names the real cause.
                } else {
                    self.report_bad_assign_target(target);
                }
            }
            ast::Stmt::If(iff) => {
                if expr_is_event(&iff.cond) {
                    // Event-controlled block (spec 3.11): the body's assignments
                    // become next-state updates (spec 3.13).
                    let condition = self.lower_expr(&iff.cond);
                    let mut updates = Vec::new();
                    self.lower_event_block(&iff.then, None, &mut updates);
                    // An `else` on an event block is unusual; lower it under the
                    // negated event for completeness.
                    if let Some(eb) = iff.else_.as_deref() {
                        let neg = Some(not(self.lower_expr(&iff.cond)));
                        self.lower_event_else(eb, neg, &mut updates);
                    }
                    self.out.event_blocks.push(EventBlock {
                        condition,
                        updates,
                        ctx: self.cur_ctx,
                    });
                } else if let Some(k) = self.eval_const(&iff.cond, &self.cur_env) {
                    // A generate-if: the condition is a compile-time constant
                    // (a parameter/const), so only the taken branch is lowered.
                    // Its instances were gathered structurally; lowering the
                    // untaken branch too would add a spurious driver that
                    // collides with a conditionally-instantiated block.
                    if k != 0 {
                        self.lower_combinational_block(&iff.then, cond.clone());
                    } else {
                        match iff.else_.as_deref() {
                            Some(ast::ElseBranch::Block(b)) => {
                                self.lower_combinational_block(b, cond.clone());
                            }
                            Some(ast::ElseBranch::If(inner)) => {
                                self.lower_stmt(&ast::Stmt::If(inner.clone()), cond.clone());
                            }
                            None => {}
                        }
                    }
                } else {
                    // A signal assigned on every path through this if/else (a
                    // terminal `else` supplies the complement) is fully covered
                    // — not a latch — even though each driver is conditional.
                    // Mark it like a wildcard match arm so the possible-latch
                    // lint skips it.
                    for id in self.if_covered_targets(iff) {
                        self.lint_defaulted.insert(id);
                    }
                    // Combinational conditional: assignments become conditional
                    // drivers; the `else` adds the negated condition.
                    let c = self.lower_expr(&iff.cond);
                    let then_cond = Some(and(cond.clone(), c.clone()));
                    self.lower_combinational_block(&iff.then, then_cond);
                    if let Some(eb) = iff.else_.as_deref() {
                        let else_cond = Some(and(cond, not(c)));
                        self.lower_combinational_else(eb, else_cond);
                    }
                }
            }
            ast::Stmt::Match(m) => {
                // Combinational match: each arm becomes conditional drivers
                // guarded by `scrutinee == variant` with first-match priority.
                let scrut = self.lower_expr(&m.scrutinee);
                // A match naming every variant of its scrutinee's enum is as
                // complete as one ending in `_`, so a signal every arm assigns
                // is driven on every path and is not a latch. Only the wildcard
                // half was implemented, so the natural spelling of an
                // exhaustive FSM decode drew an inferred-latch warning and the
                // fix people apply to it is a redundant `_` arm. The `if`
                // walker has had the general form all along
                // (`if_covered_targets`).
                for id in self.match_covered_targets(m) {
                    self.lint_defaulted.insert(id);
                }
                let mut remaining = cond;
                for arm in &m.arms {
                    let mc =
                        self.arm_match_cond(&arm.pattern, &m.scrutinee, &scrut, &HashMap::new());
                    // A wildcard arm is the match's default branch: its direct
                    // assignments cover "everything else", so those targets are
                    // not latches even though the lowered driver is conditional.
                    if mc.is_none() {
                        for s in &arm.body.stmts {
                            if let ast::Stmt::Assign { target, .. } = s {
                                if let Some(id) = self.target_signal(target) {
                                    self.lint_defaulted.insert(id.0);
                                }
                            }
                        }
                    }
                    let fire = match &mc {
                        Some(c) => Some(and(remaining.clone(), c.clone())),
                        None => remaining.clone(),
                    };
                    self.lower_combinational_block(&arm.body, fire);
                    remaining = match mc {
                        Some(c) => Some(and(remaining, not(c))),
                        None => Some(Expr::Const(0)),
                    };
                }
            }
            // A method call used as a statement (`s.send(v)`): inline the
            // method body as drivers on the receiver's signals (spec 3.20).
            ast::Stmt::Expr(ast::Expr::Call { callee, args, .. })
                if matches!(callee.as_ref(), ast::Expr::Field { .. }) =>
            {
                if let ast::Expr::Field { base, field, .. } = callee.as_ref() {
                    self.lower_method_stmt(base, &field.text, args, cond);
                }
            }
            // A free function used as a statement (`write(bus, value)`) may
            // itself contain method calls or assignments. Inline its body with
            // the concrete arguments just like a value-returning free call.
            ast::Stmt::Expr(ast::Expr::Call { callee, args, .. }) => {
                self.lower_free_stmt(callee, args, cond);
            }
            ast::Stmt::Let(declaration) => self.declare_block_local(declaration),
            // Other statement forms (bare expr and return) are not hardware
            // statements; the frontend diagnoses them when applicable.
            _ => {}
        }
    }

    /// The condition under which a match arm fires: `scrut == <variant value>`
    /// for an enum path, `(scrut & mask) == value` for a bit pattern with `?`
    /// don't-cares (spec 3.22), or always (`None`) for a wildcard.
    /// Lower a match-*expression* to a first-match `Select` chain: the wildcard
    /// arm's value is the base `els`, and each earlier arm wraps it under its
    /// `scrutinee == pattern` guard.
    pub(super) fn lower_match_expr(&self, scrutinee: &ast::Expr, arms: &[ast::MatchArm]) -> Expr {
        let scrut = self.lower_expr(scrutinee);
        // Every match needs a base case. With no `_`, the last arm is it: its
        // guard is redundant when the arms cover the scrutinee, and when they
        // do not this is at least a defined value rather than an `Unknown` no
        // engine can run. The checker warns about the uncovered case.
        //
        // This was computed from enum variants alone, so the exhaustive
        // spelling of a *numeric* match — `0 | 1 => a, 2..3 => b` on
        // `unsigned[2]`, and even `0..3 => a` — lowered to an expression that
        // could not execute, while the same shape over an enum was fine.
        let exhaustive = !arms.iter().any(|a| pattern_has_wildcard(&a.pattern));
        let mut result: Option<Expr> = None;
        for (i, arm) in arms.iter().enumerate().rev() {
            let val = arm
                .value_expr()
                .map(|v| self.lower_expr(v))
                .unwrap_or(Expr::Unknown);
            let last = i + 1 == arms.len();
            match self.arm_match_cond(&arm.pattern, scrutinee, &scrut, &HashMap::new()) {
                None => result = Some(val), // wildcard: the default branch
                Some(_) if exhaustive && last => result = Some(val),
                Some(cond) => {
                    let els = result.take().unwrap_or(Expr::Unknown);
                    result = Some(Expr::Select {
                        cond: Box::new(cond),
                        then: Box::new(val),
                        els: Box::new(els),
                    });
                }
            }
        }
        result.unwrap_or(Expr::Unknown)
    }

    /// [`Self::lower_match_expr`] at [`Val`] level: the same first-match
    /// chain and the same exhaustiveness rule, folded with `select_val` so
    /// struct-valued arms combine field by field.
    pub(super) fn lower_match_val(
        &self,
        scrutinee: &ast::Expr,
        arms: &[ast::MatchArm],
        env: &HashMap<String, Val>,
    ) -> Val {
        let scrut = self.lower_scalar_env(scrutinee, env);
        let exhaustive = !arms.iter().any(|a| pattern_has_wildcard(&a.pattern));
        let mut result: Option<Val> = None;
        for (i, arm) in arms.iter().enumerate().rev() {
            let val = arm
                .value_expr()
                .map(|v| self.lower_val_env(v, env))
                .unwrap_or(Val::Scalar(Expr::Unknown));
            let last = i + 1 == arms.len();
            match self.arm_match_cond(&arm.pattern, scrutinee, &scrut, env) {
                None => result = Some(val),
                Some(_) if exhaustive && last => result = Some(val),
                Some(cond) => {
                    let els = result.take().unwrap_or(Val::Scalar(Expr::Unknown));
                    result = Some(select_val(cond, val, els));
                }
            }
        }
        result.unwrap_or(Val::Scalar(Expr::Unknown))
    }

    /// The condition selecting one match arm, built from its pattern and the
    /// scrutinee.
    pub(super) fn arm_match_cond(
        &self,
        pattern: &ast::Pattern,
        scrutinee: &ast::Expr,
        scrut: &Expr,
        env: &HashMap<String, Val>,
    ) -> Option<Expr> {
        match pattern {
            ast::Pattern::Path(p) if p.segments.len() >= 2 => {
                let disc = self.enum_variant_path(p).unwrap_or(0);
                Some(eq(scrut.clone(), Expr::Const(disc)))
            }
            ast::Pattern::BitPattern { text, .. } => {
                let (mask, value) = crate::syntax::bit_pattern_mask(text)?;
                Some(eq(
                    Expr::Binary {
                        op: BinOp::And,
                        lhs: Box::new(scrut.clone()),
                        rhs: Box::new(words_const(mask)),
                    },
                    words_const(value),
                ))
            }
            // `A | B`: matches if any alternative matches (their conditions
            // OR-ed; a wildcard alternative makes the whole arm unconditional).
            ast::Pattern::Or { alts, .. } => {
                let mut acc: Option<Expr> = None;
                for a in alts {
                    match self.arm_match_cond(a, scrutinee, scrut, env) {
                        None => return None,
                        Some(c) => {
                            acc = Some(match acc {
                                Some(prev) => Expr::Binary {
                                    op: BinOp::Or,
                                    lhs: Box::new(prev),
                                    rhs: Box::new(c),
                                },
                                None => c,
                            })
                        }
                    }
                }
                acc
            }
            // An integer literal or inclusive range: `scrut == lo`, or
            // `lo <= scrut <= hi`. Reuse ordinary comparison selection so a
            // signed-vector `<=>` implementation, kernel-integer signedness,
            // and real coercion all remain identical to expression syntax.
            ast::Pattern::Range { lo, hi, span } => {
                let (low, high) = if lo <= hi { (*lo, *hi) } else { (*hi, *lo) };
                let compare = |op: ast::BinOp, value: i64| {
                    let magnitude = ast::Expr::Int {
                        text: value.unsigned_abs().to_string(),
                        span: *span,
                    };
                    let rhs = if value < 0 {
                        ast::Expr::Unary {
                            op: ast::UnOp::Neg,
                            rhs: Box::new(magnitude),
                            span: *span,
                        }
                    } else {
                        magnitude
                    };
                    let spelling = crate::syntax::pretty::bin_op(&op);
                    if let Some(derived) = self.inline_cmp(spelling, scrutinee, &rhs, env) {
                        return derived;
                    }
                    self.make_binary(
                        op,
                        scrut.clone(),
                        self.lower_scalar_env(&rhs, env),
                        self.binary_uses_kernel_integer(scrutinee, &rhs),
                        self.declares_kernel_integer(scrutinee),
                    )
                };
                if low == high {
                    Some(compare(ast::BinOp::Eq, low))
                } else {
                    let ge = compare(ast::BinOp::Ge, low);
                    let le = compare(ast::BinOp::Le, high);
                    Some(and(Some(ge), le))
                }
            }
            // A character literal names a variant of a char-valued enum
            // (`Logic` above all). `Expr::Logic` carries the character and is
            // resolved against the scrutinee's type downstream — exactly what
            // `l == '0'` in expression position already lowers to, so the two
            // spellings cannot disagree.
            ast::Pattern::CharLit { ch, .. } => Some(eq(scrut.clone(), Expr::Logic(*ch))),
            // A wildcard matches anything.
            _ => None,
        }
    }

    /// Lower the `else` side of a combinational `if`.
    pub(super) fn lower_combinational_else(&mut self, eb: &ast::ElseBranch, cond: Option<Expr>) {
        match eb {
            ast::ElseBranch::Block(b) => self.lower_combinational_block(b, cond),
            ast::ElseBranch::If(inner) => {
                self.lower_stmt(&ast::Stmt::If(inner.clone()), cond);
            }
        }
    }

    /// Lower the body of an event-controlled block into next-state updates,
    /// accumulating the priority condition through nested `if`/`else`.
    pub(super) fn lower_event_block(
        &mut self,
        block: &ast::Block,
        cond: Option<Expr>,
        out: &mut Vec<NextUpdate>,
    ) {
        let outer = self.cur_span;
        self.block_scopes.borrow_mut().push(HashMap::new());
        for s in &block.stmts {
            // Same reason as `lower_stmt`: pin the statement being lowered so
            // its next-state updates can name their source line.
            self.cur_span = Some(ast::stmt_span(s));
            // The clocked path unrolls its own `for` (below) rather than going
            // through `lower_stmt`, so the same check has to be made here —
            // under the same unconditioned-only rule, which on this path is
            // what keeps a clocked generate chain's dead `else` unreported.
            if cond.is_none() {
                let mut bad = Vec::new();
                self.collect_stmt_bad_indices(s, &mut bad);
                self.report_bad_indices(bad);
            }
            match s {
                ast::Stmt::Assign {
                    target,
                    value,
                    after,
                    span,
                } => {
                    if after.is_some() {
                        self.sink.emit(
                            crate::diag::Diagnostic::error(
                                "`after` delays are only allowed in #[test] testbenches (Phase 1)"
                                    .to_string(),
                            )
                            .with_code(crate::diag::codes::TYPE_MISMATCH)
                            .at(*span),
                        );
                    }
                    if self.assign_block_local(target, value, &cond) {
                        continue;
                    }
                    if let Some(leaves) = self.struct_assign_leaves(target, value) {
                        // A registered bus: one next-state update per leaf.
                        for (sig, expr) in leaves {
                            out.push(NextUpdate {
                                span: self.cur_span,
                                target: sig,
                                cond: cond.clone(),
                                expr,
                                meta: None,
                            });
                        }
                    } else if let Some(leaves) = self.array_assign_leaves(target, value) {
                        // A registered array: one next-state update per element.
                        for (sig, expr) in leaves {
                            out.push(NextUpdate {
                                span: self.cur_span,
                                target: sig,
                                cond: cond.clone(),
                                expr,
                                meta: None,
                            });
                        }
                    } else if let Some(target) = self.target_signal(target) {
                        let expr = self.lower_expr(value);
                        out.push(NextUpdate {
                            span: self.cur_span,
                            target,
                            cond: cond.clone(),
                            expr,
                            meta: self.bit_string_meta(value),
                        });
                    } else if let Some(ups) = self.dynamic_write(target, value, &cond, true, out) {
                        out.extend(ups);
                    } else if let Some(ups) = self.dynamic_struct_write(target, value, &cond) {
                        out.extend(ups);
                    } else if let Some((sig, hi, lo)) = self.slice_target(target) {
                        // Register bit-field update: next(y) holds the other
                        // bits (read-modify-write on the value this block has
                        // produced so far, which is `Current` until something
                        // in it writes the signal).
                        let v = self.lower_expr(value);
                        let width = self.out.signals[sig.0 as usize].width;
                        let base = self.slice_write_base(sig, true, out);
                        let expr = self.merge_slice(base, hi, lo, v.clone(), width);
                        let meta = if self.out.array_element_enums.contains_key(&sig.0) {
                            let companion = SignalId(self.driven_companion(sig));
                            let meta_width = self.out.signals[companion.0 as usize].width;
                            self.arm_meta_temps(
                                self.cur_ctx,
                                self.out.signals[sig.0 as usize].declaration_span,
                            );
                            let meta_base = self.slice_meta_write_base(sig, companion, true, out);
                            let slice_width = hi.saturating_sub(lo) + 1;
                            let meta_value = self.partial_write_meta(value, &v, slice_width);
                            self.flush_meta_temps();
                            Some(self.merge_slice(
                                meta_base,
                                hi * 4 + 3,
                                lo * 4,
                                meta_value,
                                meta_width,
                            ))
                        } else {
                            None
                        };
                        out.push(NextUpdate {
                            span: self.cur_span,
                            target: sig,
                            cond: cond.clone(),
                            expr,
                            meta,
                        });
                    } else if let ast::Expr::Concat { parts, span: cspan } = target {
                        // `{hi, lo} = w;` in a clocked block: each part takes
                        // its width's slice of the RHS, MSB-first.
                        self.check_concat_target_width(parts, value, *cspan);
                        let v = self.lower_expr(value);
                        let mut off: u32 = parts.iter().map(|p| self.ast_width(p)).sum();
                        for part in parts {
                            let w = self.ast_width(part);
                            let Some(t) = self.target_signal(part) else {
                                self.sink.emit(
                                    crate::diag::Diagnostic::error(
                                        "each part of a concat assignment target must be a signal"
                                            .to_string(),
                                    )
                                    .with_code(crate::diag::codes::INVALID_ASSIGN_TARGET)
                                    .at(ast::expr_span(part)),
                                );
                                continue;
                            };
                            let expr = Expr::Slice {
                                base: Box::new(v.clone()),
                                hi: off - 1,
                                lo: off - w,
                            };
                            out.push(NextUpdate {
                                span: self.cur_span,
                                target: t,
                                cond: cond.clone(),
                                expr,
                                meta: None,
                            });
                            off -= w;
                        }
                    } else if self.record_unelaborated_instance_use(target) {
                        // As above, suppress the generic assign-target error;
                        // E-P022 identifies the absent concrete child.
                    } else {
                        self.report_bad_assign_target(target);
                    }
                }
                ast::Stmt::If(iff) => {
                    let c = self.lower_expr(&iff.cond);
                    self.lower_event_block(&iff.then, Some(and(cond.clone(), c.clone())), out);
                    if let Some(eb) = iff.else_.as_deref() {
                        let neg = Some(and(cond.clone(), not(c)));
                        self.lower_event_else(eb, neg, out);
                    }
                }
                ast::Stmt::Match(m) => {
                    let scrut = self.lower_expr(&m.scrutinee);
                    let mut remaining = cond.clone();
                    for arm in &m.arms {
                        let mc = self.arm_match_cond(
                            &arm.pattern,
                            &m.scrutinee,
                            &scrut,
                            &HashMap::new(),
                        );
                        let fire = match &mc {
                            Some(c) => Some(and(remaining.clone(), c.clone())),
                            None => remaining.clone(),
                        };
                        self.lower_event_block(&arm.body, fire, out);
                        remaining = match mc {
                            Some(c) => Some(and(remaining, not(c))),
                            None => Some(Expr::Const(0)),
                        };
                    }
                }
                // A call in statement position — `a.bump(step)`, or a free
                // `write(bus, v)` — inlines its body under the edge condition,
                // so its assignments become register updates. The
                // combinational walker has always inlined these; the
                // sequential one dropped them, so `if clk.rising() {
                // a.bump(step); }` left the register at its reset value with
                // no diagnostic. The body itself is shared, so a call means
                // the same thing in both positions.
                ast::Stmt::Expr(ast::Expr::Call {
                    callee, args, span, ..
                }) => {
                    let inlined = match callee.as_ref() {
                        ast::Expr::Field { base, field, .. } => {
                            let (base, field) = (base.clone(), field.text.clone());
                            self.method_stmt_body(&base, &field, args)
                        }
                        _ => self.free_stmt_body(callee, args),
                    };
                    if let Some(stmts) = inlined {
                        let block = ast::Block { stmts, span: *span };
                        self.lower_event_block(&block, cond.clone(), out);
                    }
                }
                // A generate `for` unrolls here exactly as it does in
                // combinational position (spec: a loop unrolls over a static
                // range, "instances *and* per-iteration drivers"). The
                // combinational walker has always done this; the sequential
                // one fell through its catch-all, so every driver a loop
                // wrote inside a clocked block was dropped in silence —
                // `if clk.rising() { for i in 0..2 { v = i; } }` left `v`
                // at its initial value with no diagnostic.
                ast::Stmt::For {
                    var,
                    range: ast::Expr::Range { lo, hi, .. },
                    body,
                    ..
                } => {
                    let (Some(a), Some(b)) = (
                        self.eval_const(lo, &self.cur_env),
                        self.eval_const(hi, &self.cur_env),
                    ) else {
                        continue;
                    };
                    let saved = self.cur_env.get(&var.text).copied();
                    for i in loop_range(a, b) {
                        self.cur_env.insert(var.text.clone(), i);
                        let unrolled = ast::Block {
                            stmts: body
                                .stmts
                                .iter()
                                .map(|st| subst_stmt(st, &var.text, i))
                                .collect(),
                            span: body.span,
                        };
                        self.lower_event_block(&unrolled, cond.clone(), out);
                    }
                    match saved {
                        Some(v) => {
                            self.cur_env.insert(var.text.clone(), v);
                        }
                        None => {
                            self.cur_env.remove(&var.text);
                        }
                    }
                }
                ast::Stmt::Let(declaration) => self.declare_block_local(declaration),
                _ => {}
            }
        }
        self.block_scopes.borrow_mut().pop();
        self.cur_span = outer;
    }

    /// Lower the `else` side of an event-controlled `if`.
    pub(super) fn lower_event_else(
        &mut self,
        eb: &ast::ElseBranch,
        cond: Option<Expr>,
        out: &mut Vec<NextUpdate>,
    ) {
        match eb {
            ast::ElseBranch::Block(b) => self.lower_event_block(b, cond, out),
            ast::ElseBranch::If(inner) => {
                let c = self.lower_expr(&inner.cond);
                self.lower_event_block(&inner.then, Some(and(cond.clone(), c.clone())), out);
                if let Some(eb) = inner.else_.as_deref() {
                    self.lower_event_else(eb, Some(and(cond, not(c))), out);
                }
            }
        }
    }
}
