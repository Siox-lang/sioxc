//! Lexical source shapes and aggregate projections over canonical leaf values.

use super::*;

#[derive(Default)]
pub(super) struct SourceShapeFrame {
    pub(super) values: HashMap<String, SourceLayout>,
    pub(super) return_layout: Option<SourceLayout>,
    pub(super) function: bool,
}

pub(super) struct SourceShapeGuard<'a>(&'a std::cell::RefCell<Vec<SourceShapeFrame>>);

impl Drop for SourceShapeGuard<'_> {
    fn drop(&mut self) {
        self.0.borrow_mut().pop();
    }
}

impl Lowering<'_> {
    pub(super) fn source_shape_scope(
        &self,
        values: HashMap<String, SourceLayout>,
        return_layout: Option<SourceLayout>,
    ) -> SourceShapeGuard<'_> {
        self.source_shapes.borrow_mut().push(SourceShapeFrame {
            values,
            return_layout,
            function: true,
        });
        SourceShapeGuard(&self.source_shapes)
    }

    pub(super) fn source_lexical_scope(&self) -> SourceShapeGuard<'_> {
        self.source_shapes
            .borrow_mut()
            .push(SourceShapeFrame::default());
        SourceShapeGuard(&self.source_shapes)
    }

    pub(super) fn source_return_layout(&self) -> Option<SourceLayout> {
        self.source_shapes
            .borrow()
            .iter()
            .rev()
            .find(|frame| frame.function)
            .and_then(|frame| frame.return_layout.clone())
    }

    pub(super) fn bind_source_shape(&self, name: String, layout: SourceLayout) {
        if let Some(frame) = self.source_shapes.borrow_mut().last_mut() {
            frame.values.insert(name, layout);
        }
    }

    fn source_bound_layout(&self, name: &str) -> Option<SourceLayout> {
        for frame in self.source_shapes.borrow().iter().rev() {
            if let Some(layout) = frame.values.get(name) {
                return Some(layout.clone());
            }
            // A caller's same-spelled local is never a callee parameter.
            if frame.function {
                break;
            }
        }
        None
    }

    pub(super) fn source_function_return_layout(
        &self,
        function: &ast::FnDecl,
        shapes: &HashMap<String, SourceLayout>,
    ) -> Option<SourceLayout> {
        let ty = function.ret.as_ref()?;
        if let ast::Type::Path(path) = ty {
            if path.segments.len() == 1 {
                let name = &path.segments[0].text;
                if name == "Self" {
                    return shapes.get("self").cloned();
                }
                if function
                    .generics
                    .params
                    .iter()
                    .any(|parameter| parameter.name.text == *name)
                {
                    return function.params.iter().find_map(|parameter| {
                        (matches!(parameter.ty, Some(ast::Type::Path(_)))
                            && parameter.ty.as_ref().and_then(type_head_name)
                                == Some(name.as_str()))
                        .then(|| shapes.get(&parameter.name.as_ref()?.text).cloned())
                        .flatten()
                    });
                }
            }
        }
        Some(self.source_layout(ty, &self.cur_env))
    }

    /// Recover a concrete shape without copying an operand AST into its users.
    pub(super) fn source_operand_layout(
        &self,
        expression: &ast::Expr,
        env: &HashMap<String, Val>,
    ) -> Option<SourceLayout> {
        if let Some(layout) = self.source_call_layout(expression) {
            let root = access_steps(expression).map(|(root, _)| root);
            if root.as_ref().is_none_or(|root| !env.contains_key(root)) {
                return Some(layout);
            }
        }
        if let Some((root, steps)) = access_steps(expression) {
            if env.contains_key(&root) {
                let mut layout = self.source_bound_layout(&root)?;
                for step in steps {
                    layout = match (step, &layout.kind) {
                        (AccessStep::Field(name), LayoutKind::Struct { fields, .. }) => fields
                            .iter()
                            .find(|field| field.name == name)?
                            .layout
                            .clone(),
                        (AccessStep::Index(_), LayoutKind::Array { element, .. }) => {
                            (**element).clone()
                        }
                        (AccessStep::Index(index), LayoutKind::Packed { .. }) => {
                            self.source_packed_result_layout(&layout, index)?
                        }
                        _ => return None,
                    };
                }
                return Some(layout);
            }
        }
        if let Some(ty) = self.block_local_type(expression) {
            return Some(self.source_layout(&ty, &self.cur_env));
        }
        if let Some(layout) = self.persisted_operand_layout(expression) {
            return Some(layout);
        }
        match expression {
            ast::Expr::SysAttr { base, attr, .. } if attr.text == "old" => {
                self.source_operand_layout(base, env)
            }
            ast::Expr::Call { callee, args, .. } => {
                if let Some(argument) = self.generic_return_argument(callee, args) {
                    return self.source_operand_layout(argument, env);
                }
                if let ast::Expr::Field { base, field, .. } = callee.as_ref() {
                    let family = self.operand_type_name(base)?;
                    let function = self.find_method(&family, &field.text, None)?;
                    if function.ret.as_ref().and_then(type_head_name) == Some("Self") {
                        return self.source_operand_layout(base, env);
                    }
                    return function
                        .ret
                        .as_ref()
                        .map(|ty| self.source_layout(ty, &self.cur_env));
                }
                self.free_fns
                    .get(callee)?
                    .ret
                    .as_ref()
                    .map(|ty| self.source_layout(ty, &self.cur_env))
            }
            ast::Expr::Binary { lhs, .. } => self.source_operand_layout(lhs, env),
            ast::Expr::Unary { rhs, .. } => self.source_operand_layout(rhs, env),
            ast::Expr::IfExpr { then, els, .. } => self
                .source_operand_layout(then, env)
                .or_else(|| self.source_operand_layout(els, env)),
            _ => None,
        }
    }

    fn persisted_operand_layout(&self, expression: &ast::Expr) -> Option<SourceLayout> {
        let (root, steps) = access_steps(expression)?;
        let mut layout = self.persisted_layout(&root)?.clone();
        for step in steps {
            layout = match (step, &layout.kind) {
                (AccessStep::Field(name), LayoutKind::Struct { fields, .. }) => fields
                    .iter()
                    .find(|field| field.name == name)?
                    .layout
                    .clone(),
                (AccessStep::Index(_), LayoutKind::Array { element, .. }) => (**element).clone(),
                (AccessStep::Index(index), LayoutKind::Packed { .. }) => {
                    self.source_packed_result_layout(&layout, index)?
                }
                _ => return None,
            };
        }
        Some(layout)
    }

    pub(super) fn lower_source_argument(
        &self,
        expression: &ast::Expr,
        ty: Option<&ast::Type>,
        env: &HashMap<String, Val>,
    ) -> (Val, Option<SourceLayout>) {
        let layout = self
            .source_operand_layout(expression, env)
            .or_else(|| ty.map(|ty| self.source_layout(ty, &self.cur_env)));
        let value = match &layout {
            Some(layout)
                if matches!(
                    layout.kind,
                    LayoutKind::Array { .. } | LayoutKind::Struct { .. }
                ) =>
            {
                self.lower_shaped_source(expression, env, layout)
            }
            _ => self.lower_val_env(expression, env),
        };
        (value, layout)
    }

    pub(super) fn source_env_attribute(
        &self,
        base: &ast::Expr,
        attribute: &str,
        env: &HashMap<String, Val>,
    ) -> Option<Expr> {
        let (root, _) = access_steps(base)?;
        if !env.contains_key(&root) {
            return None;
        }
        let layout = self.source_operand_layout(base, env)?;
        if attribute == "length" {
            let length = match &layout.kind {
                LayoutKind::Array { range, .. } => range.and_then(LayoutRange::len),
                LayoutKind::Packed { width, .. } => Some(u64::from(*width)),
                _ => None,
            }?;
            return Some(Expr::Const(length));
        }
        let range = layout.index_range()?;
        let value = match attribute {
            "left" => range.left,
            "right" => range.right,
            "high" => range.left.max(range.right),
            "low" => range.left.min(range.right),
            "ascending" => i64::from(range.ascending()),
            _ => return None,
        };
        Some(if value < 0 {
            self.source_values.borrow_mut().unary(
                ProcessUnaryOp::Neg,
                &Expr::Const(value.unsigned_abs()),
                ast::expr_span(base),
            )
        } else {
            Expr::Const(value as u64)
        })
    }

    /// A constant slice bound, including one over a function receiver's
    /// shape (`self[self'low..self'high]`), which the source-level folder
    /// cannot see.
    fn slice_bound(&self, bound: &ast::Expr) -> Option<i64> {
        fn shape_arithmetic(expression: &ast::Expr) -> bool {
            match expression {
                ast::Expr::Int { .. } | ast::Expr::SysAttr { .. } => true,
                ast::Expr::Binary { lhs, rhs, .. } => {
                    shape_arithmetic(lhs) && shape_arithmetic(rhs)
                }
                _ => false,
            }
        }
        self.eval_const(bound, &self.cur_env).or_else(|| {
            shape_arithmetic(bound)
                .then(|| self.constant_expr(&self.lower_scalar_env(bound, &HashMap::new())))?
        })
    }

    pub(super) fn source_packed_slice_bounds(
        &self,
        layout: &SourceLayout,
        index: &ast::Expr,
    ) -> Option<(u32, u32)> {
        let range = layout.index_range()?;
        let (left, right) = match index {
            ast::Expr::Range { lo, hi, .. } => (self.slice_bound(lo)?, self.slice_bound(hi)?),
            ast::Expr::PartialRange { lo, hi, .. } => (
                lo.as_deref()
                    .map(|lo| self.eval_const(lo, &self.cur_env))
                    .unwrap_or(Some(range.left))?,
                hi.as_deref()
                    .map(|hi| self.eval_const(hi, &self.cur_env))
                    .unwrap_or(Some(range.right))?,
            ),
            _ => return None,
        };
        let low = range.left.min(range.right);
        let high = range.left.max(range.right);
        if left < low || left > high || right < low || right > high {
            return None;
        }
        Some((
            u32::try_from(i128::from(left) - i128::from(low)).ok()?,
            u32::try_from(i128::from(right) - i128::from(low)).ok()?,
        ))
    }

    pub(super) fn source_packed_result_layout(
        &self,
        layout: &SourceLayout,
        index: &ast::Expr,
    ) -> Option<SourceLayout> {
        let kind = if matches!(
            index,
            ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
        ) {
            let (left, right) = self.source_packed_slice_bounds(layout, index)?;
            let width = left.abs_diff(right).checked_add(1)?;
            let LayoutKind::Packed {
                family,
                element_enum,
                ..
            } = &layout.kind
            else {
                return None;
            };
            LayoutKind::Packed {
                width,
                family: family.clone(),
                range: Some(LayoutRange {
                    left: i64::from(width - 1),
                    right: 0,
                }),
                element_enum: element_enum.clone(),
            }
        } else {
            LayoutKind::Scalar {
                width: 1,
                domain: ScalarDomain::Bits,
                nominal: None,
                value_range: None,
            }
        };
        Some(SourceLayout {
            span: ast::expr_span(index),
            kind,
        })
    }

    /// The storage bit a constant, in-range index selects.
    fn constant_packed_index(
        &self,
        layout: &SourceLayout,
        index: &ast::Expr,
        env: &HashMap<String, Val>,
    ) -> Option<u32> {
        let range = layout.index_range()?;
        let low = range.left.min(range.right);
        let label = self.constant_expr(&self.lower_scalar_env(index, env))?;
        (low..=range.left.max(range.right))
            .contains(&label)
            .then(|| u32::try_from(label - low).ok())?
    }

    fn source_packed_access(
        &self,
        value: Expr,
        layout: &SourceLayout,
        index: &ast::Expr,
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let span = ast::expr_span(index);
        let result = if matches!(
            index,
            ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
        ) {
            let (left, right) = self.source_packed_slice_bounds(layout, index)?;
            if left >= right {
                self.source_slice(&value, left, right, span)
            } else {
                // Written ascending slices reverse significance, just like
                // the existing signal and block-local access paths.
                let mut result = Expr::Const(0);
                for physical in left..=right {
                    let bit = self.source_slice(&value, physical, physical, span);
                    let shifted = self.source_binary(
                        BinOp::Shl,
                        &bit,
                        &Expr::Const(u64::from(right - physical)),
                        span,
                    );
                    result = self.source_binary(BinOp::Or, &result, &shifted, span);
                }
                result
            }
        } else if let Some(bit) = self.constant_packed_index(layout, index, env) {
            // A constant index (`v[v'low + k]` in an unrolled loop) is one
            // bit; only a runtime one needs the checked mux.
            self.source_slice(&value, bit, bit, span)
        } else {
            let range = layout.index_range()?;
            let low = range.left.min(range.right);
            let labels = loop_range(range.left, range.right);
            let lowered = self.bind_source_expression(self.lower_scalar_env(index, env), span);
            let (&first, rest) = labels.split_first()?;
            let mut valid = self.source_binary(
                BinOp::Eq,
                &lowered,
                &self.source_index_label(first, span),
                span,
            );
            for &label in rest {
                let equal = self.source_binary(
                    BinOp::Eq,
                    &lowered,
                    &self.source_index_label(label, span),
                    span,
                );
                valid = self.source_binary(BinOp::Or, &valid, &equal, span);
            }
            let checked = self.source_values.borrow_mut().checked_index(
                &lowered,
                &valid,
                range.left,
                range.right,
                span,
            );
            let mut result = Expr::Const(0);
            for label in labels.into_iter().rev() {
                let physical = u32::try_from(i128::from(label) - i128::from(low)).ok()?;
                let condition = self.source_binary(
                    BinOp::Eq,
                    &checked,
                    &self.source_index_label(label, span),
                    span,
                );
                let selected = self.source_slice(&value, physical, physical, span);
                result = self.source_select(&condition, &selected, &result, span);
            }
            result
        };
        Some(self.bind_value_layout(
            Val::Scalar(result),
            self.source_packed_result_layout(layout, index)?,
            span,
        ))
    }

    /// Access an environment-bound aggregate with the same checked domain as
    /// a signal/local read. Aggregate selections retain all leaves, so a
    /// nested call can accept a runtime-selected array or struct whole.
    pub(super) fn source_env_access(
        &self,
        expression: &ast::Expr,
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let (root, _) = access_steps(expression)?;
        let value = env.get(&root)?.clone();
        self.source_access_value(expression, env, value, self.source_bound_layout(&root))
    }

    /// Runtime-selected signal aggregates have no scalar root. Project their
    /// existing leaf values exactly like argument aggregates, without copying
    /// a selected source expression once per destination leaf.
    pub(super) fn source_signal_aggregate_access(
        &self,
        expression: &ast::Expr,
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let (root, _) = access_steps(expression)?;
        if env.contains_key(&root) || self.block_local_named(&root).is_some() {
            return None;
        }
        let selected_layout = self.persisted_operand_layout(expression)?;
        if !matches!(
            selected_layout.kind,
            LayoutKind::Array { .. } | LayoutKind::Struct { .. }
        ) {
            return None;
        }
        let layout = self.persisted_layout(&root)?.clone();
        let value = self.aggregate_signal_val(&root)?;
        self.source_access_value(expression, env, value, Some(layout))
    }

    /// A selected local aggregate has no signal root. Use the same shape-aware
    /// projection as parameters instead of forcing it through scalar access.
    pub(super) fn source_block_aggregate_access(
        &self,
        expression: &ast::Expr,
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let (root, steps) = access_steps(expression)?;
        if steps.is_empty() || env.contains_key(&root) {
            return None;
        }
        let (_, binding) = self.block_local_named(&root)?;
        let layout = self.source_operand_layout(expression, env)?;
        if !matches!(
            layout.kind,
            LayoutKind::Array { .. } | LayoutKind::Struct { .. }
        ) {
            return None;
        }
        let layout = self.source_layout(&binding.ty, &self.cur_env);
        self.source_access_value(expression, env, binding.value, Some(layout))
    }

    pub(super) fn source_access_value(
        &self,
        expression: &ast::Expr,
        env: &HashMap<String, Val>,
        mut value: Val,
        mut layout: Option<SourceLayout>,
    ) -> Option<Val> {
        fn project(value: &Val, prefix: &str) -> Option<Val> {
            let Val::Fields(fields) = value else {
                return None;
            };
            if let Some((_, value)) = fields.iter().find(|(name, _)| name == prefix) {
                return Some(Val::Scalar(value.clone()));
            }
            let fields = fields
                .iter()
                .filter_map(|(name, value)| {
                    let suffix = name.strip_prefix(prefix)?;
                    let suffix = if let Some(suffix) = suffix.strip_prefix('.') {
                        suffix
                    } else if suffix.starts_with('[') {
                        suffix
                    } else {
                        return None;
                    };
                    Some((suffix.to_owned(), value.clone()))
                })
                .collect::<Vec<_>>();
            (!fields.is_empty()).then_some(Val::Fields(fields))
        }
        fn zero(value: &Val) -> Val {
            match value {
                Val::Scalar(_) => Val::Scalar(Expr::Const(0)),
                Val::Fields(fields) => Val::Fields(
                    fields
                        .iter()
                        .map(|(name, _)| (name.clone(), Expr::Const(0)))
                        .collect(),
                ),
            }
        }
        let (_, steps) = access_steps(expression)?;
        for step in steps {
            match step {
                AccessStep::Field(name) => {
                    value = project(&value, name)?;
                    layout = layout.as_ref().and_then(|layout| match &layout.kind {
                        LayoutKind::Struct { fields, .. } => fields
                            .iter()
                            .find(|field| field.name == name)
                            .map(|field| field.layout.clone()),
                        _ => None,
                    });
                }
                AccessStep::Index(index) => {
                    if let (Val::Scalar(scalar), Some(shape)) = (&value, &layout) {
                        if matches!(shape.kind, LayoutKind::Packed { .. }) {
                            value = self.source_packed_access(scalar.clone(), shape, index, env)?;
                            layout = Some(self.source_packed_result_layout(shape, index)?);
                            continue;
                        }
                    }
                    let LayoutKind::Array {
                        range: Some(range),
                        element,
                    } = &layout.as_ref()?.kind
                    else {
                        return None;
                    };
                    let indices = loop_range(range.left, range.right);
                    let lowered = self.bind_source_expression(
                        self.lower_scalar_env(index, env),
                        ast::expr_span(index),
                    );
                    let span = ast::expr_span(index);
                    let (&first, rest) = indices.split_first()?;
                    let mut valid = self.source_binary(
                        BinOp::Eq,
                        &lowered,
                        &self.source_index_label(first, span),
                        span,
                    );
                    for &position in rest {
                        let equal = self.source_binary(
                            BinOp::Eq,
                            &lowered,
                            &self.source_index_label(position, span),
                            span,
                        );
                        valid = self.source_binary(BinOp::Or, &valid, &equal, span);
                    }
                    let checked = self.source_values.borrow_mut().checked_index(
                        &lowered,
                        &valid,
                        range.left,
                        range.right,
                        span,
                    );
                    let mut selected = None;
                    for position in indices.into_iter().rev() {
                        let candidate = project(&value, &format!("[{position}]"))?;
                        let otherwise = selected.unwrap_or_else(|| zero(&candidate));
                        selected = Some(self.bind_source_value(
                            self.source_select_value(
                                self.source_binary(
                                    BinOp::Eq,
                                    &checked,
                                    &self.source_index_label(position, span),
                                    span,
                                ),
                                candidate,
                                otherwise,
                                ast::expr_span(expression),
                            ),
                            ast::expr_span(expression),
                            None,
                        ));
                    }
                    value = selected?;
                    layout = Some((**element).clone());
                }
            }
        }
        Some(value)
    }
}
