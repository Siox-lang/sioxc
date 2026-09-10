//! Statement, block-local, aggregate-write, and event-block lowering.

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
                        result = Expr::Select {
                            cond: Box::new(Expr::Binary {
                                op: BinOp::Eq,
                                lhs: Box::new(lowered_index.clone()),
                                rhs: Box::new(Expr::Const(position as u64)),
                            }),
                            then: Box::new(element(position)?),
                            els: Box::new(result),
                        };
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
                    return Some(Expr::Slice {
                        base: Box::new(Expr::Current(signal)),
                        hi: physical,
                        lo: physical,
                    });
                }
                let labels: Vec<i64> = positions.iter().map(|(label, _)| *label).collect();
                let (left, right) = self.persisted_range(path)?;
                let lowered_index =
                    self.checked_runtime_index_with_bounds(index, &labels, left, right)?;
                let mut result = Expr::Const(0);
                for (logical, physical) in positions.into_iter().rev() {
                    result = Expr::Select {
                        cond: Box::new(eq(lowered_index.clone(), Expr::Const(logical as u64))),
                        then: Box::new(Expr::Slice {
                            base: Box::new(Expr::Current(signal)),
                            hi: physical,
                            lo: physical,
                        }),
                        els: Box::new(result),
                    };
                }
                Some(result)
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
        let mut updates = Vec::new();
        for target in targets {
            match target {
                DynamicWriteTarget::Whole { signal, hit } => updates.push(NextUpdate {
                    span: self.cur_span,
                    target: signal,
                    cond: write_guard(cond, hit),
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
                    let meta = if self.out.array_element_enums.contains_key(&signal.0) {
                        let companion = SignalId(self.driven_companion(signal));
                        let meta_width = self.out.signals[companion.0 as usize].width;
                        self.arm_meta_temps(
                            self.cur_ctx,
                            self.out.signals[signal.0 as usize].declaration_span,
                        );
                        let meta_base =
                            self.slice_meta_write_base(signal, companion, sequential, pending);
                        self.flush_meta_temps();
                        let meta_value = Expr::Select {
                            cond: Box::new(Expr::Binary {
                                op: BinOp::Ge,
                                lhs: Box::new(expr.clone()),
                                rhs: Box::new(Expr::Const(2)),
                            }),
                            then: Box::new(expr.clone()),
                            els: Box::new(Expr::Const(0)),
                        };
                        Some(self.merge_slice(
                            meta_base,
                            position * 4 + 3,
                            position * 4,
                            meta_value,
                            meta_width,
                        ))
                    } else {
                        None
                    };
                    updates.push(NextUpdate {
                        span: self.cur_span,
                        target: signal,
                        cond: write_guard(cond, hit.clone()),
                        expr: self.merge_slice(base, position, position, expr.clone(), width),
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
    pub(super) fn dynamic_struct_write(
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
        // The elements must be structs; an array of scalars is the existing
        // expansion's business.
        self.local_struct
            .get(&format!("{base_path}[{}]", indices.first()?))?;
        let Val::Fields(fields) = self.lower_val_env(value, &HashMap::new()) else {
            return None;
        };
        let lowered_index = self.checked_runtime_index(index, &indices)?;
        let mut updates = Vec::new();
        for position in indices {
            let hit = eq(lowered_index.clone(), Expr::Const(position as u64));
            for (field, expr) in &fields {
                let Some(&signal) = self.locals.get(&format!("{base_path}[{position}].{field}"))
                else {
                    continue;
                };
                updates.push(NextUpdate {
                    span: self.cur_span,
                    target: signal,
                    cond: Some(and(cond.clone(), hit.clone())),
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
                .out
                .event_blocks
                .iter()
                .flat_map(|block| {
                    block
                        .updates
                        .iter()
                        .filter(|update| update.target == signal)
                        .map(move |update| {
                            let guard = match &update.cond {
                                Some(cond) => and_expr(block.condition.clone(), cond.clone()),
                                None => block.condition.clone(),
                            };
                            (guard, write_meta(&update.expr, &update.meta))
                        })
                })
                .fold(Expr::Current(companion), |acc, (guard, expr)| {
                    Expr::Select {
                        cond: Box::new(guard),
                        then: Box::new(expr),
                        els: Box::new(acc),
                    }
                });
            return pending
                .iter()
                .filter(|update| update.target == signal)
                .fold(seed, |acc, update| match &update.cond {
                    Some(cond) => Expr::Select {
                        cond: Box::new(cond.clone()),
                        then: Box::new(write_meta(&update.expr, &update.meta)),
                        els: Box::new(acc),
                    },
                    None => write_meta(&update.expr, &update.meta),
                });
        }
        let seed = self
            .resolved_neutral_planes(signal)
            .map(|(_, meta)| meta)
            .unwrap_or(Expr::Const(0));
        self.out
            .drivers
            .iter()
            .filter(|driver| driver.target == signal && driver.ctx == self.cur_ctx)
            .fold(seed, |acc, driver| match &driver.cond {
                Some(cond) => Expr::Select {
                    cond: Box::new(cond.clone()),
                    then: Box::new(write_meta(&driver.expr, &driver.meta)),
                    els: Box::new(acc),
                },
                None => write_meta(&driver.expr, &driver.meta),
            })
    }

    /// What `signal` already holds where this write appears — the base a
    /// read-modify-write must merge over.
    ///
    /// It is not enough to start from the signal's *prior* value. Each write
    /// produces a whole new value for the signal, and the backend keeps only
    /// the last one that fires: event-block updates are all staged from the
    /// pre-commit state and committed in order, and combinational drivers fold
    /// as `val = cond ? expr : val`. So a second partial write that merged over
    /// `Current(sig)` (or over nothing) silently threw the first one away —
    /// `word[1] = '1'; word[3] = '1';` set bit 3 alone. Folding the writes
    /// already lowered in this context gives each one the value its
    /// predecessors left behind, which is what the source says in both engines.
    pub(super) fn slice_write_base(
        &self,
        signal: SignalId,
        sequential: bool,
        pending: &[NextUpdate],
    ) -> Expr {
        if sequential {
            // A clocked block reads the pre-commit value, so an unwritten
            // signal keeps `Current`. Earlier *blocks* of the same driver
            // context count too: an impl may write one signal from several
            // events, and each of those blocks contributes only when its own
            // event fires. Their updates are staged from the same pre-commit
            // state, so folding them symbolically is exactly what the backend
            // computes.
            let seed = self
                .out
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
                                Some(cond) => and_expr(block.condition.clone(), cond.clone()),
                                None => block.condition.clone(),
                            };
                            (guard, update.expr.clone())
                        })
                })
                .fold(Expr::Current(signal), |acc, (guard, expr)| Expr::Select {
                    cond: Box::new(guard),
                    then: Box::new(expr),
                    els: Box::new(acc),
                });
            // Then this block's own updates: each expression is a complete
            // next value, so a later one supersedes exactly when it fires.
            return pending
                .iter()
                .filter(|update| update.target == signal)
                .fold(seed, |acc, update| match &update.cond {
                    Some(cond) => Expr::Select {
                        cond: Box::new(cond.clone()),
                        then: Box::new(update.expr.clone()),
                        els: Box::new(acc),
                    },
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
        self.out
            .drivers
            .iter()
            .filter(|driver| driver.target == signal && driver.ctx == self.cur_ctx)
            .fold(seed, |acc, driver| match &driver.cond {
                Some(cond) => Expr::Select {
                    cond: Box::new(cond.clone()),
                    then: Box::new(driver.expr.clone()),
                    els: Box::new(acc),
                },
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
        let value = repeat_element_plane(Expr::Const(encoding.value_bit(neutral)?), width, 1);
        let meta_disc = if encoding.binary.contains(&neutral) {
            0
        } else {
            neutral
        };
        let meta = repeat_element_plane(Expr::Const(meta_disc), width, 4);
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
                        let matches = eq(lowered_index.clone(), Expr::Const(position as u64));
                        self.dynamic_write_targets(
                            &format!("{path}[{position}]"),
                            rest,
                            Some(and(hit.clone(), matches)),
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
                        position,
                        hit: hit.unwrap_or(Expr::Const(1)),
                    });
                    return Some(());
                }
                let labels: Vec<i64> = positions.iter().map(|(label, _)| *label).collect();
                let (left, right) = self.persisted_range(path)?;
                let lowered_index =
                    self.checked_runtime_index_with_bounds(index, &labels, left, right)?;
                for (logical, position) in positions {
                    out.push(DynamicWriteTarget::PackedBit {
                        signal,
                        position,
                        hit: and(
                            hit.clone(),
                            eq(lowered_index.clone(), Expr::Const(logical as u64)),
                        ),
                    });
                }
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
        let kept = Expr::Binary {
            op: BinOp::And,
            lhs: Box::new(base),
            rhs: Box::new(words_const(keep)),
        };
        let masked = Expr::Binary {
            op: BinOp::And,
            lhs: Box::new(value),
            rhs: Box::new(words_const(ones(slice_w))),
        };
        let shifted = Expr::Binary {
            op: BinOp::Shl,
            lhs: Box::new(masked),
            rhs: Box::new(Expr::Const(lo as u64)),
        };
        Expr::Binary {
            op: BinOp::Or,
            lhs: Box::new(kept),
            rhs: Box::new(shifted),
        }
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
        let tpath = self
            .folded_elem_path(target)
            .or_else(|| expr_path(target))?;
        let struct_name = self.local_struct.get(&tpath).cloned()?;
        // The target's type decides how to read the braces: against a struct
        // `{ 6, 7 }` is a positional literal, not the bit concatenation it
        // lexes as. Without this the whole assignment produced no fields and
        // was dropped, leaving its leaves reported as never driven.
        let positional = self.positional_struct_args(&struct_name, value);
        let value = &match positional {
            Some(args) => ast::Expr::Construct {
                ty: None,
                args,
                spread: None,
                span: ast::expr_span(value),
            },
            None => value.clone(),
        };
        let mut out = Vec::new();
        if let Val::Fields(fields) = self.lower_val_env(value, &HashMap::new()) {
            for (fname, expr) in fields {
                if let Some(&sig) = self.locals.get(&format!("{tpath}.{fname}")) {
                    out.push((sig, self.coerce_to_target(sig, expr)));
                }
            }
        }
        Some(out)
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
        let tpath = self
            .folded_elem_path(target)
            .or_else(|| expr_path(target))?;
        let indices = self.local_array.get(&tpath).cloned()?;
        // A call stands for the expression it returns, as it does
        // combinationally.
        let reduced = self.returned_expr_from_call(value);
        let value = reduced.as_ref().unwrap_or(value);
        let leaf = |index: &i64| self.locals.get(&format!("{tpath}[{index}]")).copied();
        let mut out = Vec::new();
        match value {
            ast::Expr::Array { elems, .. } if elems.len() == indices.len() => {
                for (element, index) in elems.iter().zip(&indices) {
                    let signal = leaf(index)?;
                    out.push((
                        signal,
                        self.coerce_to_target(signal, self.lower_expr(element)),
                    ));
                }
            }
            // Another array signal: element for element, from its pre-commit
            // value like every other read in an event block.
            value
                if expr_path(value)
                    .and_then(|p| self.local_array.get(&p))
                    .is_some_and(|source| source.len() == indices.len()) =>
            {
                let source_path = expr_path(value)?;
                let source = self.local_array.get(&source_path)?;
                for (target_index, source_index) in indices.iter().zip(source) {
                    let signal = leaf(target_index)?;
                    let from = self
                        .locals
                        .get(&format!("{source_path}[{source_index}]"))
                        .copied()?;
                    out.push((signal, self.coerce_to_target(signal, Expr::Current(from))));
                }
            }
            // An element-wise expression over arrays (`g = a and b`).
            value => {
                for (position, index) in indices.iter().enumerate() {
                    let element = self.elementwise_at(value, position, indices.len())?;
                    let signal = leaf(index)?;
                    out.push((
                        signal,
                        self.coerce_to_target(signal, self.lower_expr(&element)),
                    ));
                }
            }
        }
        Some(out)
    }

    /// The signal an assignment target names, or `None` when it is not a simple
    /// place.
    pub(super) fn target_signal(&self, target: &ast::Expr) -> Option<SignalId> {
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
        for s in &b.stmts {
            match s {
                ast::Stmt::Assign { target, .. } => {
                    if let Some(id) = self.target_signal(target) {
                        out.insert(id.0);
                    }
                }
                ast::Stmt::If(inner) => out.extend(self.if_covered_targets(inner)),
                _ => {}
            }
        }
        out
    }
}
