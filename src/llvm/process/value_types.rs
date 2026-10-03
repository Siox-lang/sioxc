//! The per-block value cache, and signedness, layout, and constant queries on
//! Process values.

use super::*;

/// Per-function value cache. Pure subgraphs use one entry regardless of the
/// enclosing branch predicate; a subgraph containing `CheckedIndex` includes
/// that predicate in its key so a shared arena node can latch independently
/// on different source control-flow paths without cloning the whole cache.
pub(super) struct ProcessValueCache<'ctx, 'checks> {
    pub(super) emitted:
        HashMap<(ProcessValueId, Option<IntValue<'ctx>>, Option<u32>), IntValue<'ctx>>,
    pub(super) checked: &'checks [bool],
    pub(super) meta_free: &'checks [bool],
}

impl<'ctx, 'checks> ProcessValueCache<'ctx, 'checks> {
    pub(super) fn new(checked: &'checks [bool], meta_free: &'checks [bool]) -> Self {
        Self {
            emitted: HashMap::new(),
            checked,
            meta_free,
        }
    }

    pub(super) fn contains_check(&self, value: ProcessValueId) -> bool {
        self.checked.get(value.0 as usize).copied().unwrap_or(false)
    }

    pub(super) fn key(
        &self,
        value: ProcessValueId,
        active: Option<IntValue<'ctx>>,
        layout_width: Option<u32>,
    ) -> (ProcessValueId, Option<IntValue<'ctx>>, Option<u32>) {
        (
            value,
            self.contains_check(value).then_some(active).flatten(),
            layout_width,
        )
    }

    pub(super) fn clear(&mut self) {
        self.emitted.clear();
    }
}

/// Emit an arena value and convert it to the operation's contextual width.
#[allow(clippy::too_many_arguments)]
pub(super) fn process_value_at<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    id: ProcessValueId,
    width: u32,
    signed: bool,
    active: Option<IntValue<'ctx>>,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<IntValue<'ctx>> {
    let value = design.process_ir.values.get(id.0 as usize)?;
    let natural_width = value.bit_width?;
    // Arithmetic is evaluated in the width supplied by its consumer, not
    // necessarily in the minimum width recorded on the arena node. This is
    // observable for kernel integers: evaluating `0 - 7` as i3 first turns it
    // into `1`, and extending that result to the extern-C i64 ABI cannot
    // recover `-7`. The established digital emitter has the same `emit_at`
    // rule. Preserve explicit narrowing by taking this path only when a
    // consumer widens the expression.
    if width > natural_width {
        match &value.kind {
            ProcessValueKind::Unary {
                operation: ProcessUnaryOp::Neg,
                operand,
            } if !process_value_is_real(design, id) => {
                let operand = process_value_at(
                    context,
                    module,
                    builder,
                    design,
                    *operand,
                    width,
                    signed,
                    active,
                    index_sites,
                    cache,
                )?;
                return builder.build_int_neg(operand, "pv.context.neg").ok();
            }
            ProcessValueKind::Binary {
                operation,
                left,
                right,
            } => {
                return process_binary(
                    context,
                    module,
                    builder,
                    design,
                    operation,
                    *left,
                    *right,
                    width,
                    active,
                    index_sites,
                    cache,
                );
            }
            ProcessValueKind::Select {
                condition,
                then_value,
                else_value,
            } => {
                return process_select(
                    context,
                    module,
                    builder,
                    design,
                    *condition,
                    *then_value,
                    *else_value,
                    width,
                    active,
                    index_sites,
                    cache,
                );
            }
            _ => {}
        }
    }
    let value = process_value(
        context,
        module,
        builder,
        design,
        id,
        active,
        index_sites,
        cache,
    )?;
    if signed && process_value_is_signed(design, id) {
        fit_signed(builder, value, width)
    } else {
        fit(builder, value, width)
    }
}

/// Emit a scalar select at its consumer's width while retaining the activity
/// predicates that make checked expressions in an unselected arm inert.
#[allow(clippy::too_many_arguments)]
pub(super) fn process_select<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    condition: ProcessValueId,
    then_value: ProcessValueId,
    else_value: ProcessValueId,
    width: u32,
    active: Option<IntValue<'ctx>>,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<IntValue<'ctx>> {
    let condition = process_value(
        context,
        module,
        builder,
        design,
        condition,
        active,
        index_sites,
        cache,
    )?;
    let condition = as_condition(builder, condition)?;
    let checked_arms = cache.contains_check(then_value) || cache.contains_check(else_value);
    let (then_active, else_active) = if !checked_arms {
        (None, None)
    } else {
        let then_active = match active {
            Some(outer) => Some(
                builder
                    .build_and(outer, condition, "pv.select.then.active")
                    .ok()?,
            ),
            None => Some(condition),
        };
        let not_selected = builder.build_not(condition, "pv.select.not").ok()?;
        let else_active = match active {
            Some(outer) => Some(
                builder
                    .build_and(outer, not_selected, "pv.select.else.active")
                    .ok()?,
            ),
            None => Some(not_selected),
        };
        (then_active, else_active)
    };
    let then_value = process_value_at(
        context,
        module,
        builder,
        design,
        then_value,
        width,
        true,
        then_active,
        index_sites,
        cache,
    )?;
    let else_value = process_value_at(
        context,
        module,
        builder,
        design,
        else_value,
        width,
        true,
        else_active,
        index_sites,
        cache,
    )?;
    builder
        .build_select(condition, then_value, else_value, "pv.select")
        .ok()
        .map(|value| value.into_int_value())
}

/// Whether a scalar arena value denotes a signed mathematical result rather
/// than merely carrying a bit pattern whose high bit happens to be set.
///
/// This distinction matters when a signed operation widens an operand. The
/// minimum representation of the positive literal `3` is `i2 3`, but it must
/// zero-extend to `i65 3`; blindly sign-extending it would turn it into `-1`.
/// Conversely, a negative-capable kernel-integer signal and the result of a
/// signed arithmetic operation must preserve their sign. This mirrors the
/// established digital emitter's contextual signed-operand rules.
pub(super) fn process_value_is_signed(design: &Design, id: ProcessValueId) -> bool {
    let Some(value) = design.process_ir.values.get(id.0 as usize) else {
        return false;
    };
    let event = matches!(
        value.kind,
        ProcessValueKind::Signal {
            state: ProcessSignalState::Event,
            ..
        }
    );
    if !event {
        if let Some(layout) = process_value_layout(design, id) {
            match layout.kind {
                LayoutKind::Scalar {
                    domain: siox::ir::ScalarDomain::Integer,
                    ..
                }
                | LayoutKind::Packed { .. } => return process_layout_is_signed(layout),
                _ => {}
            }
        }
        if value.ty.as_ref().is_some_and(process_type_is_signed) {
            return true;
        }
    }
    match &value.kind {
        ProcessValueKind::Signal { signals, state } => {
            let [signal] = signals.as_slice() else {
                return false;
            };
            !matches!(state, ProcessSignalState::Event)
                && design.signals.get(signal.0 as usize).is_some_and(|signal| {
                    signal.integer && signal.range.map(|(left, _)| left < 0).unwrap_or(true)
                })
        }
        ProcessValueKind::Local { .. }
        | ProcessValueKind::Storage(_)
        | ProcessValueKind::RawResize { .. } => false,
        ProcessValueKind::Unary { operation, .. } => matches!(
            operation,
            ProcessUnaryOp::Neg | ProcessUnaryOp::RealToInteger
        ),
        ProcessValueKind::Binary { operation, .. } => matches!(
            operation,
            ProcessBinaryOp::SignedAdd
                | ProcessBinaryOp::SignedSub
                | ProcessBinaryOp::SignedMul
                | ProcessBinaryOp::SignedDiv
                | ProcessBinaryOp::ArithmeticShr
        ),
        ProcessValueKind::Select {
            then_value,
            else_value,
            ..
        } => {
            process_value_is_signed(design, *then_value)
                || process_value_is_signed(design, *else_value)
        }
        ProcessValueKind::Match { arms, .. } => arms
            .iter()
            .any(|arm| process_value_is_signed(design, arm.value)),
        ProcessValueKind::CheckedIndex { index, .. } => process_value_is_signed(design, *index),
        ProcessValueKind::ForeignCall { integer_result, .. } => *integer_result,
        _ => false,
    }
}

pub(super) fn process_value_is_real(design: &Design, id: ProcessValueId) -> bool {
    let Some(value) = design.process_ir.values.get(id.0 as usize) else {
        return false;
    };
    if matches!(value.ty, Some(siox::types::Ty::Real))
        || process_value_layout(design, id).is_some_and(|layout| {
            matches!(
                layout.kind,
                LayoutKind::Scalar {
                    domain: siox::ir::ScalarDomain::Real,
                    ..
                }
            )
        })
    {
        return true;
    }
    match &value.kind {
        ProcessValueKind::Number(ProcessNumber::Real(_)) => true,
        ProcessValueKind::Unary {
            operation: ProcessUnaryOp::Neg,
            operand,
        } => process_value_is_real(design, *operand),
        ProcessValueKind::Binary { operation, .. } => matches!(
            operation,
            ProcessBinaryOp::FloatAdd
                | ProcessBinaryOp::FloatSub
                | ProcessBinaryOp::FloatMul
                | ProcessBinaryOp::FloatDiv
        ),
        ProcessValueKind::Select {
            then_value,
            else_value,
            ..
        } => {
            process_value_is_real(design, *then_value) || process_value_is_real(design, *else_value)
        }
        ProcessValueKind::ForeignCall { float_result, .. } => *float_result,
        ProcessValueKind::HostCall {
            operation: ProcessHostValueOp::Uniform,
            ..
        } => true,
        _ => false,
    }
}

pub(super) fn process_type_is_signed(ty: &siox::types::Ty) -> bool {
    matches!(ty, siox::types::Ty::Integer)
        || matches!(
            ty,
            siox::types::Ty::Array {
                family: Some(family),
                ..
            } if family.rsplit("::").next() == Some("signed")
        )
}

pub(super) fn process_layout_is_signed(layout: &SourceLayout) -> bool {
    match &layout.kind {
        LayoutKind::Scalar {
            domain: siox::ir::ScalarDomain::Integer,
            value_range,
            ..
        } => value_range.is_none_or(|(left, _)| left < 0),
        LayoutKind::Packed { family, .. } => family.rsplit("::").next() == Some("signed"),
        _ => false,
    }
}

pub(super) fn layout_for_type<'a>(
    design: &'a Design,
    ty: &siox::types::Ty,
) -> Option<&'a SourceLayout> {
    let mut candidates = design
        .process_ir
        .storages
        .iter()
        .filter(|storage| storage.ty.as_ref() == Some(ty))
        .filter_map(|storage| storage.layout.as_ref())
        .chain(
            design
                .process_ir
                .processes
                .iter()
                .flat_map(|process| &process.locals)
                .filter(|local| local.ty.as_ref() == Some(ty))
                .filter_map(|local| local.layout.as_ref()),
        );
    let first = candidates.next()?;
    candidates
        .all(|candidate| candidate == first)
        .then_some(first)
}

pub(super) fn signal_layout<'a>(
    design: &'a Design,
    signals: &[SignalId],
) -> Option<&'a SourceLayout> {
    let first = design.signals.get(signals.first()?.0 as usize)?;
    let mut candidates = design
        .source_layouts
        .iter()
        .filter(|(path, _)| {
            first.path == **path
                || first
                    .path
                    .strip_prefix(path.as_str())
                    .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with('['))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
    candidates.into_iter().find_map(|(root, layout)| {
        let suffix = first.path.strip_prefix(root)?;
        let mut prefixes = vec![""];
        let mut offset = 0usize;
        while offset < suffix.len() {
            let rest = &suffix[offset..];
            let consumed = if let Some(field) = rest.strip_prefix('.') {
                1 + field.find(['.', '[']).unwrap_or(field.len())
            } else if let Some(index) = rest.strip_prefix('[') {
                1 + index.find(']')? + 1
            } else {
                return None;
            };
            offset = offset.checked_add(consumed)?;
            prefixes.push(&suffix[..offset]);
        }
        prefixes.into_iter().rev().find_map(|projection| {
            let path = format!("{root}{projection}");
            let field = format!("{path}.");
            let element = format!("{path}[");
            let leaves = design
                .signals
                .iter()
                .enumerate()
                .filter(|(_, signal)| {
                    signal.path == path
                        || signal.path.starts_with(&field)
                        || signal.path.starts_with(&element)
                })
                .filter_map(|(index, _)| u32::try_from(index).ok())
                .filter(|index| {
                    !design.meta_of.values().any(|companion| companion == index)
                        && !design.metavalue_temps.contains(index)
                })
                .map(SignalId)
                .collect::<Vec<_>>();
            (leaves == signals)
                .then(|| projection_slice(layout, projection).map(|slice| slice.layout))
                .flatten()
        })
    })
}

/// Recursive layout carried by one arena value. This is intentionally based
/// only on finalized IR metadata: backends never consult source syntax.
pub(super) fn process_value_layout(design: &Design, id: ProcessValueId) -> Option<&SourceLayout> {
    if let Some(layout) = design
        .process_ir
        .value_layouts
        .get(id.0 as usize)
        .and_then(Option::as_ref)
    {
        return Some(layout);
    }
    let value = design.process_ir.values.get(id.0 as usize)?;
    match &value.kind {
        ProcessValueKind::Storage(storage) => {
            let storage = design.process_ir.storages.get(storage.0 as usize)?;
            storage.layout.as_ref().or_else(|| {
                storage
                    .ty
                    .as_ref()
                    .and_then(|ty| layout_for_type(design, ty))
            })
        }
        ProcessValueKind::StorageState {
            storage,
            state: ProcessSignalState::Old,
        } => {
            let storage = design.process_ir.storages.get(storage.0 as usize)?;
            storage.layout.as_ref().or_else(|| {
                storage
                    .ty
                    .as_ref()
                    .and_then(|ty| layout_for_type(design, ty))
            })
        }
        ProcessValueKind::StorageState { .. } => None,
        ProcessValueKind::Local { process, local } => {
            let local = design
                .process_ir
                .processes
                .get(process.0 as usize)?
                .locals
                .get(local.0 as usize)?;
            local
                .layout
                .as_ref()
                .or_else(|| local.ty.as_ref().and_then(|ty| layout_for_type(design, ty)))
        }
        ProcessValueKind::Signal { signals, .. } => signal_layout(design, signals),
        ProcessValueKind::Field { base, field } => {
            field_slice(process_value_layout(design, *base)?, field).map(|slice| slice.layout)
        }
        ProcessValueKind::Index { base, index } => {
            let layout = process_value_layout(design, *base)?;
            let LayoutKind::Array { element, .. } = &layout.kind else {
                return None;
            };
            if let Some(index) = process_constant_i64(design, *index) {
                array_slice(layout, index).map(|slice| slice.layout)
            } else {
                Some(element)
            }
        }
        ProcessValueKind::Select {
            then_value,
            else_value,
            ..
        } => {
            let then_layout = process_value_layout(design, *then_value)?;
            (process_value_layout(design, *else_value) == Some(then_layout)).then_some(then_layout)
        }
        ProcessValueKind::Construct { ty, .. } => value
            .ty
            .as_ref()
            .or(ty.as_ref())
            .and_then(|ty| layout_for_type(design, ty)),
        ProcessValueKind::Array(_) => value.ty.as_ref().and_then(|ty| layout_for_type(design, ty)),
        _ => value.ty.as_ref().and_then(|ty| layout_for_type(design, ty)),
    }
}

/// Width to use while emitting one value through an expected source layout.
///
/// Recursive arrays and structs are represented as one packed Process value,
/// so their canonical width belongs to the value itself. The layout remains
/// necessary for field/element offsets, but must not manufacture a missing
/// aggregate width. Scalar and packed-vector layouts may still provide a
/// consumer width for the ordinary widening/coercion path.
pub(super) fn process_value_width_in_layout(
    design: &Design,
    id: ProcessValueId,
    layout: &SourceLayout,
) -> Option<u32> {
    match layout.kind {
        LayoutKind::Array { .. } | LayoutKind::Struct { .. } => {
            design.process_ir.values.get(id.0 as usize)?.bit_width
        }
        LayoutKind::Scalar { .. } | LayoutKind::Packed { .. } | LayoutKind::Opaque { .. } => {
            layout_width(layout)
        }
    }
}

/// Evaluate a source-layout attribute without consulting the AST. These are
/// elaboration metadata, so direct process code materializes a constant rather
/// than calling the simulation runtime.
pub(super) fn process_layout_attribute(
    design: &Design,
    base: ProcessValueId,
    attribute: &str,
) -> Option<u64> {
    if attribute == "length" {
        if let Some(value) = design.process_ir.values.get(base.0 as usize) {
            if let Some(siox::types::Ty::Array { len, .. }) = value.ty.as_ref() {
                return (*len != 0)
                    .then_some(u64::from(*len))
                    .or_else(|| value.bit_width.map(u64::from));
            }
        }
    }
    let layout = process_value_layout(design, base)?;
    if attribute == "length" {
        return match &layout.kind {
            LayoutKind::Array {
                range: Some(range), ..
            } => range.len(),
            LayoutKind::Packed { width, .. } | LayoutKind::Scalar { width, .. } => {
                Some(u64::from(*width))
            }
            LayoutKind::Opaque { width, .. } => width.map(u64::from),
            LayoutKind::Struct { .. } | LayoutKind::Array { range: None, .. } => None,
        };
    }
    let range = layout.index_range()?;
    let signed = match attribute {
        "left" => range.left,
        "right" => range.right,
        "high" => range.left.max(range.right),
        "low" => range.left.min(range.right),
        "ascending" => return process_bool_discriminant(design, range.ascending()),
        _ => return None,
    };
    Some(u64::from_ne_bytes(signed.to_ne_bytes()))
}

/// Resolve a Boolean through the elaborated std enum table. The compiler may
/// name `Bool`, but its source declaration owns both discriminants.
pub(super) fn process_bool_discriminant(design: &Design, value: bool) -> Option<u64> {
    let symbol = if value { "true" } else { "false" };
    design
        .enum_syms
        .get("core::primitive::Bool")
        .or_else(|| design.enum_syms.get("Bool"))?
        .iter()
        .find_map(|(discriminant, candidate)| (candidate == symbol).then_some(*discriminant))
}

pub(super) fn process_constant_i64(design: &Design, id: ProcessValueId) -> Option<i64> {
    match &design.process_ir.values.get(id.0 as usize)?.kind {
        ProcessValueKind::Number(ProcessNumber::Integer(words)) => {
            let [word] = words.as_slice() else {
                return None;
            };
            Some(*word as i64)
        }
        ProcessValueKind::Unary {
            operation: ProcessUnaryOp::Neg,
            operand,
        } => process_constant_i64(design, *operand)?.checked_neg(),
        _ => None,
    }
}

/// Convert an arbitrary-width scalar to the one-bit condition domain.
pub(super) fn as_condition<'ctx>(
    builder: &Builder<'ctx>,
    value: IntValue<'ctx>,
) -> Option<IntValue<'ctx>> {
    if value.get_type().get_bit_width() == 1 {
        Some(value)
    } else {
        builder
            .build_int_compare(
                IntPredicate::NE,
                value,
                value.get_type().const_zero(),
                "pv.condition",
            )
            .ok()
    }
}
