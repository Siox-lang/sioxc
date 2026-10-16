//! Contextual aggregate construction and canonical whole-value writes.

use super::*;

impl Lowering<'_> {
    /// Literal aggregates need a contextual layout, unlike ordinary scalar
    /// expressions. Flatten them in written order and retain each leaf's
    /// representation before an argument or return crosses its boundary.
    pub(super) fn lower_shaped_source(
        &self,
        expression: &ast::Expr,
        env: &HashMap<String, Val>,
        layout: &SourceLayout,
    ) -> Val {
        if let LayoutKind::Array {
            range: Some(range), ..
        } = &layout.kind
        {
            if let Some(values) = self
                .free_fns
                .constant_expr_key(expression)
                .and_then(|key| self.const_arrays.get(&key))
            {
                let fields = loop_range(range.left, range.right)
                    .into_iter()
                    .zip(values)
                    .map(|(index, value)| (format!("[{index}]"), value.clone()))
                    .collect();
                return self.bind_value_layout(
                    Val::Fields(fields),
                    layout.clone(),
                    ast::expr_span(expression),
                );
            }
        }
        match (expression, &layout.kind) {
            (ast::Expr::StrLit { .. } | ast::Expr::BitStrLit { .. }, LayoutKind::Packed { .. }) => {
                let value = self.bind_value_layout(
                    self.lower_val_env(expression, env),
                    layout.clone(),
                    ast::expr_span(expression),
                );
                if let (Val::Scalar(Expr::Canonical { value, .. }), Some(meta)) =
                    (&value, self.bit_string_meta(expression))
                {
                    self.source_values
                        .borrow_mut()
                        .set_explicit_meta(*value, meta);
                }
                value
            }
            (
                ast::Expr::Array { elems, .. },
                LayoutKind::Array {
                    range: Some(range),
                    element,
                },
            ) => {
                let mut fields = Vec::new();
                for (index, expression) in
                    loop_range(range.left, range.right).into_iter().zip(elems)
                {
                    Self::prefix_block_value(
                        &format!("[{index}]"),
                        self.lower_shaped_source(expression, env, element),
                        &mut fields,
                    );
                }
                self.bind_value_layout(
                    Val::Fields(fields),
                    layout.clone(),
                    ast::expr_span(expression),
                )
            }
            (
                ast::Expr::StrLit { text, .. },
                LayoutKind::Array {
                    range: Some(range),
                    element,
                },
            ) => {
                let mut fields = Vec::new();
                for (index, character) in loop_range(range.left, range.right)
                    .into_iter()
                    .zip(text.chars())
                {
                    let literal = ast::Expr::CharLit {
                        ch: character,
                        span: ast::expr_span(expression),
                    };
                    let value = self.lower_shaped_source(&literal, env, element);
                    Self::prefix_block_value(&format!("[{index}]"), value, &mut fields);
                }
                self.bind_value_layout(
                    Val::Fields(fields),
                    layout.clone(),
                    ast::expr_span(expression),
                )
            }
            (ast::Expr::Concat { parts, .. }, LayoutKind::Struct { fields, .. }) => {
                let mut values = Vec::new();
                for (field, value) in fields.iter().zip(parts) {
                    Self::prefix_block_value(
                        &field.name,
                        self.lower_shaped_source(value, env, &field.layout),
                        &mut values,
                    );
                }
                self.bind_value_layout(
                    Val::Fields(values),
                    layout.clone(),
                    ast::expr_span(expression),
                )
            }
            (ast::Expr::Construct { args, spread, .. }, LayoutKind::Struct { fields, .. }) => {
                let mut values = match spread {
                    Some(base) => match self.lower_shaped_source(base, env, layout) {
                        Val::Fields(fields) => fields,
                        Val::Scalar(_) => Vec::new(),
                    },
                    None => Vec::new(),
                };
                for (position, argument) in args.iter().enumerate() {
                    let field = match &argument.field {
                        Some(name) => fields.iter().find(|field| field.name == name.text),
                        None => fields.get(position),
                    };
                    let (Some(field), Some(value)) = (field, &argument.value) else {
                        continue;
                    };
                    // An override replaces the complete field subtree, not
                    // just a same-spelled scalar or the first duplicate leaf.
                    values.retain(|(name, _)| {
                        name != &field.name
                            && !name.starts_with(&format!("{}.", field.name))
                            && !name.starts_with(&format!("{}[", field.name))
                    });
                    Self::prefix_block_value(
                        &field.name,
                        self.lower_shaped_source(value, env, &field.layout),
                        &mut values,
                    );
                }
                self.bind_value_layout(
                    Val::Fields(values),
                    layout.clone(),
                    ast::expr_span(expression),
                )
            }
            (
                ast::Expr::IfExpr {
                    cond, then, els, ..
                },
                LayoutKind::Array { .. } | LayoutKind::Struct { .. },
            ) => {
                let condition = self
                    .bind_source_expression(self.lower_scalar_env(cond, env), ast::expr_span(cond));
                let value = self.source_select_value(
                    condition,
                    self.lower_shaped_source(then, env, layout),
                    self.lower_shaped_source(els, env, layout),
                    ast::expr_span(expression),
                );
                self.bind_source_value(value, ast::expr_span(expression), None)
            }
            (
                ast::Expr::Match {
                    scrutinee, arms, ..
                },
                LayoutKind::Array { .. } | LayoutKind::Struct { .. },
            ) => {
                let scrutinee_value = self.bind_source_expression(
                    if self.is_tuple_scrutinee(scrutinee) {
                        Expr::Const(0)
                    } else {
                        self.lower_scalar_env(scrutinee, env)
                    },
                    ast::expr_span(scrutinee),
                );
                let exhaustive = !arms.iter().any(|arm| pattern_has_wildcard(&arm.pattern));
                let mut result = None;
                for (position, arm) in arms.iter().enumerate().rev() {
                    let value = arm
                        .value_expr()
                        .map(|value| self.lower_shaped_source(value, env, layout))
                        .unwrap_or(Val::Scalar(Expr::Unknown));
                    result = Some(
                        match self.arm_match_cond(&arm.pattern, scrutinee, &scrutinee_value, env) {
                            None => value,
                            Some(_) if exhaustive && position + 1 == arms.len() => value,
                            Some(condition) => self.source_select_value(
                                self.bind_source_expression(condition, arm.span),
                                value,
                                result.unwrap_or(Val::Scalar(Expr::Unknown)),
                                arm.span,
                            ),
                        },
                    );
                    result = result.map(|value| self.bind_source_value(value, arm.span, None));
                }
                result.unwrap_or(Val::Scalar(Expr::Unknown))
            }
            (
                ast::Expr::Binary { .. } | ast::Expr::Unary { .. },
                LayoutKind::Array { range: Some(_), .. },
            ) => self.lower_source_array_operator(expression, env, layout),
            _ => {
                let value = self.lower_val_env(expression, env);
                if matches!(
                    layout.kind,
                    LayoutKind::Array { .. } | LayoutKind::Struct { .. }
                ) {
                    let value = match self.source_operand_layout(expression, env) {
                        Some(source) => reindex_aggregate_value(value, &source, layout)
                            .unwrap_or(Val::Scalar(Expr::Unknown)),
                        None => value,
                    };
                    self.bind_value_layout(value, layout.clone(), ast::expr_span(expression))
                } else {
                    // Keep general scalar call arguments' existing evaluation
                    // format. Only contextual literal leaves require coercion.
                    self.bind_source_value(value, ast::expr_span(expression), None)
                }
            }
        }
    }

    /// A whole aggregate store maps written source positions onto written
    /// target positions recursively. Fields retain names; arrays need not
    /// share labels or direction. No caller AST is substituted into a return.
    pub(super) fn aggregate_assign_leaves(
        &self,
        target: &ast::Expr,
        value: &ast::Expr,
    ) -> Option<Vec<(SignalId, Expr)>> {
        let path = self
            .folded_elem_path(target)
            .or_else(|| expr_path(target))?;
        let target_layout = self.persisted_layout(&path)?;
        if !matches!(
            target_layout.kind,
            LayoutKind::Array { .. } | LayoutKind::Struct { .. }
        ) {
            return None;
        }
        // `{ hi, lo }` into an element-wise bit array: the parts' elements,
        // in order, fill the labels from `'left`, as VHDL's `&` does.
        if let (
            ast::Expr::Concat { parts, .. },
            LayoutKind::Array {
                range: Some(range),
                element,
            },
        ) = (value, &target_layout.kind)
        {
            if !matches!(element.kind, LayoutKind::Scalar { .. }) {
                return None;
            }
            let mut elements = Vec::new();
            for part in parts {
                elements.extend(self.concat_part_elements(part)?);
            }
            let labels = loop_range(range.left, range.right);
            if elements.len() != labels.len() {
                return None;
            }
            return labels
                .into_iter()
                .zip(elements)
                .map(|(label, element)| {
                    let signal = *self.locals.get(&format!("{path}[{label}]"))?;
                    Some((signal, self.coerce_to_target(signal, element)))
                })
                .collect();
        }
        let env = HashMap::new();
        let value_expression = value;
        let source_layout = self
            .source_operand_layout(value, &env)
            .unwrap_or_else(|| target_layout.clone());
        let value = self.lower_shaped_source(value, &env, &source_layout);
        let Val::Fields(fields) = value else {
            return None;
        };
        let pairs = aggregate_leaf_pairs(&source_layout, target_layout)?;
        let values = fields.into_iter().collect::<HashMap<_, _>>();
        pairs
            .into_iter()
            .map(|(source, destination)| {
                let suffix = if destination.starts_with('[') {
                    destination
                } else {
                    format!(".{destination}")
                };
                let signal = *self.locals.get(&format!("{path}{suffix}"))?;
                // A field the literal leaves out (`{ .x = 3 }`, warned as
                // W-P016) takes its default, the signal's initial value.
                let value = match values.get(&source) {
                    Some(value) => value.clone(),
                    None if matches!(value_expression, ast::Expr::Construct { .. }) => {
                        match self.out.signals[signal.0 as usize].init.as_slice() {
                            [] => Expr::Const(0),
                            [word] => Expr::Const(*word),
                            words => Expr::WideConst(words.to_vec()),
                        }
                    }
                    None => return None,
                };
                Some((signal, self.coerce_to_target(signal, value)))
            })
            .collect()
    }
}

impl Lowering<'_> {
    /// One concatenation part's scalar elements in order: a slice of an
    /// element-wise array as written, a whole one from `'left`, or one bit.
    fn concat_part_elements(&self, part: &ast::Expr) -> Option<Vec<Expr>> {
        let element = |path: &str, label: i64| {
            self.locals
                .get(&format!("{path}[{label}]"))
                .map(|signal| Expr::Current(*signal))
        };
        let array_path = |base: &ast::Expr| {
            let path = expr_path(base)?;
            matches!(self.persisted_layout(&path)?.kind, LayoutKind::Array { .. }).then_some(path)
        };
        if let ast::Expr::Index { base, index, .. } = part {
            if matches!(
                index.as_ref(),
                ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
            ) {
                let path = array_path(base)?;
                let (left, right) = self.slice_bounds(base, index)?;
                return loop_range(left, right)
                    .into_iter()
                    .map(|label| element(&path, label))
                    .collect();
            }
        }
        if let Some(path) = array_path(part) {
            let (left, right) = self.persisted_range(&path)?;
            return loop_range(left, right)
                .into_iter()
                .map(|label| element(&path, label))
                .collect();
        }
        Some(vec![self.lower_expr(part)])
    }

    /// `(q, r) = e;`: each named position of the target takes its part of the
    /// value, leaf by leaf; `_` takes nothing. A position that is a block
    /// local is stored at once (under `cond`); the signal leaves are returned
    /// for the caller to drive. `None` when a position is no place or the
    /// value has no tuple shape here.
    pub(super) fn tuple_assign_leaves(
        &self,
        target: &ast::Expr,
        value: &ast::Expr,
        cond: &Option<Expr>,
    ) -> Option<Vec<(SignalId, Expr)>> {
        let env = HashMap::new();
        let source_layout = self
            .source_operand_layout(value, &env)
            .or_else(|| self.tuple_target_layout(target))?;
        let Val::Fields(fields) = self.lower_shaped_source(value, &env, &source_layout) else {
            return None;
        };
        let mut leaves = Vec::new();
        self.tuple_target_leaves(target, &source_layout, "", &fields, cond, &mut leaves)?;
        Some(leaves)
    }

    /// The layout a tuple target stands for, built from its positions; the
    /// shape a tuple literal on the right is read through.
    fn tuple_target_layout(&self, target: &ast::Expr) -> Option<SourceLayout> {
        let ast::Expr::Tuple { elems, span } = target else {
            if let Some((scope_index, name, suffix)) = self.block_local_path(target) {
                if suffix.is_empty() {
                    let ty = self
                        .block_scopes
                        .borrow()
                        .get(scope_index)?
                        .get(&name)?
                        .ty
                        .clone();
                    return Some(self.source_layout(&ty, &self.cur_env));
                }
            }
            let path = self
                .folded_elem_path(target)
                .or_else(|| expr_path(target))?;
            return self.persisted_layout(&path).cloned();
        };
        Some(SourceLayout {
            span: *span,
            kind: LayoutKind::Struct {
                name: crate::types::tuple_struct_name(elems.len()),
                view: None,
                fields: elems
                    .iter()
                    .enumerate()
                    .map(|(index, elem)| {
                        Some(crate::ir::LayoutField {
                            name: index.to_string(),
                            direction: None,
                            layout: self.tuple_target_layout(elem)?,
                        })
                    })
                    .collect::<Option<Vec<_>>>()?,
            },
        })
    }

    /// Pair each leaf of a tuple target with its value, `source` being the
    /// layout of the part of the value at `prefix`.
    fn tuple_target_leaves(
        &self,
        target: &ast::Expr,
        source: &SourceLayout,
        prefix: &str,
        values: &[(String, Expr)],
        cond: &Option<Expr>,
        out: &mut Vec<(SignalId, Expr)>,
    ) -> Option<()> {
        match target {
            ast::Expr::Tuple { elems, .. } => {
                let LayoutKind::Struct { fields, .. } = &source.kind else {
                    return None;
                };
                for (index, elem) in elems.iter().enumerate() {
                    let name = index.to_string();
                    let field = fields.iter().find(|field| field.name == name)?;
                    self.tuple_target_leaves(
                        elem,
                        &field.layout,
                        &join_leaf(prefix, &name),
                        values,
                        cond,
                        out,
                    )?;
                }
            }
            ast::Expr::Path(path) if matches!(path.segments.as_slice(), [only] if only.text == "_") =>
                {}
            place => {
                if let Some((scope_index, name, suffix)) = self.block_local_path(place) {
                    if suffix.is_empty() {
                        return self.store_tuple_part(
                            scope_index,
                            name,
                            place,
                            prefix,
                            values,
                            cond,
                        );
                    }
                }
                let path = self.folded_elem_path(place).or_else(|| expr_path(place))?;
                let layout = self.persisted_layout(&path)?;
                let values = values.iter().cloned().collect::<HashMap<_, _>>();
                for (from, to) in aggregate_leaf_pairs(source, layout)? {
                    let signal = *self.locals.get(&join_leaf(&path, &to))?;
                    let value = values.get(&join_leaf(prefix, &from))?.clone();
                    out.push((signal, self.coerce_to_target(signal, value)));
                }
            }
        }
        Some(())
    }

    /// Store the part of a tuple value at `prefix` into a block local.
    fn store_tuple_part(
        &self,
        scope_index: usize,
        name: String,
        place: &ast::Expr,
        prefix: &str,
        values: &[(String, Expr)],
        cond: &Option<Expr>,
    ) -> Option<()> {
        let previous = self
            .block_scopes
            .borrow()
            .get(scope_index)?
            .get(&name)?
            .clone();
        let part = match values.iter().find(|(path, _)| path == prefix) {
            Some((_, value)) => Val::Scalar(value.clone()),
            None => Val::Fields(
                values
                    .iter()
                    .filter_map(|(path, value)| {
                        let rest = path.strip_prefix(prefix)?;
                        let rest = match rest.strip_prefix('.') {
                            Some(field) => field,
                            None if rest.starts_with('[') => rest,
                            None => return None,
                        };
                        Some((rest.to_string(), value.clone()))
                    })
                    .collect(),
            ),
        };
        let span = ast::expr_span(place);
        let next = match cond {
            Some(condition) => {
                self.source_select_value(condition.clone(), part, previous.value, span)
            }
            None => part,
        };
        self.store_block_local(scope_index, name, next, previous.ty, span);
        Some(())
    }
}

/// `prefix` and `rest` as one leaf path: `0.a`, `0[1]`.
fn join_leaf(prefix: &str, rest: &str) -> String {
    match (prefix.is_empty(), rest.is_empty()) {
        (true, _) => rest.to_string(),
        (false, true) => prefix.to_string(),
        (false, false) if rest.starts_with('[') => format!("{prefix}{rest}"),
        (false, false) => format!("{prefix}.{rest}"),
    }
}

fn reindex_aggregate_value(
    value: Val,
    source: &SourceLayout,
    target: &SourceLayout,
) -> Option<Val> {
    let Val::Fields(fields) = value else {
        return None;
    };
    let values = fields.into_iter().collect::<HashMap<_, _>>();
    let fields = aggregate_leaf_pairs(source, target)?
        .into_iter()
        .map(|(from, to)| Some((to, values.get(&from)?.clone())))
        .collect::<Option<Vec<_>>>()?;
    Some(Val::Fields(fields))
}

fn aggregate_leaf_pairs(
    source: &SourceLayout,
    target: &SourceLayout,
) -> Option<Vec<(String, String)>> {
    fn field_path(prefix: &str, field: &str) -> String {
        if prefix.is_empty() {
            field.to_owned()
        } else {
            format!("{prefix}.{field}")
        }
    }
    fn walk(
        source: &SourceLayout,
        target: &SourceLayout,
        from: String,
        to: String,
        pairs: &mut Vec<(String, String)>,
    ) -> Option<()> {
        match (&source.kind, &target.kind) {
            (
                LayoutKind::Array {
                    range: Some(source_range),
                    element: source_element,
                },
                LayoutKind::Array {
                    range: Some(target_range),
                    element: target_element,
                },
            ) => {
                if source_range.len()? != target_range.len()? {
                    return None;
                }
                for (source_index, target_index) in
                    loop_range(source_range.left, source_range.right)
                        .into_iter()
                        .zip(loop_range(target_range.left, target_range.right))
                {
                    walk(
                        source_element,
                        target_element,
                        format!("{from}[{source_index}]"),
                        format!("{to}[{target_index}]"),
                        pairs,
                    )?;
                }
            }
            (
                LayoutKind::Struct {
                    fields: source_fields,
                    ..
                },
                LayoutKind::Struct {
                    fields: target_fields,
                    ..
                },
            ) => {
                for target_field in target_fields {
                    let source_field = source_fields
                        .iter()
                        .find(|field| field.name == target_field.name)?;
                    walk(
                        &source_field.layout,
                        &target_field.layout,
                        field_path(&from, &source_field.name),
                        field_path(&to, &target_field.name),
                        pairs,
                    )?;
                }
            }
            (
                LayoutKind::Scalar { .. } | LayoutKind::Packed { .. },
                LayoutKind::Scalar { .. } | LayoutKind::Packed { .. },
            ) => pairs.push((from, to)),
            _ => return None,
        }
        Some(())
    }
    let mut pairs = Vec::new();
    walk(source, target, String::new(), String::new(), &mut pairs)?;
    Some(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span() -> crate::diag::Span {
        crate::diag::Span::new(crate::diag::FileId(0), 0..1)
    }

    fn scalar() -> SourceLayout {
        SourceLayout {
            span: span(),
            kind: LayoutKind::Scalar {
                width: 8,
                domain: ScalarDomain::Bits,
                nominal: None,
                value_range: None,
            },
        }
    }

    fn array(element: SourceLayout, left: i64, right: i64) -> SourceLayout {
        SourceLayout {
            span: span(),
            kind: LayoutKind::Array {
                range: Some(LayoutRange { left, right }),
                element: Box::new(element),
            },
        }
    }

    #[test]
    fn nested_reindexing_keeps_written_position_order_and_canonical_ids() {
        let source = array(array(scalar(), -1, 0), 3, 2);
        let target = array(array(scalar(), 7, 6), 4, 5);
        let pairs = aggregate_leaf_pairs(&source, &target).unwrap();
        assert_eq!(
            pairs,
            vec![
                ("[3][-1]".to_owned(), "[4][7]".to_owned()),
                ("[3][0]".to_owned(), "[4][6]".to_owned()),
                ("[2][-1]".to_owned(), "[5][7]".to_owned()),
                ("[2][0]".to_owned(), "[5][6]".to_owned()),
            ]
        );
        let value = Val::Fields(
            pairs
                .iter()
                .enumerate()
                .map(|(index, (name, _))| {
                    (
                        name.clone(),
                        Expr::Canonical {
                            value: ProcessValueId(index as u32),
                            reads: std::sync::Arc::from([]),
                        },
                    )
                })
                .collect(),
        );
        let Val::Fields(fields) = reindex_aggregate_value(value, &source, &target).unwrap() else {
            panic!("aggregate expected");
        };
        for (index, (name, value)) in fields.into_iter().enumerate() {
            assert_eq!(name, pairs[index].1);
            assert!(
                matches!(value, Expr::Canonical { value, .. } if value == ProcessValueId(index as u32))
            );
        }
    }

    #[test]
    fn reindexing_does_not_truncate_a_mismatched_shape_or_missing_leaf() {
        let source = array(scalar(), 0, 1);
        let target = array(scalar(), 4, 6);
        assert!(aggregate_leaf_pairs(&source, &target).is_none());
        assert!(reindex_aggregate_value(
            Val::Fields(vec![("[0]".into(), Expr::Const(1))]),
            &source,
            &source
        )
        .is_none());
    }
}
