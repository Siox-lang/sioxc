//! Captured storage identities, aggregate projections and hardware place writes.

use super::source_calls::Binding;
use super::*;

#[derive(Clone)]
enum Storage {
    Signal(SignalId),
    Local {
        scope: usize,
        name: String,
        field: String,
    },
}

#[derive(Clone)]
struct Access {
    storage: Storage,
    guard: Option<Expr>,
    /// Result bits, low first, mapped onto the underlying storage leaf.
    bits: Option<Vec<u32>>,
}

#[derive(Clone)]
pub(super) struct Place {
    pub(super) layout: SourceLayout,
    leaves: Vec<(String, Vec<Access>)>,
}

impl Place {
    fn project_field(&mut self, prefix: &str, layout: SourceLayout) -> Option<()> {
        let leaves = self
            .leaves
            .iter()
            .filter_map(|(name, accesses)| {
                let suffix = name.strip_prefix(prefix)?;
                let suffix = if suffix.is_empty() {
                    suffix
                } else if let Some(suffix) = suffix.strip_prefix('.') {
                    suffix
                } else if suffix.starts_with('[') {
                    suffix
                } else {
                    return None;
                };
                Some((suffix.to_owned(), accesses.clone()))
            })
            .collect::<Vec<_>>();
        if leaves.is_empty() {
            return None;
        }
        self.leaves = leaves;
        self.layout = layout;
        Some(())
    }
}

impl Lowering<'_> {
    pub(super) fn source_call_signal(&self, expression: &ast::Expr) -> Option<Option<SignalId>> {
        let (root, _) = access_steps(expression)?;
        self.call_argument(&root)?;
        let signal = self.source_place(expression).and_then(|place| {
            let [(_, accesses)] = place.leaves.as_slice() else {
                return None;
            };
            let [access] = accesses.as_slice() else {
                return None;
            };
            if access.guard.is_some() || access.bits.is_some() {
                return None;
            }
            match access.storage {
                Storage::Signal(signal) => Some(signal),
                Storage::Local { .. } => None,
            }
        });
        Some(signal)
    }

    pub(super) fn source_call_covered_targets(
        &self,
        expression: &ast::Expr,
    ) -> Option<std::collections::BTreeSet<u32>> {
        let (root, _) = access_steps(expression)?;
        self.call_argument(&root)?;
        let mut covered = std::collections::BTreeSet::new();
        if let Some(place) = self.source_place(expression) {
            for (_, accesses) in place.leaves {
                for access in accesses {
                    if access.guard.is_none() && access.bits.is_none() {
                        if let Storage::Signal(signal) = access.storage {
                            covered.insert(signal.0);
                        }
                    }
                }
            }
        }
        Some(covered)
    }

    fn storage_value(&self, storage: &Storage) -> Option<Expr> {
        match storage {
            Storage::Signal(signal) => Some(Expr::Current(*signal)),
            Storage::Local { scope, name, field } => {
                let binding = self.block_scopes.borrow().get(*scope)?.get(name)?.clone();
                match binding.value {
                    Val::Scalar(value) if field.is_empty() => Some(value),
                    Val::Fields(fields) => fields
                        .into_iter()
                        .find(|(name, _)| name == field)
                        .map(|(_, value)| value),
                    _ => None,
                }
            }
        }
    }

    pub(super) fn read_place(&self, place: &Place, span: crate::diag::Span) -> Val {
        let fields = place
            .leaves
            .iter()
            .map(|(name, accesses)| {
                let mut result = Expr::Const(0);
                for access in accesses.iter().rev() {
                    let mut value = self.storage_value(&access.storage).unwrap_or(Expr::Unknown);
                    if let Some(bits) = &access.bits {
                        if let (Some(&low), Some(&high)) = (bits.first(), bits.last()) {
                            if bits.iter().enumerate().all(|(position, &bit)| {
                                u64::from(bit) == u64::from(low) + position as u64
                            }) {
                                value = Expr::Slice {
                                    base: Box::new(value),
                                    hi: high,
                                    lo: low,
                                };
                            } else {
                                let mut packed = Expr::Const(0);
                                for (position, &bit) in bits.iter().enumerate() {
                                    let part = Expr::Slice {
                                        base: Box::new(value.clone()),
                                        hi: bit,
                                        lo: bit,
                                    };
                                    let part = Expr::Binary {
                                        op: BinOp::Shl,
                                        lhs: Box::new(part),
                                        rhs: Box::new(Expr::Const(position as u64)),
                                    };
                                    packed =
                                        self.bind_source_expression(or_expr(packed, part), span);
                                }
                                value = packed;
                            }
                        }
                    }
                    result = match &access.guard {
                        Some(guard) => Expr::Select {
                            cond: Box::new(guard.clone()),
                            then: Box::new(value),
                            els: Box::new(result),
                        },
                        None => value,
                    };
                }
                (name.clone(), self.bind_source_expression(result, span))
            })
            .collect::<Vec<_>>();
        if fields.len() == 1 && fields[0].0.is_empty() {
            Val::Scalar(fields[0].1.clone())
        } else {
            Val::Fields(fields)
        }
    }

    pub(super) fn source_place(&self, expression: &ast::Expr) -> Option<Place> {
        let (root, _) = access_steps(expression)?;
        if let Some(argument) = self.call_argument(&root) {
            return match &argument.binding {
                Binding::Place(place) => self.project_place(place.clone(), expression),
                Binding::Value(_) => None,
            };
        }
        let (layout, fields, local) = if let Some((scope, binding)) = self.block_local_named(&root)
        {
            let fields = match &binding.value {
                Val::Scalar(_) => vec![String::new()],
                Val::Fields(fields) => fields.iter().map(|(name, _)| name.clone()).collect(),
            };
            (
                self.source_layout(&binding.ty, &self.cur_env),
                fields,
                Some(scope),
            )
        } else {
            let layout = self.persisted_layout(&root)?.clone();
            let fields = match self.aggregate_signal_val(&root) {
                Some(Val::Fields(fields)) => fields.into_iter().map(|(name, _)| name).collect(),
                _ => vec![String::new()],
            };
            (layout, fields, None)
        };
        let leaves = fields
            .into_iter()
            .map(|field| {
                let storage = match local {
                    Some(scope) => Storage::Local {
                        scope,
                        name: root.clone(),
                        field: field.clone(),
                    },
                    None => {
                        let separator = if field.is_empty() || field.starts_with('[') {
                            ""
                        } else {
                            "."
                        };
                        Storage::Signal(*self.locals.get(&format!("{root}{separator}{field}"))?)
                    }
                };
                Some((
                    field,
                    vec![Access {
                        storage,
                        guard: None,
                        bits: None,
                    }],
                ))
            })
            .collect::<Option<Vec<_>>>()?;
        self.project_place(Place { layout, leaves }, expression)
    }

    pub(super) fn project_place(&self, mut place: Place, expression: &ast::Expr) -> Option<Place> {
        let (_, steps) = access_steps(expression)?;
        for step in steps {
            match step {
                AccessStep::Field(name) => {
                    let LayoutKind::Struct { fields, .. } = &place.layout.kind else {
                        return None;
                    };
                    let layout = fields
                        .iter()
                        .find(|field| field.name == name)?
                        .layout
                        .clone();
                    place.project_field(name, layout)?;
                }
                AccessStep::Index(index) => match &place.layout.kind {
                    LayoutKind::Array {
                        range: Some(range),
                        element,
                    } => {
                        let (range, layout) = (*range, (**element).clone());
                        let labels = loop_range(range.left, range.right);
                        if let Some(label) = self.eval_const(index, &self.cur_env) {
                            if !labels.contains(&label) {
                                return None;
                            }
                            place.project_field(&format!("[{label}]"), layout)?;
                        } else {
                            let checked = self.checked_runtime_index_with_bounds(
                                index,
                                &labels,
                                range.left,
                                range.right,
                            )?;
                            let mut fields = Vec::<(String, Vec<Access>)>::new();
                            for label in labels {
                                let mut selected = place.clone();
                                selected.project_field(&format!("[{label}]"), layout.clone())?;
                                let hit = self.bind_source_expression(
                                    eq(checked.clone(), index_label(label)),
                                    ast::expr_span(index),
                                );
                                for (name, mut accesses) in selected.leaves {
                                    for access in &mut accesses {
                                        access.guard = Some(self.bind_source_expression(
                                            and(access.guard.clone(), hit.clone()),
                                            ast::expr_span(index),
                                        ));
                                    }
                                    if let Some((_, previous)) =
                                        fields.iter_mut().find(|(field, _)| field == &name)
                                    {
                                        previous.extend(accesses);
                                    } else {
                                        fields.push((name, accesses));
                                    }
                                }
                            }
                            place = Place {
                                layout,
                                leaves: fields,
                            };
                        }
                    }
                    LayoutKind::Packed { width, range, .. } => {
                        let range = range.unwrap_or(LayoutRange {
                            left: i64::from(*width) - 1,
                            right: 0,
                        });
                        let layout = self.source_packed_result_layout(&place.layout, index)?;
                        let mut projections = Vec::<(Vec<u32>, Option<Expr>)>::new();
                        if matches!(
                            index,
                            ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
                        ) {
                            let (left, right) =
                                self.source_packed_slice_bounds(&place.layout, index)?;
                            let bits = if left >= right {
                                (right..=left).collect()
                            } else {
                                (left..=right).rev().collect()
                            };
                            projections.push((bits, None));
                        } else if let Some(label) = self.eval_const(index, &self.cur_env) {
                            if label < range.left.min(range.right)
                                || label > range.left.max(range.right)
                            {
                                return None;
                            }
                            projections.push((
                                vec![u32::try_from(label - range.left.min(range.right)).ok()?],
                                None,
                            ));
                        } else {
                            let labels = loop_range(range.left, range.right);
                            let checked = self.checked_runtime_index_with_bounds(
                                index,
                                &labels,
                                range.left,
                                range.right,
                            )?;
                            for label in labels {
                                projections.push((
                                    vec![u32::try_from(label - range.left.min(range.right)).ok()?],
                                    Some(self.bind_source_expression(
                                        eq(checked.clone(), index_label(label)),
                                        ast::expr_span(index),
                                    )),
                                ));
                            }
                        }
                        for (_, accesses) in &mut place.leaves {
                            let mut projected = Vec::new();
                            for access in accesses.iter() {
                                for (bits, guard) in &projections {
                                    let mut access = access.clone();
                                    access.bits = Some(
                                        bits.iter()
                                            .map(|&bit| {
                                                access.bits.as_ref().map_or(Some(bit), |mapping| {
                                                    mapping.get(bit as usize).copied()
                                                })
                                            })
                                            .collect::<Option<Vec<_>>>()?,
                                    );
                                    access.guard = match guard {
                                        Some(guard) => Some(and(access.guard, guard.clone())),
                                        None => access.guard,
                                    };
                                    projected.push(access);
                                }
                            }
                            *accesses = projected;
                        }
                        place.layout = layout;
                    }
                    _ => return None,
                },
            }
        }
        Some(place)
    }

    pub(super) fn assign_source_call_place(
        &mut self,
        target: &ast::Expr,
        value: &ast::Expr,
        cond: &Option<Expr>,
        sequential: bool,
        pending: &[NextUpdate],
    ) -> Option<Vec<NextUpdate>> {
        let (root, _) = access_steps(target)?;
        self.call_argument(&root)?;
        let Some(place) = self.source_place(target) else {
            self.report_bad_assign_target(target);
            return Some(Vec::new());
        };
        let source = value;
        let value = self.lower_shaped_source(value, &HashMap::new(), &place.layout);
        let value = self.bind_value_layout(value, place.layout.clone(), ast::expr_span(source));
        if let (Val::Scalar(Expr::Canonical { value, .. }), Some(meta)) =
            (&value, self.bit_string_meta(source))
        {
            self.source_values
                .borrow_mut()
                .set_explicit_meta(*value, meta);
        }
        let fields = match value {
            Val::Scalar(value) => vec![(String::new(), value)],
            Val::Fields(fields) => fields,
        };
        let mut updates = Vec::new();
        for (name, accesses) in &place.leaves {
            let Some((_, value)) = fields.iter().find(|(field, _)| field == name) else {
                self.sink.emit(
                    crate::diag::Diagnostic::error(
                        "procedure assignment has incompatible aggregate leaves",
                    )
                    .with_code(crate::diag::codes::UNSUPPORTED_EXPR)
                    .at(ast::expr_span(target)),
                );
                return Some(Vec::new());
            };
            for access in accesses {
                let fire = match &access.guard {
                    Some(guard) => Some(and(cond.clone(), guard.clone())),
                    None => cond.clone(),
                };
                match &access.storage {
                    Storage::Signal(signal) => {
                        let mut replacement = self.coerce_to_target(*signal, value.clone());
                        let mut meta = self.bit_string_meta(source);
                        if let Some(bits) = &access.bits {
                            let width = self.out.signals[signal.0 as usize].width;
                            let mut previous = pending.to_vec();
                            previous.extend(updates.iter().cloned());
                            let mut base = self.slice_write_base(*signal, sequential, &previous);
                            for (source, &bit) in bits.iter().enumerate() {
                                base = self.merge_slice(
                                    base,
                                    bit,
                                    bit,
                                    Expr::Slice {
                                        base: Box::new(replacement.clone()),
                                        hi: source as u32,
                                        lo: source as u32,
                                    },
                                    width,
                                );
                                base = self.bind_source_expression(base, ast::expr_span(target));
                            }
                            if self.out.array_element_enums.contains_key(&signal.0) {
                                let companion = SignalId(self.driven_companion(*signal));
                                let meta_width = self.out.signals[companion.0 as usize].width;
                                self.arm_meta_temps(self.cur_ctx, ast::expr_span(source));
                                let replacement_meta = self.partial_write_meta(
                                    source,
                                    &replacement,
                                    bits.len() as u32,
                                );
                                let mut base = self.slice_meta_write_base(
                                    *signal, companion, sequential, &previous,
                                );
                                for (source, &bit) in bits.iter().enumerate() {
                                    base = self.merge_slice(
                                        base,
                                        bit * 4 + 3,
                                        bit * 4,
                                        Expr::Slice {
                                            base: Box::new(replacement_meta.clone()),
                                            hi: source as u32 * 4 + 3,
                                            lo: source as u32 * 4,
                                        },
                                        meta_width,
                                    );
                                    base =
                                        self.bind_source_expression(base, ast::expr_span(target));
                                }
                                self.flush_meta_temps();
                                meta = Some(base);
                            }
                            replacement = base;
                        }
                        updates.push(NextUpdate {
                            span: self.cur_span,
                            target: *signal,
                            cond: fire,
                            expr: replacement,
                            meta,
                        });
                    }
                    Storage::Local { scope, name, field } => {
                        let Some(binding) = self
                            .block_scopes
                            .borrow()
                            .get(*scope)
                            .and_then(|bindings| bindings.get(name))
                            .cloned()
                        else {
                            continue;
                        };
                        let old = self.storage_value(&access.storage).unwrap_or(Expr::Unknown);
                        let mut replacement = value.clone();
                        if let Some(bits) = &access.bits {
                            let width = self
                                .source_evaluated_width(&old)
                                .unwrap_or(self.block_local_width(&binding.ty));
                            let mut base = old.clone();
                            for (source, &bit) in bits.iter().enumerate() {
                                base = self.merge_slice(
                                    base,
                                    bit,
                                    bit,
                                    Expr::Slice {
                                        base: Box::new(replacement.clone()),
                                        hi: source as u32,
                                        lo: source as u32,
                                    },
                                    width,
                                );
                                base = self.bind_source_expression(base, ast::expr_span(target));
                            }
                            replacement = base;
                        }
                        replacement = match fire {
                            Some(fire) => Expr::Select {
                                cond: Box::new(fire),
                                then: Box::new(replacement),
                                els: Box::new(old),
                            },
                            None => replacement,
                        };
                        let value = match binding.value {
                            Val::Scalar(_) => Val::Scalar(replacement),
                            Val::Fields(mut fields) => {
                                if let Some((_, value)) =
                                    fields.iter_mut().find(|(name, _)| name == field)
                                {
                                    *value = replacement;
                                }
                                Val::Fields(fields)
                            }
                        };
                        self.store_block_local(
                            *scope,
                            name.clone(),
                            value,
                            binding.ty,
                            ast::expr_span(target),
                        );
                    }
                }
            }
        }
        Some(updates)
    }
}
