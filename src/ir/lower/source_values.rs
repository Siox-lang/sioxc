//! Shared source-value bindings and dependency-ordered representation rewrites.
//!
//! This uses the canonical Process arena during hardware normalization. A
//! shallow expression is only one node plus child handles, not a second tree.

use super::*;
use std::sync::Arc;

mod build;
mod metadata;
mod reconstruct;

/// One element written into a packed vector inside a hardware function
/// (`r[k] = 'X'`). The value plane is the ordinary bit merge; the companion
/// is the base's with element `bit`'s nibble replaced, so the element holds
/// the written value whatever it is, as an enum vector element does.
#[derive(Clone)]
pub(super) struct MetaMerge {
    /// The vector before the write.
    pub(super) base: Expr,
    /// The written element's value, whose companion a copy carries.
    pub(super) element: Expr,
    /// A literal's discriminant, the element's whole companion nibble.
    pub(super) literal: Option<u64>,
    /// Whether that literal is a metavalue, so the result has a companion.
    pub(super) metavalue: bool,
    /// The element's storage position.
    pub(super) bit: u32,
}

#[derive(Default)]
pub(super) struct SourceValues {
    pub(super) ir: ProcessIr,
    reads: Vec<Arc<[SignalId]>>,
    /// The one shared empty read set, so a constant costs a pointer.
    no_reads: Option<Arc<[SignalId]>>,
    pub(super) real: HashMap<ProcessValueId, bool>,
    pub(super) non_integer: HashMap<ProcessValueId, bool>,
    pub(super) meta_width: HashMap<(ProcessValueId, u32), u32>,
    pub(super) coerced_real: HashMap<ProcessValueId, Expr>,
    /// Explicit literal planes survive a captured argument without retaining
    /// its syntax. Computed planes still follow arena dependencies normally.
    pub(super) explicit_meta: HashMap<ProcessValueId, Expr>,
    /// Element writes whose companion is the base's with one nibble replaced.
    pub(super) merge_meta: HashMap<ProcessValueId, MetaMerge>,
    /// Physical bit projections used to compute companion planes, not source
    /// element reads. Keep this intent until read reconstruction has finished.
    raw_bits: HashSet<ProcessValueId>,
    meta_epoch: usize,
    meta_presence: Vec<bool>,
}

impl SourceValues {
    pub(super) fn set_explicit_meta(&mut self, value: ProcessValueId, meta: Expr) {
        let span = self.ir.values[value.0 as usize].span;
        let meta = self.append(&meta, span, None);
        self.explicit_meta.insert(value, self.reference(meta));
        self.meta_presence.clear();
    }
    pub(super) fn set_merge_meta(&mut self, value: ProcessValueId, merge: MetaMerge) {
        self.merge_meta.insert(value, merge);
        self.meta_presence.clear();
    }
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
        for (index, node) in self
            .ir
            .values
            .iter()
            .enumerate()
            .skip(self.meta_presence.len())
        {
            let id = ProcessValueId(index as u32);
            let has_meta = self.explicit_meta.contains_key(&id)
                || self
                    .merge_meta
                    .get(&id)
                    .is_some_and(|merge| merge.metavalue)
                || match &node.kind {
                    ProcessValueKind::Signal { signals, state } => {
                        !matches!(state, ProcessSignalState::Event)
                            && signals.iter().any(|signal| meta_of.contains_key(&signal.0))
                    }
                    kind => {
                        super::super::process::any_process_value_dependency(kind, |dependency| {
                            self.meta_presence[dependency.0 as usize]
                        })
                    }
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
        // Compound source values are constructed by arena operations, never
        // recursively imported here. A captured ID keeps its existing format.
        let kind = match expression {
            Expr::Canonical { value, .. } => return *value,
            Expr::Const(value) => ProcessValueKind::Number(ProcessNumber::Integer(vec![*value])),
            Expr::WideConst(words) => {
                ProcessValueKind::Number(ProcessNumber::Integer(words.clone()))
            }
            Expr::Real(value) => ProcessValueKind::Number(ProcessNumber::Real(value.to_bits())),
            Expr::Logic(character) => ProcessValueKind::Char(*character),
            Expr::Current(signal) | Expr::Old(signal) | Expr::Event(signal) => {
                ProcessValueKind::Signal {
                    signals: vec![*signal],
                    state: match expression {
                        Expr::Old(_) => ProcessSignalState::Old,
                        Expr::Event(_) => ProcessSignalState::Event,
                        _ => ProcessSignalState::Current,
                    },
                }
            }
            Expr::Unknown => ProcessValueKind::Invalid,
            _ => panic!("compound source values must be constructed in the canonical arena"),
        };
        self.push_node(
            ProcessValue {
                span,
                ty,
                bit_width: None,
                kind,
            },
            None,
        )
    }

    /// Import only handcrafted test/oracle fixtures, never a source expression.
    #[cfg(test)]
    pub(super) fn import_test_fragment(
        &mut self,
        expression: &Expr,
        span: crate::diag::Span,
        ty: Option<crate::types::Ty>,
    ) -> ProcessValueId {
        let first = self.ir.values.len();
        let id = self.ir.import_test_fragment(expression, span);
        if id.0 as usize >= first {
            self.ir.values[id.0 as usize].ty = ty;
        }
        self.record_reads(first);
        id
    }

    /// Append an already constructed canonical node with its exact format.
    pub(super) fn push_node(
        &mut self,
        value: ProcessValue,
        layout: Option<SourceLayout>,
    ) -> ProcessValueId {
        let id = ProcessValueId(self.ir.values.len() as u32);
        self.ir.values.push(value);
        self.ir.value_layouts.resize(self.ir.values.len(), None);
        self.ir.value_layouts[id.0 as usize] = layout;
        self.record_reads(id.0 as usize);
        id
    }

    fn record_reads(&mut self, first: usize) {
        for index in first..self.ir.values.len() {
            let node = &self.ir.values[index];
            let reads = if let ProcessValueKind::Signal { signals, .. } = &node.kind {
                Arc::from(signals.as_slice())
            } else {
                // A node reading through at most one distinct operand set
                // shares that set; only a real union is copied.
                let mut shared: Option<&Arc<[SignalId]>> = None;
                let mut union = false;
                super::super::process::for_each_process_value_dependency(
                    &node.kind,
                    |dependency| {
                        let operand = &self.reads[dependency.0 as usize];
                        match shared {
                            _ if operand.is_empty() => {}
                            None => shared = Some(operand),
                            Some(set) if Arc::ptr_eq(set, operand) => {}
                            Some(_) => union = true,
                        }
                    },
                );
                match (shared, union) {
                    (Some(set), false) => Arc::clone(set),
                    (None, _) => Arc::clone(self.no_reads.get_or_insert_with(|| Arc::from([]))),
                    (Some(_), true) => {
                        let mut seen = HashSet::new();
                        let mut reads = Vec::new();
                        super::super::process::for_each_process_value_dependency(
                            &node.kind,
                            |dependency| {
                                for &signal in self.reads[dependency.0 as usize].iter() {
                                    if seen.insert(signal) {
                                        reads.push(signal);
                                    }
                                }
                            },
                        );
                        reads.into()
                    }
                }
            };
            self.reads.push(reads);
        }
    }

    pub(super) fn binary(
        &mut self,
        operation: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        span: crate::diag::Span,
    ) -> Expr {
        let left = self.append(lhs, span, None);
        let right = self.append(rhs, span, None);
        let id = self.push_node(
            ProcessValue {
                span,
                ty: None,
                bit_width: None,
                kind: ProcessValueKind::Binary {
                    operation: super::super::process::process_binary_from_digital(operation),
                    left,
                    right,
                },
            },
            None,
        );
        self.reference(id)
    }

    pub(super) fn unary(
        &mut self,
        operation: ProcessUnaryOp,
        rhs: &Expr,
        span: crate::diag::Span,
    ) -> Expr {
        let operand = self.append(rhs, span, None);
        let id = self.push_node(
            ProcessValue {
                span,
                ty: None,
                bit_width: None,
                kind: ProcessValueKind::Unary { operation, operand },
            },
            None,
        );
        self.reference(id)
    }

    /// Resolving leaf values changes neither identities nor signal dependencies.
    pub(super) fn normalize_logic_literals(&mut self, lut: &HashMap<String, u64>) {
        for node in &mut self.ir.values {
            if let ProcessValueKind::Char(character) = node.kind {
                node.kind = ProcessValueKind::Number(ProcessNumber::Integer(vec![lut
                    .get(&format!("'{character}'"))
                    .copied()
                    .unwrap_or(0)]));
            }
        }
        self.clear_analysis();
    }

    fn clear_analysis(&mut self) {
        self.real.clear();
        self.non_integer.clear();
        self.meta_width.clear();
        self.coerced_real.clear();
        self.meta_presence.clear();
    }

    /// Inspect only the fixed lookup shape, never project or expand its index.
    fn packed_lookup(&self, value: ProcessValueId) -> Option<(LookupTable, ProcessValueId)> {
        let ProcessValueKind::BitSlice { base, high, low: 0 } =
            self.ir.values[value.0 as usize].kind
        else {
            return None;
        };
        let element_width = high.checked_add(1)?;
        let ProcessValueKind::Binary {
            operation: ProcessBinaryOp::Shr,
            left: packed,
            right: offset,
        } = self.ir.values[base.0 as usize].kind
        else {
            return None;
        };
        let ProcessValueKind::Binary {
            operation: ProcessBinaryOp::Mul,
            left: index,
            right: stride,
        } = self.ir.values[offset.0 as usize].kind
        else {
            return None;
        };
        // A concrete intermediate representation is an evaluation boundary,
        // not an identity that table recognition may silently bypass.
        if [base, packed, offset, stride].iter().any(|id| {
            self.ir.values[id.0 as usize].bit_width.is_some()
                || self
                    .ir
                    .value_layouts
                    .get(id.0 as usize)
                    .is_some_and(Option::is_some)
        }) {
            return None;
        }
        let ProcessValueKind::Number(ProcessNumber::Integer(stride)) =
            &self.ir.values[stride.0 as usize].kind
        else {
            return None;
        };
        if stride.as_slice() != [u64::from(element_width)] {
            return None;
        }
        let ProcessValueKind::Number(ProcessNumber::Integer(words)) =
            &self.ir.values[packed.0 as usize].kind
        else {
            return None;
        };
        Some((packed_lookup_table(element_width, words)?, index))
    }

    pub(super) fn compact_lookups(
        &mut self,
        tables: &mut Vec<LookupTable>,
        intern: &mut HashMap<LookupTable, LookupTableId>,
    ) {
        let mut changed = false;
        for index in 0..self.ir.values.len() {
            let Some((table, operand)) = self.packed_lookup(ProcessValueId(index as u32)) else {
                continue;
            };
            let table = match intern.get(&table).copied() {
                Some(id) => id,
                None => {
                    let id = LookupTableId(tables.len());
                    tables.push(table.clone());
                    intern.insert(table, id);
                    id
                }
            };
            self.ir.values[index].kind = ProcessValueKind::TableLookup {
                table,
                index: operand,
            };
            // The other fixed-shape operands are constants: reads are unchanged.
            changed = true;
        }
        if changed {
            self.clear_analysis();
        }
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
        if let Expr::Canonical { value, .. } = expression {
            if matches!(
                self.ir.values[value.0 as usize].kind,
                ProcessValueKind::RawResize { .. }
            ) && self
                .ir
                .value_layouts
                .get(value.0 as usize)
                .and_then(Option::as_ref)
                == Some(&layout)
            {
                return *value;
            }
        }
        let operand = self.append(expression, span, None);
        let id = self.bind_scalar(operand, None, span);
        self.ir.values[id.0 as usize].bit_width = layout.packed_width();
        self.ir.value_layouts.resize(self.ir.values.len(), None);
        self.ir.value_layouts[id.0 as usize] = Some(layout);
        id
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
                if let Some(meta) = self.explicit_meta.get(&id) {
                    visit_references(&mut meta.clone(), &mut |id| {
                        pending.push(id);
                        self.reference(id)
                    });
                }
                if let Some(merge) = self.merge_meta.get(&id) {
                    for expression in [&merge.base, &merge.element] {
                        visit_references(&mut expression.clone(), &mut |id| {
                            pending.push(id);
                            self.reference(id)
                        });
                    }
                }
                super::super::process::for_each_process_value_dependency(
                    &self.ir.values[id.0 as usize].kind,
                    |id| pending.push(id),
                );
            }
        }
        let mut old = std::mem::take(self);
        let mut mapped = vec![ProcessValueId(0); old.ir.values.len()];
        for (index, mut value) in old.ir.values.into_iter().enumerate() {
            let id = ProcessValueId(index as u32);
            if !live.contains(&id) {
                continue;
            }
            super::super::process::remap_process_value_dependencies(&mut value.kind, |child| {
                assert!(child.0 < id.0, "source operands precede their users");
                mapped[child.0 as usize]
            });
            let layout = old.ir.value_layouts.get_mut(index).and_then(Option::take);
            mapped[index] = self.push_node(value, layout);
            if old.raw_bits.contains(&id) {
                self.raw_bits.insert(mapped[index]);
            }
        }
        for expression in draft.expressions_mut() {
            self.remap_expression(expression, &mapped);
        }
        for (value, meta) in old.explicit_meta {
            if live.contains(&value) {
                let mut meta = meta;
                self.remap_expression(&mut meta, &mapped);
                self.set_explicit_meta(mapped[value.0 as usize], meta);
            }
        }
        for (value, mut merge) in old.merge_meta {
            if live.contains(&value) {
                self.remap_expression(&mut merge.base, &mapped);
                self.remap_expression(&mut merge.element, &mapped);
                self.set_merge_meta(mapped[value.0 as usize], merge);
            }
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
    pub(super) fn source_evaluated_width(&self, expression: &Expr) -> Option<u32> {
        match expression {
            Expr::Current(signal) | Expr::Old(signal) => self.out.signal_width(*signal),
            Expr::Event(_) => Some(1),
            Expr::Const(_) | Expr::Real(_) => Some(64),
            Expr::WideConst(words) => u32::try_from(words.len()).ok()?.checked_mul(64),
            Expr::Slice { hi, lo, .. } => hi.checked_sub(*lo)?.checked_add(1),
            Expr::Canonical { value, .. } => {
                let arena = self.source_values.borrow();
                let node = &arena.ir.values[value.0 as usize];
                node.bit_width.or_else(|| match &node.kind {
                    ProcessValueKind::Signal {
                        state: ProcessSignalState::Event,
                        ..
                    } => Some(1),
                    ProcessValueKind::Signal { signals, .. } => {
                        signals.iter().try_fold(0u32, |total, signal| {
                            total.checked_add(self.out.signal_width(*signal)?)
                        })
                    }
                    ProcessValueKind::BitSlice { high, low, .. } => {
                        high.checked_sub(*low)?.checked_add(1)
                    }
                    _ => None,
                })
            }
            _ => None,
        }
    }

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
        self.bind_value_layout(value, self.source_layout(ty, &self.cur_env), span)
    }

    pub(super) fn bind_value_layout(
        &self,
        value: Val,
        layout: SourceLayout,
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
        let bind = |expression: Expr, layout: SourceLayout| {
            let mut expression = match (&layout.kind, expression) {
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
                ) => self.coerce_real(expression, span),
                (_, expression) => expression,
            };
            // Evaluate narrow arithmetic at the storage format before binding
            // its bits; extending an already-wrapped `-3` cannot recover it.
            // Existing signal reads and typed boundaries need no extra mask.
            if matches!(
                layout.kind,
                LayoutKind::Packed { .. }
                    | LayoutKind::Scalar {
                        domain: ScalarDomain::Integer | ScalarDomain::Bits,
                        ..
                    }
            ) {
                if let Some(width) = layout.packed_width() {
                    if self
                        .source_evaluated_width(&expression)
                        .is_none_or(|evaluated| evaluated < width)
                    {
                        expression = self.source_slice(&expression, width - 1, 0, span);
                    }
                }
            }
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

    /// An explicit scalar evaluation boundary, unlike an inferred type hint
    /// while capturing a function value. Canonical operands keep their format;
    /// later wide consumers resize the bound result, not its arithmetic.
    pub(super) fn bind_source_scalar(
        &self,
        expression: Expr,
        span: crate::diag::Span,
        ty: crate::types::Ty,
    ) -> Expr {
        let mut arena = self.source_values.borrow_mut();
        let operand = arena.append(&expression, span, Some(ty.clone()));
        let id = arena.bind_scalar(operand, Some(ty), span);
        arena.reference(id)
    }

    pub(super) fn bind_source_value(
        &self,
        value: Val,
        span: crate::diag::Span,
        ty: Option<crate::types::Ty>,
    ) -> Val {
        let mut arena = self.source_values.borrow_mut();
        let mut bind = |expression: Expr, ty: Option<crate::types::Ty>| match expression {
            // The arena's format is authoritative. Inferred hints from generic
            // function syntax must not override an already canonical value.
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
        arena.compact_lookups(tables, &mut intern);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "compound source values must be constructed in the canonical arena")]
    fn production_binding_rejects_a_private_fragment_even_in_tests() {
        let span = crate::diag::Span::new(crate::diag::FileId(0), 0..1);
        SourceValues::default().append(
            &Expr::Binary {
                op: BinOp::Add,
                lhs: Box::new(Expr::Const(1)),
                rhs: Box::new(Expr::Const(2)),
            },
            span,
            None,
        );
    }

    #[test]
    fn canonical_lookup_compaction_keeps_formats_sharing_and_failed_shapes() {
        let span = crate::diag::Span::new(crate::diag::FileId(0), 4..9);
        let mut arena = SourceValues::default();
        let call = arena.import_test_fragment(
            &Expr::CCall {
                name: "next_index".into(),
                args: vec![Expr::Current(SignalId(0))],
                f64_args: vec![false],
                integer_args: vec![true],
                f64_ret: false,
                integer_ret: true,
            },
            span,
            None,
        );
        let lookup = Expr::Slice {
            base: Box::new(Expr::Binary {
                op: BinOp::Shr,
                lhs: Box::new(Expr::Const(0x1234)),
                rhs: Box::new(Expr::Binary {
                    op: BinOp::Mul,
                    lhs: Box::new(arena.reference(call)),
                    rhs: Box::new(Expr::Const(4)),
                }),
            }),
            hi: 3,
            lo: 0,
        };
        let first = arena.import_test_fragment(&lookup, span, None);
        let second = arena.import_test_fragment(&lookup, span, None);
        let narrowed = arena.import_test_fragment(&lookup, span, None);
        let ProcessValueKind::BitSlice { base, .. } = arena.ir.values[narrowed.0 as usize].kind
        else {
            unreachable!()
        };
        let ProcessValueKind::Binary { right: offset, .. } = arena.ir.values[base.0 as usize].kind
        else {
            unreachable!()
        };
        arena.ir.values[offset.0 as usize].bit_width = Some(2);
        let ordinary = arena.import_test_fragment(
            &Expr::Slice {
                base: Box::new(arena.reference(call)),
                hi: 3,
                lo: 0,
            },
            span,
            None,
        );
        let layout = SourceLayout {
            span,
            kind: LayoutKind::Scalar {
                width: 4,
                domain: ScalarDomain::Integer,
                nominal: Some("integer".into()),
                value_range: None,
            },
        };
        arena.ir.values[first.0 as usize].bit_width = Some(4);
        arena.ir.value_layouts.resize(arena.ir.values.len(), None);
        arena.ir.value_layouts[first.0 as usize] = Some(layout.clone());
        let before = arena.ir.values.clone();
        let mut tables = Vec::new();
        arena.compact_lookups(&mut tables, &mut HashMap::new());
        assert_eq!(
            tables,
            vec![LookupTable {
                element_width: 4,
                values: vec![4, 3, 2, 1]
            }]
        );
        assert_eq!(arena.ir.values.len(), before.len());
        for id in [first, second] {
            assert_eq!(
                arena.ir.values[id.0 as usize].kind,
                ProcessValueKind::TableLookup {
                    table: LookupTableId(0),
                    index: call
                }
            );
            assert_eq!(arena.ir.values[id.0 as usize].span, span);
            assert_eq!(arena.reads[id.0 as usize].as_ref(), &[SignalId(0)]);
        }
        assert_eq!(arena.ir.values[first.0 as usize].bit_width, Some(4));
        assert_eq!(arena.ir.value_layouts[first.0 as usize], Some(layout));
        assert_eq!(
            arena.ir.values[ordinary.0 as usize],
            before[ordinary.0 as usize]
        );
        for (index, value) in before.iter().enumerate() {
            if index != first.0 as usize && index != second.0 as usize {
                assert_eq!(&arena.ir.values[index], value);
            }
        }
        assert_eq!(
            arena
                .ir
                .values
                .iter()
                .filter(|v| matches!(v.kind, ProcessValueKind::ForeignCall { .. }))
                .count(),
            1
        );
        assert!(arena.ir.validate(1).is_empty());
    }

    #[test]
    fn source_operator_nodes_reuse_operand_identities_and_spans() {
        let span = crate::diag::Span::new(crate::diag::FileId(0), 4..9);
        let mut arena = SourceValues::default();
        let input = arena.import_test_fragment(&Expr::Current(SignalId(0)), span, None);
        let input = arena.reference(input);
        let product = arena.binary(BinOp::SMul, &input, &input, span);
        let result = arena.unary(ProcessUnaryOp::Neg, &product, span);
        assert!(matches!(
            result,
            Expr::Canonical {
                value: ProcessValueId(2),
                ..
            }
        ));
        assert_eq!(arena.ir.values.len(), 3);
        assert!(matches!(
            arena.ir.values[1].kind,
            ProcessValueKind::Binary {
                operation: ProcessBinaryOp::SignedMul,
                left: ProcessValueId(0),
                right: ProcessValueId(0),
            }
        ));
        assert!(matches!(
            arena.ir.values[2].kind,
            ProcessValueKind::Unary {
                operation: ProcessUnaryOp::Neg,
                operand: ProcessValueId(1),
            }
        ));
        for index in [1, 2] {
            assert_eq!(arena.ir.values[index].span, span);
            assert_eq!(
                arena.ir.values[index].bit_width, None,
                "consumer supplies the width"
            );
            assert_eq!(arena.reads[index].as_ref(), &[SignalId(0)]);
        }
        assert!(arena.ir.validate(1).is_empty());
    }

    #[test]
    fn logic_literal_normalization_preserves_canonical_nodes_and_formats() {
        let span = crate::diag::Span::new(crate::diag::FileId(0), 4..9);
        let mut arena = SourceValues::default();
        let literal = arena.import_test_fragment(&Expr::Logic('Q'), span, None);
        let layout = SourceLayout {
            span,
            kind: LayoutKind::Scalar {
                width: 5,
                domain: ScalarDomain::Enum("TestLogic".into()),
                nominal: Some("TestLogic".into()),
                value_range: None,
            },
        };
        let root = arena.push_node(
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(5),
                kind: ProcessValueKind::Index {
                    base: literal,
                    index: literal,
                },
            },
            Some(layout.clone()),
        );
        arena.set_explicit_meta(root, Expr::Logic('Q'));
        arena.meta_width.insert((root, 5), 5);
        let original = arena.ir.values[root.0 as usize].clone();
        arena.normalize_logic_literals(&HashMap::from([("'Q'".into(), 23)]));
        assert_eq!(arena.ir.values.len(), 3);
        assert_eq!(arena.ir.values[root.0 as usize], original);
        assert_eq!(arena.ir.value_layouts[root.0 as usize], Some(layout));
        assert_eq!(
            arena.ir.values[literal.0 as usize].kind,
            ProcessValueKind::Number(ProcessNumber::Integer(vec![23]))
        );
        let Expr::Canonical { value: meta, .. } = arena.explicit_meta[&root] else {
            panic!("literal plane must be canonical before normalization");
        };
        assert_eq!(arena.ir.values[meta.0 as usize].span, span);
        assert!(matches!(arena.node(meta), Expr::Const(23)));
        assert!(arena.meta_width.is_empty());
        assert!(arena.ir.validate(0).is_empty());
    }

    #[test]
    fn canonical_projection_compacts_without_expression_roundtrips() {
        let span = crate::diag::Span::new(crate::diag::FileId(0), 4..9);
        let layout = SourceLayout {
            span,
            kind: LayoutKind::Scalar {
                width: 4,
                domain: ScalarDomain::Enum("Logic".into()),
                nominal: Some("Logic".into()),
                value_range: None,
            },
        };
        let mut arena = SourceValues::default();
        arena.import_test_fragment(&Expr::Const(99), span, None);
        let base = arena.import_test_fragment(&Expr::Current(SignalId(0)), span, None);
        let index = arena.import_test_fragment(&Expr::Const(2), span, None);
        let selected = arena.push_node(
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(4),
                kind: ProcessValueKind::Index { base, index },
            },
            Some(layout.clone()),
        );
        let condition = arena.import_test_fragment(&Expr::Const(1), span, None);
        let root = arena.push_node(
            ProcessValue {
                span,
                ty: None,
                bit_width: Some(4),
                kind: ProcessValueKind::Select {
                    condition,
                    then_value: selected,
                    else_value: selected,
                },
            },
            Some(layout.clone()),
        );
        let mut draft = HardwareDraft::default();
        draft.drivers.push(Driver {
            target: SignalId(0),
            cond: None,
            expr: arena.reference(root),
            meta: None,
            ctx: 0,
            span: Some(span),
        });
        arena.retain_reachable(&mut draft);
        assert_eq!(arena.ir.values.len(), 5);
        assert!(matches!(
            arena.ir.values[2].kind,
            ProcessValueKind::Index {
                base: ProcessValueId(0),
                index: ProcessValueId(1)
            }
        ));
        assert!(matches!(
            arena.ir.values[4].kind,
            ProcessValueKind::Select {
                condition: ProcessValueId(3),
                then_value: ProcessValueId(2),
                else_value: ProcessValueId(2)
            }
        ));
        for node in [2, 4] {
            assert_eq!(arena.ir.values[node].span, span);
            assert_eq!(arena.ir.values[node].bit_width, Some(4));
            assert_eq!(arena.ir.value_layouts[node], Some(layout.clone()));
            assert_eq!(arena.reads[node].as_ref(), &[SignalId(0)]);
        }
        assert!(
            arena.ir.validate(1).is_empty(),
            "{:?}",
            arena.ir.validate(1)
        );
    }

    #[test]
    fn concrete_local_formats_survive_reconstruction_and_reachability_compaction() {
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
        arena.import_test_fragment(&Expr::Const(99), span, None); // deliberately unreachable
        let local = arena.bind_layout(&Expr::Current(SignalId(0)), layout.clone(), span);
        let scalar = arena.bind_scalar(local, Some(crate::types::Ty::Integer), span);
        assert_eq!(arena.ir.value_layouts.len(), arena.ir.values.len());
        let mapped =
            arena.reconstruct_metavalues(&HashMap::new(), &HashMap::new(), &HashMap::new());
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
        let stride = arena.import_test_fragment(
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
        let unbound = lookup(stride);
        let bound = lookup(bound);
        let unbound_id = arena.import_test_fragment(&unbound, span, None);
        let bound_id = arena.import_test_fragment(&bound, span, None);
        let bound_node = arena.ir.values[bound_id.0 as usize].clone();
        let mut tables = Vec::new();
        arena.compact_lookups(&mut tables, &mut HashMap::new());
        assert_eq!(tables.len(), 1);
        assert!(matches!(
            arena.ir.values[unbound_id.0 as usize].kind,
            ProcessValueKind::TableLookup { .. }
        ));
        assert_eq!(arena.ir.values[bound_id.0 as usize], bound_node);
    }

    #[test]
    fn captured_literal_planes_survive_reconstruction_and_compaction() {
        let span = crate::diag::Span::new(crate::diag::FileId(0), 0..1);
        let mut arena = SourceValues::default();
        arena.import_test_fragment(&Expr::Const(99), span, None);
        let value = arena.import_test_fragment(&Expr::Const(8), span, None);
        let metadata = arena.import_test_fragment(&Expr::Const(0x120), span, None);
        arena.set_explicit_meta(value, arena.reference(metadata));
        assert!(arena.may_have_meta(value, &HashMap::new()));
        let mapped =
            arena.reconstruct_metavalues(&HashMap::new(), &HashMap::new(), &HashMap::new());
        let value = mapped[value.0 as usize];
        assert!(arena.may_have_meta(value, &HashMap::new()));
        let mut draft = HardwareDraft::default();
        draft.drivers.push(Driver {
            span: Some(span),
            target: SignalId(0),
            cond: None,
            expr: arena.reference(value),
            meta: None,
            ctx: 0,
        });
        arena.retain_reachable(&mut draft);
        assert_eq!(arena.ir.values.len(), 2);
        let Expr::Canonical { value, .. } = draft.drivers[0].expr else {
            panic!("canonical value expected");
        };
        assert!(arena.may_have_meta(value, &HashMap::new()));
        let Expr::Canonical {
            value: metadata, ..
        } = arena.explicit_meta[&value]
        else {
            panic!("canonical plane expected");
        };
        assert!(matches!(arena.node(metadata), Expr::Const(0x120)));
    }
}
