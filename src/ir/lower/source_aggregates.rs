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
                let value = select_val(
                    condition,
                    self.lower_shaped_source(then, env, layout),
                    self.lower_shaped_source(els, env, layout),
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
                    self.lower_scalar_env(scrutinee, env),
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
                            Some(condition) => select_val(
                                self.bind_source_expression(condition, arm.span),
                                value,
                                result.unwrap_or(Val::Scalar(Expr::Unknown)),
                            ),
                        },
                    );
                    result = result.map(|value| self.bind_source_value(value, arm.span, None));
                }
                result.unwrap_or(Val::Scalar(Expr::Unknown))
            }
            (
                ast::Expr::Binary { .. } | ast::Expr::Unary { .. },
                LayoutKind::Array {
                    range: Some(range),
                    element,
                },
            ) => {
                // Element operator dispatch still owns a source-normalization
                // fragment. Keep its output in the same canonical value path;
                // never rebuild an aggregate returning call as caller syntax.
                let labels = loop_range(range.left, range.right);
                let mut fields = Vec::new();
                for (position, label) in labels.iter().enumerate() {
                    let Some(value) = self.elementwise_at(expression, position, labels.len())
                    else {
                        return Val::Scalar(Expr::Unknown);
                    };
                    Self::prefix_block_value(
                        &format!("[{label}]"),
                        self.lower_shaped_source(&value, env, element),
                        &mut fields,
                    );
                }
                self.bind_value_layout(
                    Val::Fields(fields),
                    layout.clone(),
                    ast::expr_span(expression),
                )
            }
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
        let env = HashMap::new();
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
                let value = values.get(&source)?.clone();
                Some((signal, self.coerce_to_target(signal, value)))
            })
            .collect()
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
