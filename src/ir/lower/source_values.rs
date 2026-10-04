//! Shared source-value bindings and dependency-ordered representation rewrites.
//!
//! This uses the canonical Process arena during hardware normalization. A
//! shallow expression is only one node plus child handles, not a second tree.

use super::*;
use std::sync::Arc;

#[derive(Default)]
pub(super) struct SourceValues {
    pub(super) ir: ProcessIr,
    reads: Vec<Arc<[SignalId]>>,
    pub(super) real: HashMap<ProcessValueId, bool>,
    pub(super) non_integer: HashMap<ProcessValueId, bool>,
    pub(super) meta_width: HashMap<(ProcessValueId, u32), u32>,
    pub(super) coerced_real: HashMap<ProcessValueId, Expr>,
    meta_epoch: usize,
    meta_presence: Vec<bool>,
}

impl SourceValues {
    pub(super) fn reference(&self, id: ProcessValueId) -> Expr {
        Expr::Canonical {
            value: id,
            reads: self.reads[id.0 as usize].clone(),
        }
    }

    fn child(&self, id: ProcessValueId) -> Result<Expr, String> {
        match self.ir.values[id.0 as usize].kind {
            ProcessValueKind::Number(_)
            | ProcessValueKind::Char(_)
            | ProcessValueKind::Signal { .. }
            | ProcessValueKind::Invalid => {
                super::super::derive::digital_node(&self.ir, id, |_| unreachable!())
            }
            _ => Ok(self.reference(id)),
        }
    }

    pub(super) fn node(&self, id: ProcessValueId) -> Expr {
        super::super::derive::digital_node(&self.ir, id, |child| self.child(child))
            .expect("source hardware values use digital arena operations")
    }

    pub(super) fn may_have_meta(
        &mut self,
        id: ProcessValueId,
        meta_of: &HashMap<u32, u32>,
    ) -> bool {
        if self.meta_epoch != meta_of.len() {
            self.meta_presence.clear();
            self.meta_epoch = meta_of.len();
        }
        for node in &self.ir.values[self.meta_presence.len()..] {
            let has_meta = match &node.kind {
                ProcessValueKind::Signal { signals, state } => {
                    !matches!(state, ProcessSignalState::Event)
                        && signals.iter().any(|signal| meta_of.contains_key(&signal.0))
                }
                kind => super::super::process::process_value_dependencies(kind)
                    .iter()
                    .any(|dependency| self.meta_presence[dependency.0 as usize]),
            };
            self.meta_presence.push(has_meta);
        }
        self.meta_presence[id.0 as usize]
    }

    pub(super) fn append(
        &mut self,
        expression: &Expr,
        span: crate::diag::Span,
        ty: Option<crate::types::Ty>,
    ) -> ProcessValueId {
        let first = self.ir.values.len();
        let id = self.ir.push_digital_expr(expression, span);
        if id.0 as usize >= first {
            self.ir.values[id.0 as usize].ty = ty;
        }
        for index in first..self.ir.values.len() {
            let node = &self.ir.values[index];
            let mut seen = HashSet::new();
            let mut reads = Vec::new();
            if let ProcessValueKind::Signal { signals, .. } = &node.kind {
                reads.extend(signals.iter().copied());
            } else {
                for dependency in super::super::process::process_value_dependencies(&node.kind) {
                    for &signal in self.reads[dependency.0 as usize].iter() {
                        if seen.insert(signal) {
                            reads.push(signal);
                        }
                    }
                }
            }
            self.reads.push(reads.into());
        }
        id
    }

    /// A typed scalar binding evaluates at its own format before a later
    /// consumer widens it. Otherwise each signed parent adds another bit and
    /// reevaluates the entire alias chain at that changing contextual width.
    fn bind_scalar(
        &mut self,
        operand: ProcessValueId,
        ty: Option<crate::types::Ty>,
        span: crate::diag::Span,
    ) -> ProcessValueId {
        let id = ProcessValueId(self.ir.values.len() as u32);
        self.ir.values.push(ProcessValue {
            span,
            ty,
            bit_width: None,
            kind: ProcessValueKind::RawResize { operand },
        });
        if !self.ir.value_layouts.is_empty() {
            self.ir.value_layouts.push(None);
        }
        self.reads.push(self.reads[operand.0 as usize].clone());
        id
    }

    /// Storage-free locals still have a declared representation. Keep that
    /// boundary in the arena, including signed ranges and nominal families.
    fn bind_layout(
        &mut self,
        expression: &Expr,
        layout: SourceLayout,
        span: crate::diag::Span,
    ) -> ProcessValueId {
        let operand = self.append(expression, span, None);
        if matches!(
            self.ir.values[operand.0 as usize].kind,
            ProcessValueKind::RawResize { .. }
        ) && self
            .ir
            .value_layouts
            .get(operand.0 as usize)
            .and_then(Option::as_ref)
            == Some(&layout)
        {
            return operand;
        }
        let id = self.bind_scalar(operand, None, span);
        self.ir.values[id.0 as usize].bit_width = layout.packed_width();
        self.ir.value_layouts.resize(self.ir.values.len(), None);
        self.ir.value_layouts[id.0 as usize] = Some(layout);
        id
    }

    fn retain_format(&mut self, id: ProcessValueId, old: &Self, index: usize) {
        self.ir.values[id.0 as usize].bit_width = old.ir.values[index].bit_width;
        if let Some(Some(layout)) = old.ir.value_layouts.get(index) {
            self.ir.value_layouts.resize(self.ir.values.len(), None);
            self.ir.value_layouts[id.0 as usize] = Some(layout.clone());
        }
    }

    /// Rewrite each dependency once. New operands precede their users and
    /// old roots are remapped afterwards, so rewrites may change graph shape
    /// without introducing forward references or expanding shared values.
    pub(super) fn rewrite(
        &mut self,
        mut rewrite: impl FnMut(&Self, &mut Expr),
    ) -> Vec<ProcessValueId> {
        let old = std::mem::take(self);
        let mut mapped = Vec::with_capacity(old.ir.values.len());
        for (index, value) in old.ir.values.iter().enumerate() {
            if let ProcessValueKind::RawResize { operand } = value.kind {
                let id = self.bind_scalar(mapped[operand.0 as usize], value.ty.clone(), value.span);
                self.retain_format(id, &old, index);
                mapped.push(id);
                continue;
            }
            let mut expression = super::super::derive::digital_node(
                &old.ir,
                ProcessValueId(index as u32),
                |child| self.child(mapped[child.0 as usize]),
            )
            .expect("source representation rewrite uses digital nodes");
            rewrite(self, &mut expression);
            mapped.push(self.append(&expression, value.span, value.ty.clone()));
        }
        mapped
    }

    /// The table recognizer inspects only three nodes: a slice, a shift and
    /// its stride multiplication. Exposing that fixed shape does not expand
    /// the variable index's shared value graph.
    fn expose_lookup_shape(&self, expression: &mut Expr) {
        let Expr::Slice { base, .. } = expression else {
            return;
        };
        if let Expr::Canonical { value, .. } = base.as_ref() {
            **base = self.node(*value);
        }
        let Expr::Binary {
            op: BinOp::Shr,
            rhs,
            ..
        } = base.as_mut()
        else {
            return;
        };
        if let Expr::Canonical { value, .. } = rhs.as_ref() {
            **rhs = self.node(*value);
        }
    }

    pub(super) fn remap_expression(&self, expression: &mut Expr, mapped: &[ProcessValueId]) {
        visit_references(expression, &mut |id| self.reference(mapped[id.0 as usize]));
    }

    /// Drop unused source aliases and failed speculative overload values.
    /// Compaction copies live arena nodes once, retaining dependency identity.
    pub(super) fn retain_reachable(&mut self, draft: &mut HardwareDraft) {
        let mut pending = Vec::new();
        for expression in draft.expressions_mut() {
            visit_references(expression, &mut |id| {
                pending.push(id);
                self.reference(id)
            });
        }
        let mut live = HashSet::new();
        while let Some(id) = pending.pop() {
            if live.insert(id) {
                pending.extend(super::super::process::process_value_dependencies(
                    &self.ir.values[id.0 as usize].kind,
                ));
            }
        }
        let old = std::mem::take(self);
        let mut mapped = vec![ProcessValueId(0); old.ir.values.len()];
        for (index, value) in old.ir.values.iter().enumerate() {
            let id = ProcessValueId(index as u32);
            if !live.contains(&id) {
                continue;
            }
            if let ProcessValueKind::RawResize { operand } = value.kind {
                mapped[index] =
                    self.bind_scalar(mapped[operand.0 as usize], value.ty.clone(), value.span);
                self.retain_format(mapped[index], &old, index);
                continue;
            }
            let expression = super::super::derive::digital_node(&old.ir, id, |child| {
                Ok(self.reference(mapped[child.0 as usize]))
            })
            .expect("live source values have dominating dependencies");
            mapped[index] = self.append(&expression, value.span, value.ty.clone());
        }
        for expression in draft.expressions_mut() {
            self.remap_expression(expression, &mapped);
        }
    }
}

fn visit_references(expression: &mut Expr, visit: &mut impl FnMut(ProcessValueId) -> Expr) {
    match expression {
        Expr::Canonical { value, .. } => *expression = visit(*value),
        Expr::Unary { rhs, .. }
        | Expr::Slice { base: rhs, .. }
        | Expr::TableLookup { index: rhs, .. } => visit_references(rhs, visit),
        Expr::Binary { lhs, rhs, .. } => {
            visit_references(lhs, visit);
            visit_references(rhs, visit);
        }
        Expr::CheckedIndex { index, valid, .. } => {
            visit_references(index, visit);
            visit_references(valid, visit);
        }
        Expr::Select { cond, then, els } => {
            visit_references(cond, visit);
            visit_references(then, visit);
            visit_references(els, visit);
        }
        Expr::MetaCmp {
            operands, inner, ..
        } => {
            for operand in operands {
                visit_references(operand, visit);
            }
            visit_references(inner, visit);
        }
        Expr::CCall { args, .. } => {
            for argument in args {
                visit_references(argument, visit);
            }
        }
        _ => {}
    }
}

impl HardwareDraft {
    pub(super) fn expressions_mut(&mut self) -> impl Iterator<Item = &mut Expr> {
        self.drivers
            .iter_mut()
            .flat_map(|write| {
                write
                    .cond
                    .iter_mut()
                    .chain(std::iter::once(&mut write.expr))
                    .chain(write.meta.iter_mut())
            })
            .chain(self.event_blocks.iter_mut().flat_map(|block| {
                std::iter::once(&mut block.condition).chain(block.updates.iter_mut().flat_map(
                    |write| {
                        write
                            .cond
                            .iter_mut()
                            .chain(std::iter::once(&mut write.expr))
                            .chain(write.meta.iter_mut())
                    },
                ))
            }))
    }
}

impl Lowering<'_> {
    pub(super) fn bind_source_expression(&self, expression: Expr, span: crate::diag::Span) -> Expr {
        let Val::Scalar(expression) = self.bind_source_value(Val::Scalar(expression), span, None)
        else {
            unreachable!()
        };
        expression
    }

    /// Match flattened value paths against the same recursive layout used for
    /// signals. Do not infer a leaf's representation from its expression.
    pub(super) fn bind_block_value(
        &self,
        value: Val,
        ty: &ast::Type,
        span: crate::diag::Span,
    ) -> Val {
        fn leaves(layout: &SourceLayout, prefix: String, out: &mut HashMap<String, SourceLayout>) {
            match &layout.kind {
                LayoutKind::Struct { fields, .. } => {
                    for field in fields {
                        let path = if prefix.is_empty() {
                            field.name.clone()
                        } else {
                            format!("{prefix}.{}", field.name)
                        };
                        leaves(&field.layout, path, out);
                    }
                }
                LayoutKind::Array {
                    range: Some(range),
                    element,
                } => {
                    for index in loop_range(range.left, range.right) {
                        leaves(element, format!("{prefix}[{index}]"), out);
                    }
                }
                _ => {
                    out.insert(prefix, layout.clone());
                }
            }
        }
        let layout = self.source_layout(ty, &self.cur_env);
        let bind = |expression: Expr, layout: SourceLayout| {
            let expression = match (&layout.kind, expression) {
                (
                    LayoutKind::Scalar {
                        domain: ScalarDomain::Character,
                        ..
                    },
                    Expr::Logic(character),
                ) => Expr::Const(u64::from(character as u32)),
                (
                    LayoutKind::Scalar {
                        domain: ScalarDomain::Enum(name),
                        ..
                    },
                    Expr::Logic(character),
                ) => self
                    .char_disc(character, name)
                    .map(Expr::Const)
                    .unwrap_or(Expr::Logic(character)),
                (
                    LayoutKind::Scalar {
                        domain: ScalarDomain::Real,
                        ..
                    },
                    expression,
                ) => self.coerce_real(expression),
                (_, expression) => expression,
            };
            let mut arena = self.source_values.borrow_mut();
            let id = arena.bind_layout(&expression, layout, span);
            arena.reference(id)
        };
        match value {
            Val::Scalar(expression) => Val::Scalar(bind(expression, layout)),
            Val::Fields(fields) => {
                let mut layouts = HashMap::new();
                leaves(&layout, String::new(), &mut layouts);
                Val::Fields(
                    fields
                        .into_iter()
                        .map(|(name, expression)| {
                            let expression = match layouts.remove(&name) {
                                Some(layout) => bind(expression, layout),
                                None => self.bind_source_expression(expression, span),
                            };
                            (name, expression)
                        })
                        .collect(),
                )
            }
        }
    }

    pub(super) fn bind_source_value(
        &self,
        value: Val,
        span: crate::diag::Span,
        ty: Option<crate::types::Ty>,
    ) -> Val {
        let mut arena = self.source_values.borrow_mut();
        let mut bind = |expression: Expr, ty: Option<crate::types::Ty>| match expression {
            Expr::Canonical { .. }
            | Expr::Const(_)
            | Expr::WideConst(_)
            | Expr::Real(_)
            | Expr::Logic(_)
            | Expr::Current(_)
            | Expr::Old(_)
            | Expr::Event(_)
            | Expr::Unknown => expression,
            expression => {
                let mut id = arena.append(&expression, span, ty.clone());
                if matches!(ty, Some(crate::types::Ty::Integer | crate::types::Ty::Real)) {
                    id = arena.bind_scalar(id, ty, span);
                }
                arena.reference(id)
            }
        };
        match value {
            Val::Scalar(expression) => Val::Scalar(bind(expression, ty)),
            Val::Fields(fields) => Val::Fields(
                fields
                    .into_iter()
                    .map(|(name, expression)| (name, bind(expression, None)))
                    .collect(),
            ),
        }
    }

    pub(super) fn source_node(&self, id: ProcessValueId) -> Expr {
        self.source_values.borrow().node(id)
    }

    pub(super) fn compact_source_lookups(&mut self) {
        let arena = self.source_values.get_mut();
        let tables = &mut self.out.lookup_tables;
        let mut intern = tables
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, table)| (table, LookupTableId(index)))
            .collect();
        let mapped = arena.rewrite(|arena, expression| {
            arena.expose_lookup_shape(expression);
            *expression = compact_lookup_expr(expression.clone(), tables, &mut intern);
        });
        for expression in self.hardware.expressions_mut() {
            arena.remap_expression(expression, &mapped);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concrete_local_formats_survive_rewrites_and_reachability_compaction() {
        let span = crate::diag::Span::new(crate::diag::FileId(0), 0..1);
        let layout = SourceLayout {
            span,
            kind: LayoutKind::Scalar {
                width: 4,
                domain: ScalarDomain::Integer,
                nominal: Some("integer".to_owned()),
                value_range: Some((-8, 7)),
            },
        };
        let mut arena = SourceValues::default();
        arena.append(&Expr::Const(99), span, None); // deliberately unreachable
        let local = arena.bind_layout(&Expr::Current(SignalId(0)), layout.clone(), span);
        let scalar = arena.bind_scalar(local, Some(crate::types::Ty::Integer), span);
        assert_eq!(arena.ir.value_layouts.len(), arena.ir.values.len());
        let mapped = arena.rewrite(|_, _| {});
        let mut draft = HardwareDraft::default();
        draft.drivers.push(Driver {
            target: SignalId(1),
            cond: None,
            expr: arena.reference(mapped[scalar.0 as usize]),
            meta: None,
            ctx: 0,
            span: Some(span),
        });
        arena.retain_reachable(&mut draft);
        assert_eq!(arena.ir.values.len(), 3);
        assert_eq!(arena.ir.value_layouts.len(), 3);
        assert_eq!(arena.ir.values[1].bit_width, Some(4));
        assert_eq!(arena.ir.value_layouts[1], Some(layout));
        assert_eq!(arena.reads[2].as_ref(), &[SignalId(0)]);
        assert!(matches!(
            arena.ir.values[2].kind,
            ProcessValueKind::RawResize {
                operand: ProcessValueId(1)
            }
        ));
    }

    #[test]
    fn table_recognition_preserves_a_typed_stride_boundary() {
        let span = crate::diag::Span::new(crate::diag::FileId(0), 0..1);
        let mut arena = SourceValues::default();
        let stride = arena.append(
            &Expr::Binary {
                op: BinOp::Mul,
                lhs: Box::new(Expr::Current(SignalId(0))),
                rhs: Box::new(Expr::Const(4)),
            },
            span,
            Some(crate::types::Ty::Integer),
        );
        let bound = arena.bind_scalar(stride, Some(crate::types::Ty::Integer), span);
        let lookup = |index| Expr::Slice {
            base: Box::new(Expr::Binary {
                op: BinOp::Shr,
                lhs: Box::new(Expr::Const(0x1234)),
                rhs: Box::new(arena.reference(index)),
            }),
            hi: 3,
            lo: 0,
        };
        let mut unbound = lookup(stride);
        arena.expose_lookup_shape(&mut unbound);
        assert!(packed_lookup(&unbound).is_some());
        let mut bound = lookup(bound);
        arena.expose_lookup_shape(&mut bound);
        assert!(
            packed_lookup(&bound).is_none(),
            "compaction would bypass the bound stride's evaluation width"
        );
    }
}
