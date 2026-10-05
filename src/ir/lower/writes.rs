//! Dynamic, sliced, aggregate, and coverage-aware write lowering.

use super::*;

impl<'a> Lowering<'a> {
    /// Lower a scalar leaf reached through one or more runtime array indices.
    /// Arrays and structs are flattened (`m[0][1]`, `pack[0].data`), so each
    /// index expands over the concrete paths registered in `local_array` and
    /// each field simply extends the path. A mux arm remains as backend
    /// recovery, but its checked index makes an active miss a simulation
    /// failure rather than a selected language value.
    pub(super) fn lower_dynamic_access(&self, access: &ast::Expr) -> Option<Expr> {
        let (root, steps) = access_steps(access)?;
        if !steps
            .iter()
            .any(|step| matches!(step, AccessStep::Index(_)))
        {
            return None;
        }
        self.lower_dynamic_access_from(&root, &steps)
    }

    /// Read through a walked access path into a signal.
    pub(super) fn lower_dynamic_access_from(
        &self,
        path: &str,
        steps: &[AccessStep<'_>],
    ) -> Option<Expr> {
        let Some((step, rest)) = steps.split_first() else {
            return self.locals.get(path).copied().map(Expr::Current);
        };
        match step {
            AccessStep::Field(field) => {
                self.lower_dynamic_access_from(&format!("{path}.{field}"), rest)
            }
            AccessStep::Index(index) => {
                if let Some(indices) = self.local_array.get(path) {
                    let (&last, earlier) = indices.split_last()?;
                    let lowered_index = self.checked_runtime_index(index, indices)?;
                    let element = |position: i64| {
                        self.lower_dynamic_access_from(&format!("{path}[{position}]"), rest)
                    };
                    let mut result = element(last)?;
                    for &position in earlier.iter().rev() {
                        let span = ast::expr_span(index);
                        let condition = self.source_binary(
                            BinOp::Eq,
                            &lowered_index,
                            &index_label(position),
                            span,
                        );
                        result = self.source_select(&condition, &element(position)?, &result, span);
                    }
                    return Some(result);
                }
                if !rest.is_empty()
                    || matches!(
                        index,
                        ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
                    )
                {
                    return None;
                }
                let signal = *self.locals.get(path)?;
                let positions = self.packed_positions(path)?;
                if let Some(logical) = self.eval_const(index, &self.cur_env) {
                    let physical = positions
                        .iter()
                        .find_map(|&(label, position)| (label == logical).then_some(position))?;
                    return Some(self.source_slice(
                        &Expr::Current(signal),
                        physical,
                        physical,
                        ast::expr_span(index),
                    ));
                }
                let labels: Vec<i64> = positions.iter().map(|(label, _)| *label).collect();
                let (left, right) = self.persisted_range(path)?;
                let lowered_index =
                    self.checked_runtime_index_with_bounds(index, &labels, left, right)?;
                let span = ast::expr_span(index);
                let position = self.source_binary(
                    BinOp::Sub,
                    &lowered_index,
                    &index_label(left.min(right)),
                    span,
                );
                let position = self.bind_source_scalar(position, span, crate::types::Ty::Integer);
                let shifted =
                    self.source_binary(BinOp::Shr, &Expr::Current(signal), &position, span);
                Some(self.source_slice(&shifted, 0, 0, span))
            }
        }
    }

    /// A dynamic aggregate write (`mem[addr] = v`, `m[row][col] = v`, or
    /// `pack[slot].data = v`): enumerate every concrete scalar leaf and gate it
    /// by all runtime index comparisons. An out-of-range write matches no leaf
    /// after its checked index has latched the simulation failure.
    pub(super) fn dynamic_write(
        &mut self,
        target: &ast::Expr,
        value: &ast::Expr,
        cond: &Option<Expr>,
        sequential: bool,
        pending: &[NextUpdate],
    ) -> Option<Vec<NextUpdate>> {
        let (root, steps) = access_steps(target)?;
        if !steps
            .iter()
            .any(|step| matches!(step, AccessStep::Index(_)))
        {
            return None;
        }
        let mut targets = Vec::new();
        self.dynamic_write_targets(&root, &steps, None, &mut targets)?;
        let expr = self.lower_expr(value);
        let target_span = ast::expr_span(target);
        let mut updates = Vec::new();
        for target in targets {
            match target {
                DynamicWriteTarget::Whole { signal, hit } => updates.push(NextUpdate {
                    span: self.cur_span,
                    target: signal,
                    cond: self.source_write_guard(cond, hit, target_span),
                    expr: self.coerce_to_target(signal, expr.clone()),
                    meta: None,
                }),
                DynamicWriteTarget::PackedBit {
                    signal,
                    position,
                    hit,
                } => {
                    let width = self.out.signals[signal.0 as usize].width;
                    let base = self.slice_write_base(signal, sequential, pending);
                    let encoding = self
                        .out
                        .array_element_enums
                        .get(&signal.0)
                        .and_then(|element| self.logic_encoding(element))
                        .cloned();
                    let value_bit = encoding
                        .as_ref()
                        .map(|encoding| {
                            self.source_values.borrow_mut().value_bit(
                                &expr,
                                encoding,
                                ast::expr_span(value),
                            )
                        })
                        .unwrap_or_else(|| expr.clone());
                    let meta = if self.out.array_element_enums.contains_key(&signal.0) {
                        let companion = SignalId(self.driven_companion(signal));
                        let meta_width = self.out.signals[companion.0 as usize].width;
                        self.arm_meta_temps(
                            self.cur_ctx,
                            self.out.signals[signal.0 as usize].declaration_span,
                            cond.clone(),
                        );
                        let meta_base =
                            self.slice_meta_write_base(signal, companion, sequential, pending);
                        self.flush_meta_temps();
                        let span = ast::expr_span(value);
                        let meta_value = encoding.as_ref().map(|encoding| {
                            let binary = self.source_values.borrow_mut().disc_in(
                                &expr,
                                &encoding.binary,
                                span,
                            );
                            self.source_select(&binary, &Expr::Const(0), &expr, span)
                        })?;
                        let meta_position =
                            self.source_binary(BinOp::Mul, &position, &Expr::Const(4), span);
                        Some(self.merge_dynamic_slice(
                            meta_base,
                            meta_position,
                            4,
                            meta_value,
                            meta_width,
                            target_span,
                        ))
                    } else {
                        None
                    };
                    updates.push(NextUpdate {
                        span: self.cur_span,
                        target: signal,
                        cond: self.source_write_guard(cond, hit.clone(), target_span),
                        expr: self.merge_dynamic_slice(
                            base,
                            position,
                            1,
                            value_bit,
                            width,
                            target_span,
                        ),
                        meta,
                    });
                }
            }
        }
        Some(updates)
    }

    /// A whole struct written to an array element chosen at runtime
    /// (`slots[i] = { .tag = t, .val = v }`).
    ///
    /// The element has no signal of its own — its fields are `slots[0].tag`,
    /// `slots[0].val` — so the leaf lookup that the runtime-index expansion
    /// depends on found nothing and the statement was reported as an
    /// unassignable target. Everything around it lowered: the same write at a
    /// *constant* index, a single *field* of it at a runtime index, and a
    /// runtime index into an array of scalars.
    ///
    /// One update per element per field, each gated on the index matching that
    /// element, which is the same shape the scalar expansion produces.
    pub(super) fn dynamic_aggregate_write(
        &self,
        target: &ast::Expr,
        value: &ast::Expr,
        cond: &Option<Expr>,
    ) -> Option<Vec<NextUpdate>> {
        let ast::Expr::Index { base, index, .. } = target else {
            return None;
        };
        if matches!(
            index.as_ref(),
            ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
        ) {
            return None;
        }
        // A constant index already resolves to one element's leaves.
        if self.eval_const(index, &self.cur_env).is_some() {
            return None;
        }
        let base_path = expr_path(base)?;
        let indices = self.local_array.get(&base_path)?.clone();
        // Scalars are the existing dynamic_write path's business. Aggregate
        // elements share the contextual value and recursive leaf mapping.
        let layout = self.persisted_layout(&format!("{base_path}[{}]", indices.first()?))?;
        if !matches!(
            layout.kind,
            LayoutKind::Array { .. } | LayoutKind::Struct { .. }
        ) {
            return None;
        }
        let Val::Fields(fields) = self.lower_shaped_source(value, &HashMap::new(), layout) else {
            return None;
        };
        let lowered_index = self.checked_runtime_index(index, &indices)?;
        let mut updates = Vec::new();
        for position in indices {
            let hit = self.source_binary(
                BinOp::Eq,
                &lowered_index,
                &index_label(position),
                ast::expr_span(index),
            );
            for (field, expr) in &fields {
                let separator = if field.starts_with('[') { "" } else { "." };
                let Some(&signal) = self
                    .locals
                    .get(&format!("{base_path}[{position}]{separator}{field}"))
                else {
                    continue;
                };
                updates.push(NextUpdate {
                    span: self.cur_span,
                    target: signal,
                    cond: Some(self.source_and(cond.clone(), hit.clone(), ast::expr_span(target))),
                    expr: self.coerce_to_target(signal, expr.clone()),
                    meta: None,
                });
            }
        }
        (!updates.is_empty()).then_some(updates)
    }

    /// The discriminant plane that preceding writes in this context have
    /// produced. It mirrors [`Self::slice_write_base`] but reads the metadata
    /// retained on each value write instead of relying on independently ordered
    /// companion writes.
    pub(super) fn slice_meta_write_base(
        &self,
        signal: SignalId,
        companion: SignalId,
        sequential: bool,
        pending: &[NextUpdate],
    ) -> Expr {
        let span = self
            .cur_span
            .unwrap_or(self.out.signals[signal.0 as usize].declaration_span);
        let write_meta = |expr: &Expr, explicit: &Option<Expr>| {
            explicit
                .clone()
                .or_else(|| {
                    let mut temps = self.meta_temps.borrow_mut();
                    self.lower_meta_ir(expr, self.out.signals[signal.0 as usize].width, &mut temps)
                })
                .unwrap_or(Expr::Const(0))
        };
        if sequential {
            let seed = self
                .hardware
                .event_blocks
                .iter()
                .flat_map(|block| {
                    block
                        .updates
                        .iter()
                        .filter(|update| update.target == signal)
                        .map(move |update| {
                            let guard = match &update.cond {
                                Some(cond) => self.source_binary(
                                    BinOp::And,
                                    &block.condition,
                                    cond,
                                    update.span.unwrap_or(span),
                                ),
                                None => block.condition.clone(),
                            };
                            (guard, write_meta(&update.expr, &update.meta))
                        })
                })
                .fold(Expr::Current(companion), |acc, (guard, expr)| {
                    self.source_select(&guard, &expr, &acc, span)
                });
            return pending
                .iter()
                .filter(|update| update.target == signal)
                .fold(seed, |acc, update| match &update.cond {
                    Some(cond) => self.source_select(
                        cond,
                        &write_meta(&update.expr, &update.meta),
                        &acc,
                        update.span.unwrap_or(span),
                    ),
                    None => write_meta(&update.expr, &update.meta),
                });
        }
        let seed = self
            .resolved_neutral_planes(signal)
            .map(|(_, meta)| meta)
            .unwrap_or(Expr::Const(0));
        self.hardware
            .drivers
            .iter()
            .filter(|driver| driver.target == signal && driver.ctx == self.cur_ctx)
            .fold(seed, |acc, driver| match &driver.cond {
                Some(cond) => self.source_select(
                    cond,
                    &write_meta(&driver.expr, &driver.meta),
                    &acc,
                    driver.span.unwrap_or(span),
                ),
                None => write_meta(&driver.expr, &driver.meta),
            })
    }

    /// What `signal` already holds where this write appears — the base a
    /// read-modify-write must merge over.
    ///
    /// It is not enough to start from the signal's *prior* value. Each write
    /// produces a whole new value for the signal, and lowering keeps only
    /// the last one that fires: event-block updates are all staged from the
    /// pre-commit state and committed in order, and combinational drivers fold
    /// as `val = cond ? expr : val`. So a second partial write that merged over
    /// `Current(sig)` (or over nothing) silently threw the first one away —
    /// `word[1] = '1'; word[3] = '1';` set bit 3 alone. Folding the writes
    /// already lowered in this context gives each one the value its
    /// predecessors left behind, which is what the source says.
    pub(super) fn slice_write_base(
        &self,
        signal: SignalId,
        sequential: bool,
        pending: &[NextUpdate],
    ) -> Expr {
        let span = self
            .cur_span
            .unwrap_or(self.out.signals[signal.0 as usize].declaration_span);
        if sequential {
            // A clocked block reads the pre-commit value, so an unwritten
            // signal keeps `Current`. Earlier *blocks* of the same driver
            // context count too: an impl may write one signal from several
            // events, and each of those blocks contributes only when its own
            // event fires. Their updates are staged from the same pre-commit
            // state, so folding them symbolically is exactly what the backend
            // computes.
            let seed = self
                .hardware
                .event_blocks
                .iter()
                .filter(|block| block.ctx == self.cur_ctx)
                .flat_map(|block| {
                    block
                        .updates
                        .iter()
                        .filter(|update| update.target == signal)
                        .map(move |update| {
                            let guard = match &update.cond {
                                Some(cond) => self.source_binary(
                                    BinOp::And,
                                    &block.condition,
                                    cond,
                                    update.span.unwrap_or(span),
                                ),
                                None => block.condition.clone(),
                            };
                            (guard, update.expr.clone())
                        })
                })
                .fold(Expr::Current(signal), |acc, (guard, expr)| {
                    self.source_select(&guard, &expr, &acc, span)
                });
            // Then this block's own updates: each expression is a complete
            // next value, so a later one supersedes exactly when it fires.
            return pending
                .iter()
                .filter(|update| update.target == signal)
                .fold(seed, |acc, update| match &update.cond {
                    Some(cond) => {
                        self.source_select(cond, &update.expr, &acc, update.span.unwrap_or(span))
                    }
                    None => update.expr.clone(),
                });
        }
        // A partial driver of a resolved packed signal contributes `Z` on the
        // elements it does not name. That is what lets independent concurrent
        // slice assignments compose instead of forcing zero against one
        // another. Unresolved/two-valued signals retain the historical zero
        // seed until their own undriven-value model is represented explicitly.
        let seed = self
            .resolved_neutral_planes(signal)
            .map(|(value, _)| value)
            .unwrap_or(Expr::Const(0));
        self.hardware
            .drivers
            .iter()
            .filter(|driver| driver.target == signal && driver.ctx == self.cur_ctx)
            .fold(seed, |acc, driver| match &driver.cond {
                Some(cond) => {
                    self.source_select(cond, &driver.expr, &acc, driver.span.unwrap_or(span))
                }
                None => driver.expr.clone(),
            })
    }

    /// Value and discriminant planes for a packed resolved signal's neutral
    /// driver contribution. The identity comes from the source-owned
    /// `LogicEncoding`/`Resolve` contracts; no logic symbol or enum position is
    /// hardcoded in the compiler.
    pub(super) fn resolved_neutral_planes(&self, signal: SignalId) -> Option<(Expr, Expr)> {
        let element = self.out.array_element_enums.get(&signal.0)?;
        let encoding = self.logic_encoding(element)?;
        encoding.binary_ops.get("resolve")?;
        let neutral = encoding.high_impedance_value()?;
        let width = self.out.signals.get(signal.0 as usize)?.width;
        let span = self.out.signals[signal.0 as usize].declaration_span;
        let value = self.source_values.borrow_mut().repeat_plane(
            &Expr::Const(encoding.value_bit(neutral)?),
            width,
            1,
            span,
        );
        let meta_disc = if encoding.binary.contains(&neutral) {
            0
        } else {
            neutral
        };
        let meta =
            self.source_values
                .borrow_mut()
                .repeat_plane(&Expr::Const(meta_disc), width, 4, span);
        Some((value, meta))
    }

    /// Discriminant plane carried by the value written into a packed slice.
    /// Bit strings retain every explicit symbol, scalar logic literals use the
    /// target element's encoding, and computed values defer to ordinary
    /// metavalue propagation.
    pub(super) fn partial_write_meta(&self, source: &ast::Expr, value: &Expr, width: u32) -> Expr {
        if let Some(meta) = self.bit_string_meta(source) {
            return meta;
        }
        if width == 1 {
            if let ast::Expr::CharLit { ch, .. } = source {
                if let Some(disc) = self.char_disc(*ch, DEFAULT_LOGIC_TYPE) {
                    let meta = if self
                        .logic_encoding(DEFAULT_LOGIC_TYPE)
                        .is_some_and(|encoding| !encoding.binary.contains(&disc))
                    {
                        disc
                    } else {
                        0
                    };
                    return Expr::Const(meta);
                }
            }
        }
        let mut temps = self.meta_temps.borrow_mut();
        self.lower_meta_ir(value, width, &mut temps)
            .unwrap_or(Expr::Const(0))
    }

    /// The signals a dynamic write may target, with the guard selecting each.
    pub(super) fn dynamic_write_targets(
        &self,
        path: &str,
        steps: &[AccessStep<'_>],
        hit: Option<Expr>,
        out: &mut Vec<DynamicWriteTarget>,
    ) -> Option<()> {
        let Some((step, rest)) = steps.split_first() else {
            let signal = *self.locals.get(path)?;
            out.push(DynamicWriteTarget::Whole {
                signal,
                hit: hit.unwrap_or(Expr::Const(1)),
            });
            return Some(());
        };
        match step {
            AccessStep::Field(field) => {
                self.dynamic_write_targets(&format!("{path}.{field}"), rest, hit, out)
            }
            AccessStep::Index(index) => {
                if let Some(indices) = self.local_array.get(path) {
                    let lowered_index = self.checked_runtime_index(index, indices)?;
                    for &position in indices {
                        let matches = self.source_binary(
                            BinOp::Eq,
                            &lowered_index,
                            &index_label(position),
                            ast::expr_span(index),
                        );
                        self.dynamic_write_targets(
                            &format!("{path}[{position}]"),
                            rest,
                            Some(self.source_and(hit.clone(), matches, ast::expr_span(index))),
                            out,
                        )?;
                    }
                    return Some(());
                }
                if !rest.is_empty()
                    || matches!(
                        index,
                        ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
                    )
                {
                    return None;
                }
                let signal = *self.locals.get(path)?;
                let positions = self.packed_positions(path)?;
                if let Some(logical) = self.eval_const(index, &self.cur_env) {
                    let position = positions
                        .into_iter()
                        .find_map(|(label, position)| (label == logical).then_some(position))?;
                    out.push(DynamicWriteTarget::PackedBit {
                        signal,
                        position: Expr::Const(u64::from(position)),
                        hit: hit.unwrap_or(Expr::Const(1)),
                    });
                    return Some(());
                }
                let labels: Vec<i64> = positions.iter().map(|(label, _)| *label).collect();
                let (left, right) = self.persisted_range(path)?;
                let lowered_index =
                    self.checked_runtime_index_with_bounds(index, &labels, left, right)?;
                // A packed signal is one storage leaf. Compute its physical
                // position once rather than generating a full-frame write for
                // every possible bit. Freeze label arithmetic as a kernel
                // integer before wide value/companion consumers widen it.
                let span = ast::expr_span(index);
                let position = self.source_binary(
                    BinOp::Sub,
                    &lowered_index,
                    &index_label(left.min(right)),
                    span,
                );
                let position = self.bind_source_scalar(position, span, crate::types::Ty::Integer);
                out.push(DynamicWriteTarget::PackedBit {
                    signal,
                    position,
                    hit: hit.unwrap_or(Expr::Const(1)),
                });
                Some(())
            }
        }
    }

    /// A slice-assignment target `y[hi..lo]`: the base signal and the
    /// (normalized) bit range.
    pub(super) fn slice_target(&self, target: &ast::Expr) -> Option<(SignalId, u32, u32)> {
        let ast::Expr::Index { base, index, .. } = target else {
            return None;
        };
        let (a, b) = self.storage_slice_bounds(base, index)?;
        let sig = *self.locals.get(&expr_path(base)?)?;
        Some((sig, a.max(b), a.min(b)))
    }

    /// A partial (bit-slice) write as a read-modify-write over `base`:
    /// `(base & keep) | ((value & slice_mask) << lo)`, where `keep` clears the
    /// [hi..lo] window. `width` is the target signal's width.
    pub(super) fn merge_slice(
        &self,
        base: Expr,
        hi: u32,
        lo: u32,
        value: Expr,
        width: u32,
        span: crate::diag::Span,
    ) -> Expr {
        let slice_w = hi - lo + 1;
        let ones = |bits: u32| {
            let mut words = vec![u64::MAX; (bits as usize).div_ceil(64)];
            if let Some(last) = words.last_mut() {
                let used = bits % 64;
                if used != 0 {
                    *last = (1u64 << used) - 1;
                }
            }
            words
        };
        let mut keep = ones(width);
        for bit in lo..=hi {
            if let Some(word) = keep.get_mut(bit as usize / 64) {
                *word &= !(1u64 << (bit % 64));
            }
        }
        let kept = self.source_binary(BinOp::And, &base, &words_const(keep), span);
        let masked = self.source_binary(BinOp::And, &value, &words_const(ones(slice_w)), span);
        let shifted = self.source_binary(BinOp::Shl, &masked, &Expr::Const(lo as u64), span);
        self.source_binary(BinOp::Or, &kept, &shifted, span)
    }

    /// Insert one runtime-selected region without enumerating packed bits.
    /// Masks are exact-width, including companion frames wider than one ABI
    /// word. The checked position retains the source access's bounds failure.
    fn merge_dynamic_slice(
        &self,
        base: Expr,
        position: Expr,
        region_width: u32,
        value: Expr,
        width: u32,
        span: crate::diag::Span,
    ) -> Expr {
        let ones = |bits: u32| {
            let mut words = vec![u64::MAX; bits.div_ceil(64) as usize];
            if !bits.is_multiple_of(64) {
                *words.last_mut().expect("nonempty mask") = (1u64 << (bits % 64)) - 1;
            }
            words_const(words)
        };
        let position = self.bind_source_expression(position, span);
        let mask = self.source_binary(BinOp::Shl, &ones(region_width), &position, span);
        let keep = self.source_binary(BinOp::Xor, &ones(width), &mask, span);
        let kept = self.source_binary(BinOp::And, &base, &keep, span);
        let masked = self.source_binary(BinOp::And, &value, &ones(region_width), span);
        let inserted = self.source_binary(BinOp::Shl, &masked, &position, span);
        self.source_binary(BinOp::Or, &kept, &inserted, span)
    }

    /// Expand a whole-struct assignment (`bus = Bus { .. }`, `mem[0] = E { .. }`)
    /// into one write per leaf field.
    ///
    /// A struct signal is many leaves and no single id, so `target_signal`
    /// returns `None` for it. The combinational path had this expansion and
    /// the clocked path did not, so registering a bus — the ordinary way to
    /// write a pipeline stage — failed with "`e` cannot be assigned to",
    /// which reads as though the signal were an input.
    ///
    /// `Some` whenever the target *is* a struct path, so the caller stops
    /// rather than falling through to a diagnostic about a different problem.
    pub(super) fn struct_assign_leaves(
        &self,
        target: &ast::Expr,
        value: &ast::Expr,
    ) -> Option<Vec<(SignalId, Expr)>> {
        let path = self
            .folded_elem_path(target)
            .or_else(|| expr_path(target))?;
        if !matches!(
            self.persisted_layout(&path)?.kind,
            LayoutKind::Struct { .. }
        ) {
            return None;
        }
        self.aggregate_assign_leaves(target, value)
    }

    /// Expand a whole-array assignment (`g = src`, `g = [3, 4]`, `g = f()`)
    /// into one write per element.
    ///
    /// An array signal is many element signals and no single id, so
    /// `target_signal` returns `None` for it. The combinational path has had
    /// this expansion; the clocked path had none, so *every* array assignment
    /// in an event block — from another array, from a literal, from a call —
    /// fell through to the target check and reported `g` as something that
    /// cannot be assigned to. Registering an array is the ordinary way to
    /// write a pipeline, and it named the innocent half of the statement.
    pub(super) fn array_assign_leaves(
        &self,
        target: &ast::Expr,
        value: &ast::Expr,
    ) -> Option<Vec<(SignalId, Expr)>> {
        let path = self
            .folded_elem_path(target)
            .or_else(|| expr_path(target))?;
        if !matches!(self.persisted_layout(&path)?.kind, LayoutKind::Array { .. }) {
            return None;
        }
        self.aggregate_assign_leaves(target, value)
    }

    /// The signal an assignment target names, or `None` when it is not a simple
    /// place.
    pub(super) fn target_signal(&self, target: &ast::Expr) -> Option<SignalId> {
        if let Some(signal) = self.source_call_signal(target) {
            return signal;
        }
        // Prefer a constant-folded element path (`w[i+1]` with `i` bound in a
        // generate loop -> `w[3]`), so an unrolled constant index resolves to a
        // static element rather than falling through to a dynamic array write.
        if let Some(p) = self.folded_elem_path(target) {
            if let Some(&id) = self.locals.get(&p) {
                return Some(id);
            }
        }
        expr_path(target).and_then(|p| self.locals.get(&p).copied())
    }

    /// Render an element/field path with every index constant-folded through
    /// the current generate-loop environment (`w[i+1]` -> `w[3]`). `None` if any
    /// index is not a compile-time constant.
    pub(super) fn folded_elem_path(&self, e: &ast::Expr) -> Option<String> {
        match e {
            ast::Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
            ast::Expr::Field { base, field, .. } => {
                Some(format!("{}.{}", self.folded_elem_path(base)?, field.text))
            }
            ast::Expr::Index { base, index, .. } => {
                let i = self.eval_const(index, &self.cur_env)?;
                Some(format!("{}[{}]", self.folded_elem_path(base)?, i))
            }
            _ => None,
        }
    }

    /// Signals assigned on *every* path through an if/else — a terminal `else`
    /// supplies the complement, so these are fully covered and are not latches
    /// even though each driver is conditional. Without a terminal `else` the
    /// fall-through path assigns nothing, so nothing is covered. An
    /// event-controlled branch is sequential (not a combinational latch).
    pub(super) fn if_covered_targets(&self, iff: &ast::IfStmt) -> std::collections::BTreeSet<u32> {
        use std::collections::BTreeSet;
        if expr_is_event(&iff.cond) {
            return BTreeSet::new();
        }
        let then = self.block_covered_targets(&iff.then);
        let els = match iff.else_.as_deref() {
            Some(ast::ElseBranch::Block(b)) => self.block_covered_targets(b),
            Some(ast::ElseBranch::If(inner)) => self.if_covered_targets(inner),
            None => return BTreeSet::new(),
        };
        then.intersection(&els).copied().collect()
    }

    /// Signals assigned by *every* arm of a match that names every variant of
    /// its scrutinee's enum — the match equivalent of a terminal `else`. Empty
    /// when the scrutinee is not an enum, when a variant is unmatched, or when
    /// the match already has a wildcard (which the caller handles).
    pub(super) fn match_covered_targets(
        &self,
        m: &ast::MatchStmt,
    ) -> std::collections::BTreeSet<u32> {
        use std::collections::BTreeSet;
        let Some(ty) = self.operand_type_name(&m.scrutinee) else {
            return BTreeSet::new();
        };
        let Some(variants) = self.enum_variants.get(&ty) else {
            return BTreeSet::new();
        };
        let mut named: std::collections::HashSet<String> = std::collections::HashSet::new();
        for arm in &m.arms {
            collect_named_variants(&arm.pattern, &mut named);
        }
        if !variants.keys().all(|v| named.contains(v)) {
            return BTreeSet::new();
        }
        let mut covered: Option<BTreeSet<u32>> = None;
        for arm in &m.arms {
            let here = self.block_covered_targets(&arm.body);
            covered = Some(match covered {
                Some(prev) => prev.intersection(&here).copied().collect(),
                None => here,
            });
        }
        covered.unwrap_or_default()
    }

    /// Signals a block assigns on every path: its direct assignment targets,
    /// plus any target fully covered by a nested if/else.
    pub(super) fn block_covered_targets(&self, b: &ast::Block) -> std::collections::BTreeSet<u32> {
        let mut out = std::collections::BTreeSet::new();
        let mut local_names = std::collections::HashSet::new();
        for s in &b.stmts {
            match s {
                ast::Stmt::Let(declaration) => {
                    local_names.insert(declaration.name.text.as_str());
                }
                ast::Stmt::Assign { target, .. } => {
                    let root = access_steps(target).map(|(root, _)| root);
                    if root.as_deref().is_some_and(|root| {
                        local_names.contains(root) || self.block_local_named(root).is_some()
                    }) {
                        continue;
                    }
                    if let Some(covered) = self.source_call_covered_targets(target) {
                        out.extend(covered);
                        continue;
                    }
                    if let Some(id) = self.target_signal(target) {
                        out.insert(id.0);
                    } else if let Some(path) =
                        self.folded_elem_path(target).or_else(|| expr_path(target))
                    {
                        if self.persisted_layout(&path).is_some_and(|layout| {
                            matches!(
                                layout.kind,
                                LayoutKind::Array { .. } | LayoutKind::Struct { .. }
                            )
                        }) {
                            let fields = format!("{path}.");
                            let elements = format!("{path}[");
                            out.extend(self.locals.iter().filter_map(|(name, signal)| {
                                (name.starts_with(&fields) || name.starts_with(&elements))
                                    .then_some(signal.0)
                            }));
                        }
                    }
                }
                ast::Stmt::If(inner) => out.extend(self.if_covered_targets(inner)),
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_partial_writes_preserve_masks_and_share_dynamic_positions() {
        let span = crate::diag::Span::new(crate::diag::FileId(0), 20..29);
        let operand_span = crate::diag::Span::new(crate::diag::FileId(0), 11..18);
        let ones = |bits: u32| {
            if bits == 128 {
                u128::MAX
            } else {
                (1u128 << bits) - 1
            }
        };
        for width in [1, 64, 65, 128] {
            for region_width in [1, 4, 64, 65, 128]
                .into_iter()
                .filter(|region| *region <= width)
            {
                for position in [0, width - region_width] {
                    let mut sink = DiagnosticSink::new();
                    let resolved = Resolved::default();
                    let lowering = Lowering::new(&mut sink, &resolved);
                    let operand = |name: &str| {
                        lowering.bind_source_expression(
                            Expr::CCall {
                                name: name.into(),
                                args: vec![],
                                f64_args: vec![],
                                integer_args: vec![],
                                f64_ret: false,
                                integer_ret: true,
                            },
                            operand_span,
                        )
                    };
                    let base = operand("base");
                    let value = operand("value");
                    let offset = operand("position");
                    let static_write = lowering.merge_slice(
                        base.clone(),
                        position + region_width - 1,
                        position,
                        value.clone(),
                        width,
                        span,
                    );
                    let dynamic_write = lowering.merge_dynamic_slice(
                        base,
                        offset,
                        region_width,
                        value,
                        width,
                        span,
                    );
                    let arena = lowering.source_values.borrow();
                    assert!(arena.ir.validate(0).is_empty());
                    assert_eq!(
                        arena
                            .ir
                            .values
                            .iter()
                            .filter(|node| matches!(
                                node.kind,
                                ProcessValueKind::ForeignCall { .. }
                            ))
                            .count(),
                        3
                    );
                    for base in [
                        0,
                        ones(width),
                        0x1234_5678_9abc_def0_9876_5432_1abc_def0 & ones(width),
                    ] {
                        for value in [0, u128::MAX, 0xaaaa_5555_aaaa_5555] {
                            // Evaluate the canonical DAG once in dependency order, including
                            // both words. This checks masks spanning the ABI-word boundary.
                            let mut values = Vec::<u128>::new();
                            for node in &arena.ir.values {
                                let result = match &node.kind {
                                    ProcessValueKind::Number(ProcessNumber::Integer(words)) => {
                                        words.iter().enumerate().fold(0, |result, (index, word)| {
                                            result | (u128::from(*word) << (64 * index))
                                        })
                                    }
                                    ProcessValueKind::ForeignCall { name, .. } => {
                                        match name.as_str() {
                                            "base" => base,
                                            "value" => value,
                                            "position" => u128::from(position),
                                            _ => unreachable!(),
                                        }
                                    }
                                    ProcessValueKind::Binary {
                                        operation,
                                        left,
                                        right,
                                    } => {
                                        let (left, right) =
                                            (values[left.0 as usize], values[right.0 as usize]);
                                        match operation {
                                            ProcessBinaryOp::And => left & right,
                                            ProcessBinaryOp::Or => left | right,
                                            ProcessBinaryOp::Xor => left ^ right,
                                            ProcessBinaryOp::Shl => {
                                                left.checked_shl(right as u32).unwrap_or(0)
                                            }
                                            _ => panic!("unexpected merge operation"),
                                        }
                                    }
                                    _ => panic!("unexpected merge node"),
                                };
                                values.push(result);
                            }
                            let region = ones(region_width);
                            let expected = (base & (ones(width) ^ (region << position)))
                                | ((value & region) << position);
                            for expression in [&static_write, &dynamic_write] {
                                let Expr::Canonical { value: root, .. } = expression else {
                                    panic!("merge must be canonical")
                                };
                                assert_eq!(arena.ir.values[root.0 as usize].span, span);
                                assert_eq!(
                                    values[root.0 as usize], expected,
                                    "width={width} region={region_width} position={position}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
