//! Generated-statement diagnostics and process-local value lowering.

use super::*;

impl<'a> Lowering<'a> {
    /// Warn when specialization makes two unconditional assignments target
    /// the same concrete signal. The type checker handles direct source
    /// sequences, but cannot know a generic entity's parameter values or
    /// substitute generate-loop variables. Do that here, beside the actual
    /// unroller, and emit only when at least one assignment was generated so
    /// ordinary source warnings are not duplicated.
    pub(super) fn lint_generated_dead_assignments<'s>(
        &mut self,
        statements: impl Iterator<Item = &'s ast::Stmt>,
    ) {
        let mut seen = HashMap::new();
        for statement in statements {
            self.lint_generated_dead_statement(statement, false, &mut seen);
        }
    }

    /// Report a generated statement that can have no effect (E-P019), so a
    /// misspelled name does not compile clean.
    pub(super) fn lint_generated_dead_statement(
        &mut self,
        statement: &ast::Stmt,
        generated: bool,
        seen: &mut HashMap<String, (crate::diag::Span, bool)>,
    ) {
        match statement {
            ast::Stmt::Assign { target, span, .. } => {
                let key = crate::syntax::pretty::expr_string(target);
                if let Some((previous, previous_generated)) =
                    seen.insert(key.clone(), (*span, generated))
                {
                    let frontend_reported = self.sink.diagnostics().iter().any(|diagnostic| {
                        diagnostic.code == Some(crate::diag::codes::DEAD_ASSIGNMENT)
                            && diagnostic.primary == Some(*span)
                            && diagnostic.labels.iter().any(|label| label.span == previous)
                    });
                    if !frontend_reported
                        && (generated || previous_generated)
                        && self.reported_generated_dead_assignments.insert((
                            previous,
                            *span,
                            key.clone(),
                        ))
                    {
                        self.sink.emit(
                            crate::diag::Diagnostic::warning(format!(
                                "`{key}` is assigned again here; the earlier assignment has no effect"
                            ))
                            .with_code(crate::diag::codes::DEAD_ASSIGNMENT)
                            .at(*span)
                            .label(previous, "this generated assignment is overridden")
                            .help(
                                "remove the overlapping generated assignment, or make one of them conditional",
                            ),
                        );
                    }
                }
            }
            ast::Stmt::For {
                var,
                range: ast::Expr::Range { lo, hi, .. },
                body,
                ..
            } => {
                let (Some(left), Some(right)) = (
                    self.eval_const(lo, &self.cur_env),
                    self.eval_const(hi, &self.cur_env),
                ) else {
                    seen.clear();
                    return;
                };
                for index in loop_range(left, right) {
                    for nested in &body.stmts {
                        let nested = subst_stmt(nested, &var.text, index);
                        self.lint_generated_dead_statement(&nested, true, seen);
                    }
                }
            }
            ast::Stmt::If(conditional) => {
                let Some(selected) = self.eval_const(&conditional.cond, &self.cur_env) else {
                    seen.clear();
                    return;
                };
                if selected != 0 {
                    for nested in &conditional.then.stmts {
                        self.lint_generated_dead_statement(nested, true, seen);
                    }
                } else {
                    match conditional.else_.as_deref() {
                        Some(ast::ElseBranch::Block(block)) => {
                            for nested in &block.stmts {
                                self.lint_generated_dead_statement(nested, true, seen);
                            }
                        }
                        Some(ast::ElseBranch::If(nested)) => self.lint_generated_dead_statement(
                            &ast::Stmt::If(nested.clone()),
                            true,
                            seen,
                        ),
                        None => {}
                    }
                }
            }
            // Any runtime control flow or statement with effects we do not
            // flatten ends the unconditional run, matching the frontend lint.
            _ => seen.clear(),
        }
    }

    /// Lower a top-level (combinational-context) statement. `cond` accumulates
    /// the enclosing combinational conditions.
    pub(super) fn lower_combinational_block(&mut self, block: &ast::Block, cond: Option<Expr>) {
        self.block_scopes.borrow_mut().push(HashMap::new());
        for statement in &block.stmts {
            self.lower_stmt(statement, cond.clone());
        }
        self.block_scopes.borrow_mut().pop();
    }

    /// Find the innermost block-local binding addressed by a source path.
    /// The suffix is empty for the whole value and is a flattened field name
    /// (`inner.valid`) for a struct leaf.
    pub(super) fn block_local_path(
        &self,
        expression: &ast::Expr,
    ) -> Option<(usize, String, String)> {
        let path = self
            .folded_elem_path(expression)
            .or_else(|| expr_path(expression))?;
        let scopes = self.block_scopes.borrow();
        for (scope_index, scope) in scopes.iter().enumerate().rev() {
            for name in scope.keys() {
                let suffix = if path == *name {
                    ""
                } else if let Some(rest) = path.strip_prefix(name) {
                    if rest.starts_with('.') || rest.starts_with('[') {
                        rest
                    } else {
                        continue;
                    }
                } else {
                    continue;
                };
                return Some((
                    scope_index,
                    name.clone(),
                    suffix.strip_prefix('.').unwrap_or(suffix).to_string(),
                ));
            }
        }
        None
    }

    /// The block-local binding an expression names, if it is one.
    pub(super) fn block_local_binding(&self, expression: &ast::Expr) -> Option<BlockLocal> {
        let (scope, name, _) = self.block_local_path(expression)?;
        self.block_scopes.borrow().get(scope)?.get(&name).cloned()
    }

    /// Find a block local by name, returning its scope depth so inner scopes
    /// shadow outer ones.
    pub(super) fn block_local_named(&self, name: &str) -> Option<(usize, BlockLocal)> {
        self.block_scopes
            .borrow()
            .iter()
            .enumerate()
            .rev()
            .find_map(|(scope, bindings)| {
                bindings.get(name).cloned().map(|binding| (scope, binding))
            })
    }

    /// The declared type at a local path. Aggregate roots retain their own
    /// type while a selected struct field or array element carries the leaf's
    /// type for width attributes and operator dispatch.
    pub(super) fn block_local_type(&self, expression: &ast::Expr) -> Option<ast::Type> {
        match expression {
            ast::Expr::Path(_) => self
                .block_local_binding(expression)
                .map(|binding| binding.ty),
            ast::Expr::Field { base, field, .. } => {
                let base_type = self.block_local_type(base)?;
                self.struct_fields(&base_type)?
                    .into_iter()
                    .find(|(name, _)| *name == field.text)
                    .map(|(_, ty)| ty)
            }
            ast::Expr::Index { base, .. } => {
                let base_type = self.block_local_type(base)?;
                array_of(
                    &base_type,
                    &self.cur_env,
                    &self.const_ranges,
                    &self.array_families,
                    &self.free_fns,
                )
                .map(|(element, _)| element.clone())
            }
            _ => None,
        }
    }

    /// The current value of a block local named by an expression.
    pub(super) fn block_local_value(&self, expression: &ast::Expr) -> Option<Val> {
        let (scope, name, suffix) = self.block_local_path(expression)?;
        let binding = self.block_scopes.borrow().get(scope)?.get(&name)?.clone();
        if suffix.is_empty() {
            return Some(binding.value);
        }
        let Val::Fields(fields) = binding.value else {
            return None;
        };
        fields
            .into_iter()
            .find(|(field, _)| *field == suffix)
            .map(|(_, value)| Val::Scalar(value))
    }

    /// Runtime aggregate access into a storage-free block local. Its leaves
    /// use the same flattened suffixes as signals (`[0][1]`, `[0].data`), but
    /// live as expressions inside `Val::Fields` rather than `SignalId`s.
    pub(super) fn lower_block_dynamic_access(&self, expression: &ast::Expr) -> Option<Expr> {
        let (root, steps) = access_steps(expression)?;
        if !steps
            .iter()
            .any(|step| matches!(step, AccessStep::Index(_)))
        {
            return None;
        }
        let (_, binding) = self.block_local_named(&root)?;
        match binding.value {
            Val::Scalar(value) => {
                let [AccessStep::Index(index)] = steps.as_slice() else {
                    return None;
                };
                self.lower_block_packed_read(&binding.ty, value, index)
            }
            Val::Fields(fields) => {
                self.lower_block_dynamic_access_from(&binding.ty, "", &steps, &fields)
            }
        }
    }

    /// Lower a non-constant index while retaining the declared index domain
    /// for simulation. Domains are represented as equality predicates rather
    /// than signed min/max comparisons: an `unsigned` index and a negative
    /// integer label then keep their own source semantics, and ascending and
    /// descending declarations use the same backend contract.
    pub(super) fn checked_runtime_index(&self, index: &ast::Expr, labels: &[i64]) -> Option<Expr> {
        let &left = labels.first()?;
        let right = labels.last().copied().unwrap_or(left);
        self.checked_runtime_index_with_bounds(index, labels, left, right)
    }

    /// Wrap a runtime index in its declared-domain predicate, so an
    /// out-of-range access fails with the offending value and range.
    pub(super) fn checked_runtime_index_with_bounds(
        &self,
        index: &ast::Expr,
        labels: &[i64],
        left: i64,
        right: i64,
    ) -> Option<Expr> {
        let (&first, rest) = labels.split_first()?;
        let lowered = self.lower_expr(index);
        let valid = rest.iter().copied().fold(
            eq(lowered.clone(), Expr::Const(first as u64)),
            |valid, label| or_expr(valid, eq(lowered.clone(), Expr::Const(label as u64))),
        );
        Some(Expr::CheckedIndex {
            index: Box::new(lowered),
            valid: Box::new(valid),
            left,
            right,
            span: ast::expr_span(index),
        })
    }

    /// Read one element out of a packed block local.
    pub(super) fn lower_block_packed_read(
        &self,
        ty: &ast::Type,
        value: Expr,
        index: &ast::Expr,
    ) -> Option<Expr> {
        if matches!(
            index,
            ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
        ) {
            return None;
        }
        let positions = self.block_packed_positions(ty)?;
        if let Some(logical) = self.eval_const(index, &self.cur_env) {
            let physical = positions
                .iter()
                .find_map(|&(label, position)| (label == logical).then_some(position))?;
            return Some(Expr::Slice {
                base: Box::new(value),
                hi: physical,
                lo: physical,
            });
        }
        let labels: Vec<i64> = positions.iter().map(|(label, _)| *label).collect();
        let (left, right) = self.declared_range(ty, &self.cur_env)?;
        let lowered_index = self.checked_runtime_index_with_bounds(index, &labels, left, right)?;
        let mut result = Expr::Const(0);
        for (logical, physical) in positions.into_iter().rev() {
            result = Expr::Select {
                cond: Box::new(eq(lowered_index.clone(), Expr::Const(logical as u64))),
                then: Box::new(Expr::Slice {
                    base: Box::new(value.clone()),
                    hi: physical,
                    lo: physical,
                }),
                els: Box::new(result),
            };
        }
        Some(result)
    }

    /// Read through a walked access path into a block local.
    pub(super) fn lower_block_dynamic_access_from(
        &self,
        ty: &ast::Type,
        prefix: &str,
        steps: &[AccessStep<'_>],
        fields: &[(String, Expr)],
    ) -> Option<Expr> {
        let Some((step, rest)) = steps.split_first() else {
            return fields
                .iter()
                .find(|(name, _)| name == prefix)
                .map(|(_, value)| value.clone());
        };
        match step {
            AccessStep::Field(field) => {
                let field_ty = self
                    .struct_fields(ty)?
                    .into_iter()
                    .find(|(name, _)| name == field)
                    .map(|(_, field_ty)| field_ty)?;
                let separator = if prefix.is_empty() { "" } else { "." };
                self.lower_block_dynamic_access_from(
                    &field_ty,
                    &format!("{prefix}{separator}{field}"),
                    rest,
                    fields,
                )
            }
            AccessStep::Index(index) => {
                if let Some((element_ty, indices)) = array_of(
                    ty,
                    &self.cur_env,
                    &self.const_ranges,
                    &self.array_families,
                    &self.free_fns,
                ) {
                    let (&last, earlier) = indices.split_last()?;
                    let lowered_index = self.checked_runtime_index(index, &indices)?;
                    let element = |position: i64| {
                        self.lower_block_dynamic_access_from(
                            element_ty,
                            &format!("{prefix}[{position}]"),
                            rest,
                            fields,
                        )
                    };
                    let mut result = element(last)?;
                    for &position in earlier.iter().rev() {
                        result = Expr::Select {
                            cond: Box::new(eq(lowered_index.clone(), Expr::Const(position as u64))),
                            then: Box::new(element(position)?),
                            els: Box::new(result),
                        };
                    }
                    return Some(result);
                }
                if !rest.is_empty() {
                    return None;
                }
                let value = fields
                    .iter()
                    .find(|(name, _)| name == prefix)
                    .map(|(_, value)| value.clone())?;
                self.lower_block_packed_read(ty, value, index)
            }
        }
    }

    /// The bit width of a block local's type.
    pub(super) fn block_local_width(&self, ty: &ast::Type) -> u32 {
        self.enum_representation(ty)
            .map(|(width, _)| width)
            .or_else(|| self.ranged_numeric(ty).map(|(width, _, _)| width))
            .unwrap_or_else(|| {
                type_width(
                    ty,
                    &self.cur_env,
                    &self.free_fns,
                    &self.structs,
                    &self.const_ranges,
                )
            })
    }

    /// The structural default of a block local's type -- always initialized,
    /// even when undriven.
    pub(super) fn block_local_default(&self, ty: &ast::Type) -> Val {
        if let Some((element, indices)) = array_of(
            ty,
            &self.cur_env,
            &self.const_ranges,
            &self.array_families,
            &self.free_fns,
        ) {
            let mut fields = Vec::new();
            for index in indices {
                Self::prefix_block_value(
                    &format!("[{index}]"),
                    self.block_local_default(element),
                    &mut fields,
                );
            }
            return Val::Fields(fields);
        }
        let Some(head) = self.free_fns.type_head_key(ty) else {
            return Val::Scalar(Expr::Const(0));
        };
        if let Some(fields) = self.struct_default_leaves(&head, "") {
            return Val::Fields(fields);
        }
        let value = self
            .new_defaults
            .get(&head)
            .or_else(|| self.enum_first_disc.get(&head))
            .copied()
            .unwrap_or(0);
        Val::Scalar(Expr::Const(value))
    }

    /// Prefix a value's leaf names with `prefix` and append them to `out`.
    pub(super) fn prefix_block_value(prefix: &str, value: Val, out: &mut Vec<(String, Expr)>) {
        match value {
            Val::Scalar(value) => out.push((prefix.to_string(), value)),
            Val::Fields(fields) => {
                for (field, value) in fields {
                    let separator = if field.starts_with('[') { "" } else { "." };
                    out.push((format!("{prefix}{separator}{field}"), value));
                }
            }
        }
    }

    /// Lower a value with the declaration's aggregate shape available. The
    /// general expression inliner deliberately has no contextual type, while
    /// an array literal needs exactly that context to name its flattened
    /// elements.
    pub(super) fn lower_block_value(&self, value: &ast::Expr, ty: &ast::Type) -> Val {
        if let Some((element, indices)) = array_of(
            ty,
            &self.cur_env,
            &self.const_ranges,
            &self.array_families,
            &self.free_fns,
        ) {
            if let ast::Expr::Array { elems, .. } = value {
                let mut fields = Vec::new();
                for (index, expression) in indices.into_iter().zip(elems) {
                    Self::prefix_block_value(
                        &format!("[{index}]"),
                        self.lower_block_value(expression, element),
                        &mut fields,
                    );
                }
                return Val::Fields(fields);
            }
            if let ast::Expr::StrLit { text, .. } = value {
                let mut fields = Vec::new();
                for (index, character) in indices.into_iter().zip(text.chars()) {
                    let scalar =
                        self.coerce_block_local(element, Val::Scalar(Expr::Logic(character)));
                    Self::prefix_block_value(&format!("[{index}]"), scalar, &mut fields);
                }
                return Val::Fields(fields);
            }
        }
        self.coerce_block_local(ty, self.lower_val_env(value, &HashMap::new()))
    }

    /// Apply the representation boundary a signal store would provide. A
    /// block local has no storage of its own, so width, enum-character and
    /// real coercions must be made explicit in the substituted expression.
    pub(super) fn coerce_block_local(&self, ty: &ast::Type, value: Val) -> Val {
        let Val::Scalar(mut expression) = value else {
            return value;
        };
        if let Some((_, enum_name)) = self.enum_representation(ty) {
            if let Expr::Logic(character) = expression {
                if let Some(discriminant) = self.char_disc(character, &enum_name) {
                    expression = Expr::Const(discriminant);
                } else {
                    expression = Expr::Logic(character);
                }
            }
        }
        let head = self.free_fns.type_head_key(ty).unwrap_or_default();
        if head == "Char" || struct_derives_kernel(&head, "Char", &self.structs, &self.free_fns) {
            if let Expr::Logic(character) = expression {
                expression = Expr::Const(character as u32 as u64);
            }
        }
        if head == "real" || struct_derives_kernel(&head, "real", &self.structs, &self.free_fns) {
            return Val::Scalar(self.coerce_real(expression));
        }
        let width = self.block_local_width(ty);
        if width > 0 {
            expression = Expr::Slice {
                base: Box::new(expression),
                hi: width - 1,
                lo: 0,
            };
        }
        Val::Scalar(expression)
    }

    /// Bring a block local into scope with its declared type and default.
    pub(super) fn declare_block_local(&self, declaration: &ast::LetDecl) {
        let Some(ty) = declaration.ty.clone() else {
            return;
        };
        let value = declaration
            .value
            .as_ref()
            .map(|value| self.lower_block_value(value, &ty))
            .unwrap_or_else(|| self.block_local_default(&ty));
        if let Some(scope) = self.block_scopes.borrow_mut().last_mut() {
            scope.insert(declaration.name.text.clone(), BlockLocal { value, ty });
        }
    }

    /// An assignment to a block-local value is an immediate expression
    /// rewrite, not a driver or next-state update. Return whether the target
    /// named such a local so signal lowering does not see it too.
    pub(super) fn assign_block_local(
        &self,
        target: &ast::Expr,
        value: &ast::Expr,
        cond: &Option<Expr>,
    ) -> bool {
        if self.assign_block_dynamic_access(target, value, cond) {
            return true;
        }
        // A packed local vector is one scalar expression. Its indexed writes
        // are immediate read-modify-writes over that expression, unlike an
        // array local whose elements live in `Val::Fields` below.
        if let ast::Expr::Index { base, index, .. } = target {
            if let Some((scope_index, name, suffix)) = self.block_local_path(base) {
                if suffix.is_empty() {
                    let previous = {
                        let scopes = self.block_scopes.borrow();
                        scopes
                            .get(scope_index)
                            .and_then(|scope| scope.get(&name))
                            .cloned()
                    };
                    if let Some(previous) = previous {
                        if let (Val::Scalar(old), Some((left, right))) = (
                            previous.value.clone(),
                            self.storage_slice_bounds(base, index),
                        ) {
                            let next = self.merge_slice(
                                old.clone(),
                                left.max(right),
                                left.min(right),
                                self.lower_expr(value),
                                self.block_local_width(&previous.ty),
                            );
                            let next = match cond {
                                Some(condition) => Expr::Select {
                                    cond: Box::new(condition.clone()),
                                    then: Box::new(next),
                                    els: Box::new(old),
                                },
                                None => next,
                            };
                            if let Some(scope) = self.block_scopes.borrow_mut().get_mut(scope_index)
                            {
                                scope.insert(
                                    name,
                                    BlockLocal {
                                        value: Val::Scalar(next),
                                        ty: previous.ty,
                                    },
                                );
                            }
                            return true;
                        }
                        if let Val::Scalar(old) = previous.value.clone() {
                            if !matches!(
                                index.as_ref(),
                                ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
                            ) && self.eval_const(index, &self.cur_env).is_none()
                            {
                                if let Some(positions) = self.block_packed_positions(&previous.ty) {
                                    let labels: Vec<i64> =
                                        positions.iter().map(|(label, _)| *label).collect();
                                    let Some((left, right)) =
                                        self.declared_range(&previous.ty, &self.cur_env)
                                    else {
                                        return false;
                                    };
                                    let Some(lowered_index) = self
                                        .checked_runtime_index_with_bounds(
                                            index, &labels, left, right,
                                        )
                                    else {
                                        return false;
                                    };
                                    let replacement = self.lower_expr(value);
                                    let width = self.block_local_width(&previous.ty);
                                    let mut next = old.clone();
                                    for (logical, physical) in positions.into_iter().rev() {
                                        let fire = and(
                                            cond.clone(),
                                            eq(lowered_index.clone(), Expr::Const(logical as u64)),
                                        );
                                        next = Expr::Select {
                                            cond: Box::new(fire),
                                            then: Box::new(self.merge_slice(
                                                old.clone(),
                                                physical,
                                                physical,
                                                replacement.clone(),
                                                width,
                                            )),
                                            els: Box::new(next),
                                        };
                                    }
                                    if let Some(scope) =
                                        self.block_scopes.borrow_mut().get_mut(scope_index)
                                    {
                                        scope.insert(
                                            name,
                                            BlockLocal {
                                                value: Val::Scalar(next),
                                                ty: previous.ty,
                                            },
                                        );
                                    }
                                    return true;
                                }
                            }
                        }
                        if let (Val::Fields(mut fields), Some((element_ty, indices))) = (
                            previous.value.clone(),
                            array_of(
                                &previous.ty,
                                &self.cur_env,
                                &self.const_ranges,
                                &self.array_families,
                                &self.free_fns,
                            ),
                        ) {
                            let new_element = self.lower_block_value(value, element_ty);
                            let Some(lowered_index) = self.checked_runtime_index(index, &indices)
                            else {
                                return false;
                            };
                            for position in indices {
                                let prefix = format!("[{position}]");
                                let hit = Expr::Binary {
                                    op: BinOp::Eq,
                                    lhs: Box::new(lowered_index.clone()),
                                    rhs: Box::new(Expr::Const(position as u64)),
                                };
                                let fire = and(cond.clone(), hit);
                                for (field, old) in &mut fields {
                                    let suffix = if *field == prefix {
                                        Some("")
                                    } else {
                                        field
                                            .strip_prefix(&prefix)
                                            .and_then(|rest| rest.strip_prefix('.'))
                                    };
                                    let Some(suffix) = suffix else {
                                        continue;
                                    };
                                    let replacement = match &new_element {
                                        Val::Scalar(value) if suffix.is_empty() => {
                                            Some(value.clone())
                                        }
                                        Val::Fields(values) => values
                                            .iter()
                                            .find(|(name, _)| *name == suffix)
                                            .map(|(_, value)| value.clone()),
                                        _ => None,
                                    };
                                    if let Some(replacement) = replacement {
                                        *old = Expr::Select {
                                            cond: Box::new(fire.clone()),
                                            then: Box::new(replacement),
                                            els: Box::new(old.clone()),
                                        };
                                    }
                                }
                            }
                            if let Some(scope) = self.block_scopes.borrow_mut().get_mut(scope_index)
                            {
                                scope.insert(
                                    name,
                                    BlockLocal {
                                        value: Val::Fields(fields),
                                        ty: previous.ty,
                                    },
                                );
                            }
                            return true;
                        }
                    }
                }
            }
        }
        let Some((scope_index, name, suffix)) = self.block_local_path(target) else {
            return false;
        };
        let Some(previous) = self
            .block_scopes
            .borrow()
            .get(scope_index)
            .and_then(|scope| scope.get(&name))
            .cloned()
        else {
            return false;
        };

        let next = if suffix.is_empty() {
            self.lower_block_value(value, &previous.ty)
        } else {
            let Val::Fields(mut fields) = previous.value.clone() else {
                // A suffix on a scalar that was not recognized as a packed
                // bit/slice above has no place representation. Do not
                // misclassify it as a signal write.
                self.unsupported_exprs.borrow_mut().push((
                    crate::syntax::pretty::expr_string(target),
                    ast::expr_span(target),
                ));
                return true;
            };
            let Some((_, old)) = fields.iter_mut().find(|(field, _)| *field == suffix) else {
                self.unsupported_exprs.borrow_mut().push((
                    crate::syntax::pretty::expr_string(target),
                    ast::expr_span(target),
                ));
                return true;
            };
            let new = self
                .block_local_type(target)
                .map(|ty| self.lower_block_value(value, &ty))
                .and_then(|value| match value {
                    Val::Scalar(value) => Some(value),
                    Val::Fields(_) => None,
                })
                .unwrap_or_else(|| self.lower_expr(value));
            *old = match cond {
                Some(condition) => Expr::Select {
                    cond: Box::new(condition.clone()),
                    then: Box::new(new),
                    els: Box::new(old.clone()),
                },
                None => new,
            };
            Val::Fields(fields)
        };
        let next = if suffix.is_empty() {
            match cond {
                Some(condition) => select_val(condition.clone(), next, previous.value),
                None => next,
            }
        } else {
            next
        };
        if let Some(scope) = self.block_scopes.borrow_mut().get_mut(scope_index) {
            scope.insert(
                name,
                BlockLocal {
                    value: next,
                    ty: previous.ty,
                },
            );
        }
        true
    }

    /// Immediate assignment through one or more runtime indices of a block
    /// local. Each possible flattened target leaf becomes a `Select` guarded
    /// by the conjunction of its index matches.
    pub(super) fn assign_block_dynamic_access(
        &self,
        target: &ast::Expr,
        value: &ast::Expr,
        cond: &Option<Expr>,
    ) -> bool {
        let Some((root, steps)) = access_steps(target) else {
            return false;
        };
        if !steps
            .iter()
            .any(|step| matches!(step, AccessStep::Index(_)))
        {
            return false;
        }
        let Some((scope_index, previous)) = self.block_local_named(&root) else {
            return false;
        };
        let Val::Fields(mut fields) = previous.value.clone() else {
            return false;
        };
        let mut targets = Vec::new();
        let Some(target_ty) =
            self.block_dynamic_targets(&previous.ty, "", &steps, None, &mut targets)
        else {
            return false;
        };
        let replacement = self.lower_block_value(value, &target_ty);
        for (prefix, hit) in targets {
            let fire = and(cond.clone(), hit);
            for (field, old) in &mut fields {
                let suffix = if *field == prefix {
                    Some("")
                } else if let Some(rest) = field.strip_prefix(&prefix) {
                    if let Some(rest) = rest.strip_prefix('.') {
                        Some(rest)
                    } else if rest.starts_with('[') {
                        Some(rest)
                    } else {
                        None
                    }
                } else {
                    None
                };
                let Some(suffix) = suffix else {
                    continue;
                };
                let new = match &replacement {
                    Val::Scalar(value) if suffix.is_empty() => Some(value.clone()),
                    Val::Fields(values) => values
                        .iter()
                        .find(|(name, _)| name == suffix)
                        .map(|(_, value)| value.clone()),
                    _ => None,
                };
                if let Some(new) = new {
                    *old = Expr::Select {
                        cond: Box::new(fire.clone()),
                        then: Box::new(new),
                        els: Box::new(old.clone()),
                    };
                }
            }
        }
        if let Some(scope) = self.block_scopes.borrow_mut().get_mut(scope_index) {
            scope.insert(
                root,
                BlockLocal {
                    value: Val::Fields(fields),
                    ty: previous.ty,
                },
            );
        }
        true
    }

    /// The leaves a dynamic write through `steps` may touch, with the guard that
    /// selects each.
    pub(super) fn block_dynamic_targets(
        &self,
        ty: &ast::Type,
        prefix: &str,
        steps: &[AccessStep<'_>],
        hit: Option<Expr>,
        out: &mut Vec<(String, Expr)>,
    ) -> Option<ast::Type> {
        let Some((step, rest)) = steps.split_first() else {
            out.push((prefix.to_string(), hit.unwrap_or(Expr::Const(1))));
            return Some(ty.clone());
        };
        match step {
            AccessStep::Field(field) => {
                let field_ty = self
                    .struct_fields(ty)?
                    .into_iter()
                    .find(|(name, _)| name == field)
                    .map(|(_, field_ty)| field_ty)?;
                let separator = if prefix.is_empty() { "" } else { "." };
                self.block_dynamic_targets(
                    &field_ty,
                    &format!("{prefix}{separator}{field}"),
                    rest,
                    hit,
                    out,
                )
            }
            AccessStep::Index(index) => {
                let (element_ty, indices) = array_of(
                    ty,
                    &self.cur_env,
                    &self.const_ranges,
                    &self.array_families,
                    &self.free_fns,
                )?;
                let lowered_index = self.checked_runtime_index(index, &indices)?;
                let mut target_ty = None;
                for position in indices {
                    let matches = eq(lowered_index.clone(), Expr::Const(position as u64));
                    let found = self.block_dynamic_targets(
                        element_ty,
                        &format!("{prefix}[{position}]"),
                        rest,
                        Some(and(hit.clone(), matches)),
                        out,
                    )?;
                    target_ty.get_or_insert(found);
                }
                target_ty
            }
        }
    }
}
