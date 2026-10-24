//! Native source frames over the same canonical expressions as ordinary builds.

use super::*;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::basic_block::BasicBlock;
use inkwell::debug_info::DILocalVariable;
use inkwell::types::{
    AnyType, BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType, StructType,
};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, StructValue};
use siox::ir::ProcessValueInline;
use std::collections::HashSet;

type AggregateMembers = HashMap<ProcessValueId, (ProcessValueId, u32, u32)>;

/// Executable return ABI, separate from the declaration's DWARF type.
pub(in crate::llvm::process) enum SourceReturn<'ctx> {
    Direct(BasicTypeEnum<'ctx>),
    Register {
        native: BasicTypeEnum<'ctx>,
        layout: SourceLayout,
    },
    Memory {
        storage: StructType<'ctx>,
        view: SourceReturnView<'ctx>,
    },
}

pub(in crate::llvm::process) enum SourceReturnView<'ctx> {
    Packed(SourceLayout),
    Literal(StructValue<'ctx>),
    String {
        query: FunctionValue<'ctx>,
        root: u32,
    },
}

impl<'ctx> SourceReturn<'ctx> {
    fn memory(
        debug: &DebugInfo<'ctx, '_>,
        width: u32,
        bytes: u32,
        view: SourceReturnView<'ctx>,
    ) -> Option<Self> {
        let bits = debug
            .context
            .custom_width_int_type(std::num::NonZeroU32::new(width)?)
            .ok()?;
        Some(Self::Memory {
            storage: debug.context.struct_type(
                &[
                    debug.context.i8_type().array_type(bytes).into(),
                    bits.into(),
                ],
                false,
            ),
            view,
        })
    }

    pub(in crate::llvm::process) fn fn_type(
        &self,
        context: &'ctx Context,
        parameters: &[BasicMetadataTypeEnum<'ctx>],
    ) -> FunctionType<'ctx> {
        match self {
            Self::Direct(ty) | Self::Register { native: ty, .. } => ty.fn_type(parameters, false),
            Self::Memory { .. } => {
                let parameters = std::iter::once(context.ptr_type(AddressSpace::default()).into())
                    .chain(parameters.iter().copied())
                    .collect::<Vec<_>>();
                context.void_type().fn_type(&parameters, false)
            }
        }
    }

    pub(in crate::llvm::process) fn parameter_offset(&self) -> u32 {
        u32::from(matches!(self, Self::Memory { .. }))
    }

    pub(in crate::llvm::process) fn declare(
        &self,
        context: &'ctx Context,
        function: FunctionValue<'ctx>,
        debug: Option<&DebugInfo<'ctx, '_>>,
    ) {
        if let (Self::Register { layout, .. }, Some(debug)) = (self, debug) {
            debug
                .register_returns
                .borrow_mut()
                .insert(function, layout.clone());
        }
        if let Self::Memory { storage, .. } = self {
            function.add_attribute(
                AttributeLoc::Param(0),
                context.create_type_attribute(
                    Attribute::get_named_enum_kind_id("sret"),
                    storage.as_any_type_enum(),
                ),
            );
        }
    }

    pub(in crate::llvm::process) fn emit_return(
        &self,
        debug: Option<&DebugInfo<'ctx, '_>>,
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
    ) -> Option<()> {
        match self {
            Self::Direct(_) => {
                let returned = return_value(builder, value)?;
                builder.build_return(Some(&returned)).ok()?;
            }
            Self::Register { layout, .. } => {
                let value = register_bits(builder, value, layout, true)?;
                let returned = return_value(builder, value)?;
                builder.build_return(Some(&returned)).ok()?;
            }
            Self::Memory { storage, view } => {
                if value.get_type().as_basic_type_enum() != storage.get_field_type_at_index(1)? {
                    return None;
                }
                let output = builder
                    .get_insert_block()?
                    .get_parent()?
                    .get_first_param()?
                    .into_pointer_value();
                // Source views are written from the already-emitted value;
                // only the separate canonical payload is executable data.
                match view {
                    SourceReturnView::Packed(layout) => {
                        debug?.copy_fields(builder, output, value, layout, &mut 0, &mut 0);
                    }
                    SourceReturnView::Literal(view) => {
                        builder
                            .build_store(output, *view)
                            .ok()?
                            .set_alignment(1)
                            .ok()?;
                    }
                    SourceReturnView::String { query, root } => {
                        debug?.capture_string(
                            builder,
                            *query,
                            *root,
                            fit(builder, value, 64)?,
                            output,
                        );
                    }
                }
                let payload = builder
                    .build_struct_gep(*storage, output, 1, "debug.return.payload")
                    .ok()?;
                builder
                    .build_store(payload, value)
                    .ok()?
                    .set_alignment(1)
                    .ok()?;
                builder.build_return(None).ok()?;
            }
        }
        Some(())
    }
}

/// Both inline and shared callers consume canonical bits, never inspection
/// bytes. The sret attribute describes our native carrier's actual layout.
pub(in crate::llvm::process) fn call_return<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    function: FunctionValue<'ctx>,
    arguments: &[BasicMetadataValueEnum<'ctx>],
    name: &str,
    debug: Option<&DebugInfo<'ctx, '_>>,
) -> Option<IntValue<'ctx>> {
    if let Some(attribute) = function.get_enum_attribute(
        AttributeLoc::Param(0),
        Attribute::get_named_enum_kind_id("sret"),
    ) {
        let ty = attribute.get_type_value().into_struct_type();
        let bytes = ty.get_field_type_at_index(0)?.into_array_type().len();
        let width = ty
            .get_field_type_at_index(1)?
            .into_int_type()
            .get_bit_width();
        // Rebuild in this context: Attribute's type accessor has a short Rust
        // borrow lifetime, while the LLVM type belongs to the whole context.
        let bits = context
            .custom_width_int_type(std::num::NonZeroU32::new(width)?)
            .ok()?;
        let storage = context.struct_type(
            &[context.i8_type().array_type(bytes).into(), bits.into()],
            false,
        );
        let entry = builder
            .get_insert_block()?
            .get_parent()?
            .get_first_basic_block()?;
        let allocation = context.create_builder();
        if let Some(first) = entry.get_first_instruction() {
            allocation.position_before(&first);
        } else {
            allocation.position_at_end(entry);
        }
        // One slot per static call site, not one alloca per runtime loop turn.
        let output = allocation
            .build_alloca(storage, "debug.return.storage")
            .ok()?;
        let arguments = std::iter::once(output.into())
            .chain(arguments.iter().copied())
            .collect::<Vec<_>>();
        let call = builder.build_call(function, &arguments, "").ok()?;
        call.add_attribute(AttributeLoc::Param(0), attribute);
        let payload = builder
            .build_struct_gep(storage, output, 1, "debug.return.payload")
            .ok()?;
        let value = builder.build_load(bits, payload, name).ok()?;
        value.as_instruction_value()?.set_alignment(1).ok()?;
        Some(value.into_int_value())
    } else {
        let value = builder
            .build_call(function, arguments, name)
            .ok()?
            .try_as_basic_value()
            .basic()
            .and_then(|value| return_bits(builder, value))?;
        if let Some(layout) =
            debug.and_then(|debug| debug.register_returns.borrow().get(&function).cloned())
        {
            register_bits(builder, value, &layout, false)
        } else {
            Some(value)
        }
    }
}

/// Retain one native source boundary for jointly evaluable aggregate leaves.
/// No binding metadata or discarded field is made an executable dependency.
pub(super) fn aggregate_returns(
    design: &Design,
) -> (HashMap<ProcessValueId, AggregateReturn>, AggregateMembers) {
    let mut pure = Vec::with_capacity(design.process_ir.values.len());
    let mut emittable = Vec::with_capacity(design.process_ir.values.len());
    let mut needs_order = Vec::with_capacity(design.process_ir.values.len());
    let mut groups: HashMap<_, Vec<_>> = HashMap::new();
    for (index, value) in design.process_ir.values.iter().enumerate() {
        // Total arithmetic may be grouped freely. Effectful calls and checks
        // need joint demand and an uninterrupted executable field order.
        let safe = matches!(
            value.kind,
            ProcessValueKind::Number(_)
                | ProcessValueKind::BitString { .. }
                | ProcessValueKind::Char(_)
                | ProcessValueKind::Signal { .. }
                | ProcessValueKind::Parameter { .. }
                | ProcessValueKind::Array(_)
                | ProcessValueKind::Construct { .. }
                | ProcessValueKind::Field { .. }
                | ProcessValueKind::RawResize { .. }
                | ProcessValueKind::BitSlice { .. }
                | ProcessValueKind::PackedSlice { .. }
                | ProcessValueKind::Select { .. }
                | ProcessValueKind::Unary {
                    operation: ProcessUnaryOp::Neg | ProcessUnaryOp::Not,
                    ..
                }
        ) || matches!(
            value.kind,
            ProcessValueKind::Binary { ref operation, .. }
                if !matches!(operation, ProcessBinaryOp::Custom(_))
        );
        pure.push(
            safe && !siox::ir::any_process_value_dependency(&value.kind, |id| {
                !pure.get(id.0 as usize).copied().unwrap_or(false)
            }),
        );
        emittable.push(
            (safe
                || matches!(
                    value.kind,
                    ProcessValueKind::InlineCall { .. }
                        | ProcessValueKind::CheckedIndex { .. }
                        | ProcessValueKind::Index { .. }
                        | ProcessValueKind::ForeignCall { .. }
                        | ProcessValueKind::HostCall { .. }
                ))
                && !siox::ir::any_process_value_dependency(&value.kind, |id| {
                    !emittable.get(id.0 as usize).copied().unwrap_or(false)
                }),
        );
        needs_order.push(
            matches!(
                value.kind,
                ProcessValueKind::Index { .. }
                    | ProcessValueKind::CheckedIndex { .. }
                    | ProcessValueKind::ForeignCall { .. }
                    | ProcessValueKind::HostCall { .. }
            ) || siox::ir::any_process_value_dependency(&value.kind, |id| {
                needs_order.get(id.0 as usize).copied().unwrap_or(false)
            }),
        );
        if let ProcessValueKind::InlineCall { source, .. } = &value.kind {
            if let Some((expansion, _)) = &source.aggregate {
                groups
                    .entry((*expansion, source.function, source.call))
                    .or_default()
                    .push(ProcessValueId(index as u32));
            }
        }
    }
    let mut returns = HashMap::new();
    let mut members = HashMap::new();
    for group in groups.into_values().filter(|group| group.len() > 1) {
        let leader = group[0];
        let ProcessValueKind::InlineCall { source, .. } =
            &design.process_ir.values[leader.0 as usize].kind
        else {
            continue;
        };
        let Some(layout) = &source.result_layout else {
            continue;
        };
        let Some(root_width) = layout_width(layout) else {
            continue;
        };
        let pieces = group
            .iter()
            .map(|id| {
                let ProcessValueKind::InlineCall { operand, source } =
                    &design.process_ir.values[id.0 as usize].kind
                else {
                    return None;
                };
                let (_, path) = source.aggregate.as_ref()?;
                let projection = if path.starts_with('[') {
                    path.clone()
                } else {
                    format!(".{path}")
                };
                let selected = projection_slice(layout, &projection)?;
                (emittable[operand.0 as usize]
                    && design.process_ir.values[operand.0 as usize].bit_width
                        == Some(selected.width))
                .then(|| (*operand, selected.offset, selected.layout.clone()))
            })
            .collect::<Option<Vec<_>>>();
        let Some(mut pieces) = pieces else { continue };
        // Reject overlapping source regions. Missing fields need a compact
        // private carrier and source fragments, never zero-filled source bits.
        let mut regions = pieces
            .iter()
            .map(|(_, offset, layout)| (*offset, layout_width(layout).unwrap()))
            .collect::<Vec<_>>();
        regions.sort_unstable();
        let covered = regions.iter().try_fold(0u32, |end, (offset, width)| {
            (*offset >= end)
                .then(|| offset.checked_add(*width))
                .flatten()
        });
        if covered.is_none_or(|end| end > root_width) {
            continue;
        }
        let retained = regions
            .iter()
            .try_fold(0u32, |sum, (_, width)| sum.checked_add(*width));
        let partial = retained != Some(root_width);
        if retained.is_none() || (partial && !joint_aggregate_demand(design, &group)) {
            continue;
        }
        let order = if pieces
            .iter()
            .any(|(operand, _, _)| !pure[operand.0 as usize])
        {
            if !joint_aggregate_demand(design, &group) {
                continue;
            }
            if pieces
                .iter()
                .any(|(operand, _, _)| needs_order[operand.0 as usize])
            {
                let Some(order) = aggregate_evaluation_order(design, &group) else {
                    continue;
                };
                Some(order)
            } else {
                // Effect-free nested calls still execute exactly once under
                // joint demand; opposite pure branch orders need no effect
                // transport and retain their complete source return ABI.
                None
            }
        } else {
            None
        };
        let mut carrier = layout.clone();
        let mut fields = Vec::new();
        if partial {
            let mut indices = (0..pieces.len()).collect::<Vec<_>>();
            indices.sort_by_key(|index| pieces[*index].1);
            let mut offset = 0;
            let mut retained_fields = Vec::new();
            for index in indices {
                let ProcessValueKind::InlineCall { source, .. } =
                    &design.process_ir.values[group[index].0 as usize].kind
                else {
                    unreachable!();
                };
                let path = source.aggregate.as_ref().unwrap().1.clone();
                let (operand, position, layout) = &mut pieces[index];
                *position = offset;
                offset += layout_width(layout).unwrap();
                fields.push((path.clone(), Some(*operand)));
                retained_fields.push(siox::ir::LayoutField {
                    name: path,
                    direction: None,
                    layout: layout.clone(),
                });
            }
            carrier.kind = LayoutKind::Struct {
                name: "<partial return>".into(),
                view: None,
                fields: retained_fields,
            };
        }
        for (id, (_, offset, layout)) in group.iter().zip(&pieces) {
            members.insert(*id, (leader, *offset, layout_width(layout).unwrap()));
        }
        if let Some(order) = order {
            // Source construction order need not be consumer order. Preserve
            // the original field offsets while emitting in first-use order.
            pieces = order
                .iter()
                .map(|id| pieces[group.iter().position(|member| member == id).unwrap()].clone())
                .collect();
        }
        returns.insert(
            leader,
            AggregateReturn {
                layout: carrier,
                pieces,
                fields,
            },
        );
    }
    (returns, members)
}

/// Fold only the unsigned, non-overflowing selector subset whose native bits
/// equal the shared mathematical constant evaluator. Dynamic/signed values
/// and arithmetic wrapping are not a proof that a branch is taken.
fn constant_selector(design: &Design, id: ProcessValueId) -> Option<i128> {
    let value = design.process_ir.values.get(id.0 as usize)?;
    if process_value_is_signed(design, id)
        || process_value_is_real(design, id)
        || process_value_packed_width(design, id).is_some()
    {
        return None;
    }
    let folded = match &value.kind {
        ProcessValueKind::Number(ProcessNumber::Integer(_)) => {
            siox::ir::arena_constant_integer(id, &design.process_ir.values)?
        }
        ProcessValueKind::CheckedIndex { index, valid, .. }
            if constant_selector(design, *valid)? != 0 =>
        {
            constant_selector(design, *index)?
        }
        ProcessValueKind::Binary {
            operation,
            left,
            right,
        } => {
            let left = constant_selector(design, *left)?;
            let right = constant_selector(design, *right)?;
            match operation {
                ProcessBinaryOp::Eq => i128::from(left == right),
                ProcessBinaryOp::Ne => i128::from(left != right),
                ProcessBinaryOp::And => left & right,
                ProcessBinaryOp::Or => left | right,
                _ => return None,
            }
        }
        _ => return None,
    };
    let width = value.bit_width?;
    (folded >= 0 && (1..=64).contains(&width) && folded < 1i128.checked_shl(width)?)
        .then_some(folded)
}

/// The common emitter guards activity-sensitive one-bit `and` / `or` RHSs.
/// A mathematical dependency is therefore not necessarily executable demand.
fn eager_binary_right(
    design: &Design,
    operation: &ProcessBinaryOp,
    left: ProcessValueId,
    right: ProcessValueId,
) -> bool {
    if !matches!(operation, ProcessBinaryOp::And | ProcessBinaryOp::Or)
        || design.process_ir.values[left.0 as usize].bit_width != Some(1)
        || design.process_ir.values[right.0 as usize].bit_width != Some(1)
    {
        return true;
    }
    constant_selector(design, left)
        .is_some_and(|value| (value != 0) == matches!(operation, ProcessBinaryOp::And))
}

/// Every path to a field must first cross a value demanding all fields. This
/// allows whole-value selections, not independently guarded nested calls.
fn joint_aggregate_demand(design: &Design, group: &[ProcessValueId]) -> bool {
    // ponytail: one DAG pass per nested-call aggregate; share demand facts if
    // many such groups become a measured compilation bottleneck.
    let group = group.iter().copied().collect::<HashSet<_>>();
    let values = &design.process_ir.values;
    let selected = values
        .iter()
        .map(|value| {
            if let ProcessValueKind::Select {
                condition,
                then_value,
                else_value,
            } = value.kind
            {
                constant_selector(design, condition).map(|condition| {
                    if condition != 0 {
                        then_value
                    } else {
                        else_value
                    }
                })
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let mut used = vec![false; values.len()];
    for (index, value) in values.iter().enumerate() {
        if let Some(arm) = selected[index] {
            let ProcessValueKind::Select { condition, .. } = value.kind else {
                unreachable!()
            };
            used[condition.0 as usize] = true;
            used[arm.0 as usize] = true;
        } else {
            siox::ir::for_each_process_value_dependency(&value.kind, |id| {
                used[id.0 as usize] = true
            });
        }
    }
    let mut unprotected = Vec::with_capacity(values.len());
    let mut required: HashMap<ProcessValueId, HashSet<ProcessValueId>> = HashMap::new();
    for (index, value) in values.iter().enumerate() {
        let id = ProcessValueId(index as u32);
        let touches = group.contains(&id)
            || selected[index].map_or_else(
                || {
                    siox::ir::any_process_value_dependency(&value.kind, |child| {
                        unprotected[child.0 as usize]
                    })
                },
                |arm| unprotected[arm.0 as usize],
            );
        let mut demand = HashSet::new();
        if group.contains(&id) {
            demand.insert(id);
        }
        if touches {
            match &value.kind {
                ProcessValueKind::Select {
                    condition,
                    then_value,
                    else_value,
                } => {
                    if let Some(condition) = required.get(condition) {
                        demand.extend(condition);
                    }
                    if let Some(arm) = selected[index] {
                        if let Some(arm) = required.get(&arm) {
                            demand.extend(arm);
                        }
                    } else if let (Some(then), Some(els)) =
                        (required.get(then_value), required.get(else_value))
                    {
                        demand.extend(then.intersection(els));
                    }
                }
                ProcessValueKind::InlineCall { .. }
                | ProcessValueKind::RawResize { .. }
                | ProcessValueKind::BitSlice { .. }
                | ProcessValueKind::PackedSlice { .. }
                | ProcessValueKind::Unary { .. } => {
                    siox::ir::for_each_process_value_dependency(&value.kind, |child| {
                        if let Some(child) = required.get(&child) {
                            demand.extend(child);
                        }
                    });
                }
                ProcessValueKind::Binary {
                    operation,
                    left,
                    right,
                } => {
                    if let Some(left) = required.get(left) {
                        demand.extend(left);
                    }
                    if eager_binary_right(design, operation, *left, *right) {
                        if let Some(right) = required.get(right) {
                            demand.extend(right);
                        }
                    }
                }
                _ => {}
            }
        }
        let escaping = touches && demand.len() != group.len();
        unprotected.push(escaping);
        if escaping {
            if !used[index] {
                return false;
            }
            required.insert(id, demand);
        }
    }
    let protected = |id: ProcessValueId| !unprotected[id.0 as usize];
    design
        .process_ir
        .functions
        .iter()
        .all(|function| protected(function.result))
        && design
            .process_ir
            .storages
            .iter()
            .all(|storage| storage.initializer.is_none_or(protected))
        && design
            .process_ir
            .processes
            .iter()
            .flat_map(|process| &process.blocks)
            .all(|block| {
                block.instructions.iter().all(|instruction| {
                    siox::ir::process::process_instruction_values(instruction)
                        .into_iter()
                        .all(protected)
                }) && siox::ir::process::process_terminator_values(&block.terminator)
                    .into_iter()
                    .all(protected)
            })
}

/// Moving later fields into the first field's native body must not cross a
/// check, call, or mutable read. Joint demand alone does not prove this:
/// `(pair.left + foreign()) + pair.right` demands both but cannot be grouped.
fn aggregate_evaluation_order(
    design: &Design,
    group: &[ProcessValueId],
) -> Option<Vec<ProcessValueId>> {
    #[derive(Clone, Default)]
    struct Order {
        fields: Vec<ProcessValueId>,
        before: bool,
        after: bool,
    }
    impl Order {
        fn append(&mut self, other: &Self) -> Option<()> {
            if !self.fields.is_empty() && !other.fields.is_empty() && (self.after || other.before) {
                return None;
            }
            if self.fields.is_empty() {
                self.before |= other.before;
            }
            self.after = if other.fields.is_empty() {
                self.after || other.before || other.after
            } else {
                other.after
            };
            for field in &other.fields {
                if !self.fields.contains(field) {
                    self.fields.push(*field);
                }
            }
            Some(())
        }
    }
    // ponytail: one bounded field-order summary per DAG value; branch-specific
    // native bodies are needed if different paths demand different orders.
    let mut orders: Vec<Order> = Vec::with_capacity(design.process_ir.values.len());
    let mut complete = None;
    for (index, value) in design.process_ir.values.iter().enumerate() {
        let id = ProcessValueId(index as u32);
        let mut order = Order::default();
        if group.contains(&id) {
            // Its internal effects stay inside this field, in their old order.
            order.fields.push(id);
        } else if let ProcessValueKind::Select {
            condition,
            then_value,
            else_value,
        } = value.kind
        {
            order = orders[condition.0 as usize].clone();
            let then = &orders[then_value.0 as usize];
            let els = &orders[else_value.0 as usize];
            let arm = if let Some(condition) = constant_selector(design, condition) {
                if condition != 0 { then } else { els }.clone()
            } else {
                if !then.fields.is_empty() && !els.fields.is_empty() && then.fields != els.fields {
                    return None;
                }
                let mut arm = if then.fields.is_empty() { els } else { then }.clone();
                arm.before |= then.before || els.before;
                arm.after |= then.after || els.after;
                arm
            };
            order.append(&arm)?;
        } else {
            let mut valid = true;
            siox::ir::for_each_process_value_dependency(&value.kind, |child| {
                valid &= order.append(&orders[child.0 as usize]).is_some();
            });
            if !valid {
                return None;
            }
            let transparent = matches!(
                value.kind,
                ProcessValueKind::Number(_)
                    | ProcessValueKind::BitString { .. }
                    | ProcessValueKind::Char(_)
                    | ProcessValueKind::Parameter { .. }
                    | ProcessValueKind::RawResize { .. }
                    | ProcessValueKind::BitSlice { .. }
                    | ProcessValueKind::PackedSlice { .. }
                    | ProcessValueKind::Unary {
                        operation: ProcessUnaryOp::Neg | ProcessUnaryOp::Not,
                        ..
                    }
            ) || matches!(value.kind, ProcessValueKind::Binary { ref operation, .. }
                if !matches!(operation, ProcessBinaryOp::Custom(_)));
            if !transparent {
                if !order.fields.is_empty()
                    && !matches!(
                        value.kind,
                        ProcessValueKind::InlineCall { .. }
                            | ProcessValueKind::CheckedIndex { .. }
                            | ProcessValueKind::ForeignCall { .. }
                            | ProcessValueKind::HostCall { .. }
                    )
                {
                    return None;
                }
                order.after = true;
                if order.fields.is_empty() {
                    order.before = true;
                }
            }
        }
        if order.fields.len() == group.len() {
            if complete
                .as_ref()
                .is_some_and(|other| *other != order.fields)
            {
                return None;
            }
            complete = Some(order.fields.clone());
        }
        orders.push(order);
    }
    complete
}

/// Use the source ABI only at the native boundary. The common emitter
/// and its exact consumer formats continue to exchange the same integer bits.
pub(in crate::llvm::process) fn source_return_type<'ctx>(
    debug: &DebugInfo<'ctx, '_>,
    module: &Module<'ctx>,
    design: &Design,
    source: &ProcessValueInline,
    operand: ProcessValueId,
    width: u32,
    root: Option<u32>,
) -> Option<SourceReturn<'ctx>> {
    // Unbounded strings use the existing borrowed view. A declared Char[N]
    // result remains its array type, not a retagged dynamic string.
    if cfg!(all(target_arch = "x86_64", not(target_os = "windows")))
        && source
            .result_layout
            .as_ref()
            .is_none_or(|layout| runtime_handle_layout(Some(layout)) == Some(64))
    {
        let literal = process_string(design, operand);
        if literal.is_some_and(|text| !text.is_empty()) || dynamic_string_value(design, operand) {
            let view = if let Some(text) = literal {
                SourceReturnView::Literal(debug.string_constant_view(
                    module,
                    &format!("sx.debug.return.string.{}.{}", operand.0, width),
                    text,
                ))
            } else {
                SourceReturnView::String {
                    query: debug.string_query(module),
                    root: root?,
                }
            };
            let bytes =
                u32::try_from(debug.target.get_store_size(&debug.string_view_type())).ok()?;
            return SourceReturn::memory(debug, width, bytes, view);
        }
    }
    let layout = source
        .result_layout
        .as_ref()
        .map(std::borrow::Cow::Borrowed)
        .or_else(|| debug.value_debug_layout(design, operand));
    if width == 64
        && layout.as_ref().is_some_and(|layout| {
            matches!(
                layout.kind,
                LayoutKind::Scalar {
                    domain: ScalarDomain::Real,
                    ..
                }
            )
        })
    {
        Some(SourceReturn::Direct(debug.context.f64_type().into()))
    } else {
        if let Some(layout) = layout.as_deref() {
            // Source scalar types occupy a byte even when the kernel uses
            // i1..i7. Normalize every return bit: otherwise an i1 ABI can
            // leave the other register bits undefined for GDB's enum reader.
            if width < 8
                && layout_width(layout) == Some(width)
                && matches!(
                    layout.kind,
                    LayoutKind::Scalar { .. } | LayoutKind::Packed { .. }
                )
            {
                return Some(SourceReturn::Register {
                    native: debug.context.i8_type().into(),
                    layout: layout.clone(),
                });
            }
            // ponytail: SysV integer/double slots and memory classes;
            // odd-sized basic leaves and partial layouts remain separate work.
            // The native backend targets this host, not a cross-target triple.
            if cfg!(all(target_arch = "x86_64", not(target_os = "windows")))
                && matches!(
                    layout.kind,
                    LayoutKind::Struct { .. } | LayoutKind::Array { .. }
                )
                && layout_width(layout) == Some(width)
            {
                if let Some(inspection) = inspection_bits(layout).filter(|bits| {
                    (*bits > 128 || return_unaligned(layout) == Some(true))
                        && debug.ty(design, layout, needs_mirror(layout)).is_some()
                }) {
                    let bytes = u32::try_from(inspection / 8).ok()?;
                    // Leading source view, then a private execution payload.
                    // A caller cannot reconstruct packed offsets/signs merely
                    // by concatenating the normalized inspection bytes.
                    return SourceReturn::memory(
                        debug,
                        width,
                        bytes,
                        SourceReturnView::Packed(layout.clone()),
                    );
                }
                let mut fields = Vec::new();
                if let Some(inspection) = inspection_bits(layout).filter(|bits| *bits <= 128) {
                    if return_unaligned(layout) == Some(false)
                        && return_fields(debug.context, layout, &mut fields).is_some()
                    {
                        let native = if fields.iter().any(|field| field.is_float_type()) {
                            debug.context.struct_type(&fields, false).into()
                        } else {
                            debug
                                .context
                                .custom_width_int_type(std::num::NonZeroU32::new(
                                    u32::try_from(inspection).ok()?,
                                )?)
                                .ok()?
                                .into()
                        };
                        return Some(if inspection == u64::from(width) {
                            SourceReturn::Direct(native)
                        } else {
                            SourceReturn::Register {
                                native,
                                layout: layout.clone(),
                            }
                        });
                    }
                }
            }
        }
        debug
            .context
            .custom_width_int_type(std::num::NonZeroU32::new(width)?)
            .ok()
            .map(|ty| SourceReturn::Direct(ty.into()))
    }
}

/// The natural alignment GDB assigns to our byte-normalized source types.
/// Odd-sized basic integers remain an unsupported debugger ABI, not a guessed
/// alignment. A struct/array inherits its members' maximum/element alignment.
fn return_alignment(layout: &SourceLayout) -> Option<u64> {
    match &layout.kind {
        LayoutKind::Struct { fields, .. } => fields.iter().try_fold(1, |alignment, field| {
            Some(alignment.max(return_alignment(&field.layout)?))
        }),
        LayoutKind::Array { element, .. } => return_alignment(element),
        _ => {
            let bytes = inspection_bits(layout)? / 8;
            bytes.is_power_of_two().then_some(bytes)
        }
    }
}

/// SysV puts even small aggregates with naturally unaligned members in memory.
fn return_unaligned(layout: &SourceLayout) -> Option<bool> {
    match &layout.kind {
        LayoutKind::Struct { fields, .. } => {
            let mut offset = 0u64;
            for field in fields {
                if !offset.is_multiple_of(return_alignment(&field.layout)?)
                    || return_unaligned(&field.layout)?
                {
                    return Some(true);
                }
                offset = offset.checked_add(inspection_bits(&field.layout)? / 8)?;
            }
            Some(false)
        }
        LayoutKind::Array { element, .. } => return_unaligned(element),
        _ => Some(false),
    }
}

/// Flatten complete byte-normalized leaves into SysV INTEGER/SSE eightbytes.
/// Adjacent subword integers share one slot, not one register per field.
fn return_fields<'ctx>(
    context: &'ctx Context,
    layout: &SourceLayout,
    fields: &mut Vec<BasicTypeEnum<'ctx>>,
) -> Option<()> {
    match &layout.kind {
        LayoutKind::Struct {
            fields: members, ..
        } => {
            for field in members {
                return_fields(context, &field.layout, fields)?;
            }
        }
        LayoutKind::Array {
            range: Some(range),
            element,
        } => {
            for _ in 0..range.len()? {
                return_fields(context, element, fields)?;
            }
        }
        LayoutKind::Scalar {
            width: 64,
            domain: ScalarDomain::Real,
            ..
        } => {
            fields.push(context.f64_type().into());
        }
        LayoutKind::Scalar { width, .. } | LayoutKind::Packed { width, .. }
            if *width != 0 && matches!(width.div_ceil(8) * 8, 8 | 16 | 32 | 64) =>
        {
            let mut width = width.div_ceil(8) * 8;
            if let Some(BasicTypeEnum::IntType(last)) = fields.last() {
                if last.get_bit_width() < 64 {
                    width = width
                        .checked_add(last.get_bit_width())
                        .filter(|width| *width <= 64)?;
                    fields.pop();
                }
            }
            fields.push(
                context
                    .custom_width_int_type(std::num::NonZeroU32::new(width)?)
                    .ok()?
                    .into(),
            );
        }
        _ => return None,
    }
    Some(())
}

fn return_field_width(field: BasicTypeEnum<'_>) -> Option<u32> {
    match field {
        BasicTypeEnum::IntType(ty) => Some(ty.get_bit_width()),
        BasicTypeEnum::FloatType(ty) if ty == ty.get_context().f64_type() => Some(64),
        _ => None,
    }
}

/// Reversible SSA-only packing between canonical bit offsets and byte-sized
/// native register fields. Signed padding is source data, not debugger storage.
fn register_bits<'ctx>(
    builder: &Builder<'ctx>,
    value: IntValue<'ctx>,
    layout: &SourceLayout,
    normalize: bool,
) -> Option<IntValue<'ctx>> {
    fn fields<'ctx>(
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
        layout: &SourceLayout,
        normalize: bool,
        offsets: &mut (u32, u32),
        result: &mut IntValue<'ctx>,
    ) -> Option<()> {
        match &layout.kind {
            LayoutKind::Struct {
                fields: members, ..
            } => {
                for member in members {
                    fields(builder, value, &member.layout, normalize, offsets, result)?;
                }
            }
            LayoutKind::Array {
                range: Some(range),
                element,
            } => {
                for _ in 0..range.len()? {
                    fields(builder, value, element, normalize, offsets, result)?;
                }
            }
            _ => {
                let logical = layout_width(layout)?;
                let native = logical.div_ceil(8).checked_mul(8)?;
                let (source, source_width, target, target_width) = if normalize {
                    (offsets.0, logical, offsets.1, native)
                } else {
                    (offsets.1, native, offsets.0, logical)
                };
                let part = extract_region(builder, value, source, source_width)?;
                let part =
                    if normalize && source_width < target_width && process_layout_is_signed(layout)
                    {
                        let ty = value
                            .get_type()
                            .get_context()
                            .custom_width_int_type(std::num::NonZeroU32::new(target_width)?)
                            .ok()?;
                        builder
                            .build_int_s_extend(part, ty, "debug.return.signed")
                            .ok()?
                    } else {
                        fit(builder, part, target_width)?
                    };
                let part = fit(builder, part, result.get_type().get_bit_width())?;
                let part = builder
                    .build_left_shift(
                        part,
                        part.get_type().const_int(u64::from(target), false),
                        "debug.return.normalized.shift",
                    )
                    .ok()?;
                *result = builder
                    .build_or(*result, part, "debug.return.normalized")
                    .ok()?;
                offsets.0 = offsets.0.checked_add(logical)?;
                offsets.1 = offsets.1.checked_add(native)?;
            }
        }
        Some(())
    }
    let width = if normalize {
        u32::try_from(inspection_bits(layout)?).ok()?
    } else {
        layout_width(layout)?
    };
    let ty = value
        .get_type()
        .get_context()
        .custom_width_int_type(std::num::NonZeroU32::new(width)?)
        .ok()?;
    let mut result = ty.const_zero();
    fields(builder, value, layout, normalize, &mut (0, 0), &mut result)?;
    Some(result)
}

pub(in crate::llvm::process) fn return_value<'ctx>(
    builder: &Builder<'ctx>,
    value: IntValue<'ctx>,
) -> Option<BasicValueEnum<'ctx>> {
    let ty = builder
        .get_insert_block()?
        .get_parent()?
        .get_type()
        .get_return_type()?;
    if ty.is_float_type() {
        builder.build_bit_cast(value, ty, "debug.return.real").ok()
    } else if let BasicTypeEnum::StructType(ty) = ty {
        let mut result = ty.get_undef();
        let mut offset = 0;
        for (index, field) in ty.get_field_types().into_iter().enumerate() {
            let shifted = builder
                .build_right_shift(
                    value,
                    value.get_type().const_int(offset, false),
                    false,
                    "debug.return.field",
                )
                .ok()?;
            let width = return_field_width(field)?;
            let bits = fit(builder, shifted, width)?;
            let field_value = if field.is_float_type() {
                builder
                    .build_bit_cast(bits, field, "debug.return.real")
                    .ok()?
            } else {
                bits.into()
            };
            result = builder
                .build_insert_value(result, field_value, index as u32, "debug.return.aggregate")
                .ok()?
                .into_struct_value();
            offset += u64::from(width);
        }
        Some(result.into())
    } else {
        Some(value.into())
    }
}

pub(in crate::llvm::process) fn return_bits<'ctx>(
    builder: &Builder<'ctx>,
    value: BasicValueEnum<'ctx>,
) -> Option<IntValue<'ctx>> {
    match value {
        BasicValueEnum::FloatValue(value) => builder
            .build_bit_cast(
                value,
                value.get_type().get_context().i64_type(),
                "debug.return.bits",
            )
            .ok()
            .map(|value| value.into_int_value()),
        BasicValueEnum::IntValue(value) => Some(value),
        BasicValueEnum::StructValue(value) => {
            let count = value.get_type().count_fields();
            let context = value.get_type().get_context();
            let width = value
                .get_type()
                .get_field_types()
                .into_iter()
                .try_fold(0u32, |total, field| {
                    total.checked_add(return_field_width(field)?)
                })?;
            let ty = context
                .custom_width_int_type(std::num::NonZeroU32::new(width)?)
                .ok()?;
            let mut bits = ty.const_zero();
            let mut offset = 0u64;
            for index in 0..count {
                let field = builder
                    .build_extract_value(value, index, "debug.return.field")
                    .ok()?;
                let field = return_bits(builder, field)?;
                let field_width = field.get_type().get_bit_width();
                let field = builder
                    .build_int_z_extend(field, ty, "debug.return.word")
                    .ok()?;
                let field = builder
                    .build_left_shift(field, ty.const_int(offset, false), "debug.return.shift")
                    .ok()?;
                bits = builder.build_or(bits, field, "debug.return.bits").ok()?;
                offset += u64::from(field_width);
            }
            Some(bits)
        }
        _ => None,
    }
}

pub(in crate::llvm::process) struct ValueFrame<'ctx> {
    function: Span,
    scope: DISubprogram<'ctx>,
    caller: Option<DILocation<'ctx>>,
    bindings: Vec<ValueBinding<'ctx>>,
    branches: Vec<(ProcessValueId, [Span; 2])>,
}

struct ValueBinding<'ctx> {
    id: ProcessValueId,
    variable: DILocalVariable<'ctx>,
    location: DILocation<'ctx>,
    reference: PointerValue<'ctx>,
    buffer: GlobalValue<'ctx>,
    inspection: ValueInspection<'ctx>,
    signed: bool,
    parameter: bool,
    captured: Cell<Option<(IntValue<'ctx>, Option<IntValue<'ctx>>, BasicBlock<'ctx>)>>,
    fragment: Option<(u64, u64)>,
}

/// Byte-aligned inspection offsets, not the executable packed bit offsets.
fn inspection_projection<'a>(
    layout: &'a SourceLayout,
    path: &str,
) -> Option<(u64, &'a SourceLayout)> {
    if path.is_empty() {
        return Some((0, layout));
    }
    match &layout.kind {
        LayoutKind::Struct { fields, .. } => {
            let path = path.strip_prefix('.').unwrap_or(path);
            let end = path.find(['.', '[']).unwrap_or(path.len());
            let mut offset = 0u64;
            for field in fields {
                if field.name == path[..end] {
                    let (child, layout) = inspection_projection(&field.layout, &path[end..])?;
                    return Some((offset.checked_add(child)?, layout));
                }
                offset = offset.checked_add(inspection_bits(&field.layout)?)?;
            }
            None
        }
        LayoutKind::Array {
            range: Some(range),
            element,
        } => {
            let (index, rest) = path.strip_prefix('[')?.split_once(']')?;
            let index = index.parse::<i64>().ok()?;
            if index < range.left.min(range.right) || index > range.left.max(range.right) {
                return None;
            }
            let (child, layout) = inspection_projection(element, rest)?;
            let offset = index
                .abs_diff(range.left)
                .checked_mul(inspection_bits(element)?)?;
            Some((offset.checked_add(child)?, layout))
        }
        _ => None,
    }
}

fn binding_expression(fragment: Option<(u64, u64)>) -> Vec<i64> {
    let mut expression = vec![0x06 /* DW_OP_deref */];
    if let Some((offset, size)) = fragment {
        expression.extend([
            0x1000, /* DW_OP_LLVM_fragment */
            offset as i64,
            size as i64,
        ]);
    }
    expression
}

enum ValueInspection<'ctx> {
    Packed(SourceLayout),
    Logic(SourceLayout),
    String(u32, FunctionValue<'ctx>),
}

/// Discover the native ABI from actual consumer requests, not inspection types.
/// Temporary entry phis are replaced by formal arguments before verification;
/// they never execute and introduce no argument buffers or simulation copies.
pub(in crate::llvm::process) struct ValueArguments<'ctx> {
    ids: std::collections::HashSet<ProcessValueId>,
    values: Vec<(ValueFunctionKey, IntValue<'ctx>)>,
    /// Consumed compile-time arguments need source calls, but no native payload.
    literals: Vec<ProcessValueId>,
}

impl ValueArguments<'_> {
    fn new(source: &ProcessValueInline) -> Self {
        Self {
            ids: source
                .parameters
                .iter()
                .flat_map(|parameter| parameter.values())
                .collect(),
            values: Vec::new(),
            literals: Vec::new(),
        }
    }
}

fn literal_argument(
    design: &Design,
    cache: &mut ProcessValueCache<'_, '_>,
    id: ProcessValueId,
) -> Option<ProcessValueId> {
    let arguments = cache.debug_arguments.as_mut()?;
    if !arguments.ids.contains(&id) || process_string(design, id).is_none() {
        return None;
    }
    if !arguments.literals.contains(&id) {
        arguments.literals.push(id);
    }
    let mut literal = id;
    while let ProcessValueKind::InlineCall { operand, .. } =
        design.process_ir.values[literal.0 as usize].kind
    {
        literal = operand;
    }
    Some(literal)
}

pub(in crate::llvm::process) fn argument_value<'ctx>(
    builder: &Builder<'ctx>,
    design: &Design,
    cache: &mut ProcessValueCache<'ctx, '_>,
    mut key: ValueFunctionKey,
) -> Option<IntValue<'ctx>> {
    let arguments = cache.debug_arguments.as_mut()?;
    if !arguments.ids.contains(&key.0) || process_string(design, key.0).is_some() {
        return None;
    }
    let (id, width, signed) = (key.0, key.1, key.2);
    if key.3.is_none() {
        if let Some(width) = argument_boundary_width(design, id) {
            key.1 = width;
            key.2 = false;
        }
    }
    let existing = arguments
        .values
        .iter()
        .find(|(format, _)| *format == key)
        .map(|(_, value)| *value);
    let context = cache.debug?.context;
    let value = if let Some(value) = existing {
        value
    } else {
        let entry = builder
            .get_insert_block()?
            .get_parent()?
            .get_first_basic_block()?;
        let argument_builder = context.create_builder();
        if let Some(first) = entry.get_first_instruction() {
            argument_builder.position_before(&first);
        } else {
            argument_builder.position_at_end(entry);
        }
        let ty = context
            .custom_width_int_type(std::num::NonZeroU32::new(key.1)?)
            .ok()?;
        let value = argument_builder
            .build_phi(ty, "debug.argument.format")
            .ok()?
            .as_basic_value()
            .into_int_value();
        arguments.values.push((key, value));
        value
    };
    if signed && process_value_is_signed(design, id) {
        fit_signed(builder, value, width)
    } else {
        fit(builder, value, width)
    }
}

fn argument_boundary_width(design: &Design, id: ProcessValueId) -> Option<u32> {
    let mut boundary = id;
    while let ProcessValueKind::InlineCall { operand, .. } =
        &design.process_ir.values.get(boundary.0 as usize)?.kind
    {
        boundary = *operand;
    }
    let node = design.process_ir.values.get(boundary.0 as usize)?;
    // These operands already have an executable width boundary. The ordinary
    // emitter reads them once at that width and only fits the resulting bits;
    // passing a new ABI argument for every consumer needlessly re-expands the
    // caller. Contextual arithmetic and expected-layout conversions stay exact.
    if node.bit_width == design.process_ir.values[id.0 as usize].bit_width
        && process_value_is_signed(design, boundary) == process_value_is_signed(design, id)
        && matches!(
            node.kind,
            ProcessValueKind::Number(_)
                | ProcessValueKind::BitString { .. }
                | ProcessValueKind::Char(_)
                | ProcessValueKind::Signal { .. }
                | ProcessValueKind::Local { .. }
                | ProcessValueKind::Storage(_)
                | ProcessValueKind::Parameter { .. }
                | ProcessValueKind::RawResize { .. }
                | ProcessValueKind::BitSlice { .. }
                | ProcessValueKind::PackedSlice { .. }
        )
    {
        node.bit_width
    } else {
        None
    }
}

impl DebugInfo<'_, '_> {
    pub(in crate::llvm::process) fn value_requires_activity(&self, id: ProcessValueId) -> bool {
        self.value_activity
            .get(id.0 as usize)
            .copied()
            .unwrap_or(false)
    }
}

/// Child emission restores its parent's location before the parent operation.
pub(in crate::llvm::process) struct ValueLocationGuard<'a, 'ctx> {
    builder: &'a Builder<'ctx>,
    previous: Option<inkwell::llvm_sys::prelude::LLVMMetadataRef>,
}

impl<'a, 'ctx> ValueLocationGuard<'a, 'ctx> {
    pub(in crate::llvm::process) fn new(
        builder: &'a Builder<'ctx>,
        cache: &ProcessValueCache<'ctx, '_>,
        span: Span,
    ) -> Self {
        // Inkwell 0.9 uses the deprecated LLVMValueRef getter; a missing
        // location can become an empty MDTuple. Preserve the metadata directly.
        let previous = cache.debug.map(|_| unsafe {
            inkwell::llvm_sys::core::LLVMGetCurrentDebugLocation2(builder.as_mut_ptr())
        });
        if let Some(location) = value_location(cache, span) {
            builder.set_current_debug_location(location);
        }
        Self { builder, previous }
    }
}

impl Drop for ValueLocationGuard<'_, '_> {
    fn drop(&mut self) {
        if let Some(previous) = self.previous {
            unsafe {
                inkwell::llvm_sys::core::LLVMSetCurrentDebugLocation2(
                    self.builder.as_mut_ptr(),
                    previous,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::llvm::process) fn inline_value<'ctx>(
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    id: ProcessValueId,
    operand: ProcessValueId,
    source: &ProcessValueInline,
    width: u32,
    signed: bool,
    layout: Option<&SourceLayout>,
    active: Option<IntValue<'ctx>>,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
    emit: impl FnOnce(
        &Builder<'ctx>,
        &mut ProcessValueCache<'ctx, '_>,
        Option<IntValue<'ctx>>,
        ProcessValueId,
    ) -> Option<IntValue<'ctx>>,
) -> Option<IntValue<'ctx>> {
    let Some(debug) = cache.debug else {
        return emit(builder, cache, active, operand);
    };
    if let Some(literal) = literal_argument(design, cache, id) {
        return emit(builder, cache, active, literal);
    }
    if let Some(active) = active {
        return active_value(
            debug.context,
            builder,
            width,
            active,
            cache,
            |builder, cache| {
                inline_value(
                    module,
                    builder,
                    design,
                    id,
                    operand,
                    source,
                    width,
                    signed,
                    layout,
                    None,
                    index_sites,
                    cache,
                    emit,
                )
            },
        );
    }
    if source.aggregate.is_some() && debug.aggregate_members.contains_key(&id) {
        return aggregate_value(
            module,
            builder,
            design,
            id,
            width,
            signed,
            index_sites,
            cache,
        );
    }
    let key = (id, width, signed, layout.cloned());
    if debug
        .value_effects
        .get(id.0 as usize)
        .copied()
        .unwrap_or(false)
    {
        // A debug-only native body would partition the common emitter's
        // captured effects. Keep their execution/cache in the caller and use
        // a real DWARF inline frame, including lazy argument consumption.
        return call_value(debug.context, builder, key, cache, |builder, cache| {
            inline_body(
                module,
                builder,
                design,
                None,
                operand,
                source,
                cache,
                |builder, cache, active| emit(builder, cache, active, operand),
            )
        });
    }
    let existing = debug.value_functions.borrow().get(&key).cloned();
    let (function, formats, literals) = if let Some(existing) = existing {
        existing
    } else {
        let ty = source_return_type(
            debug,
            module,
            design,
            source,
            operand,
            width,
            cache.debug_root,
        )?;
        // Let the common emitter request its real operand formats once. A
        // source argument can be used at several widths inside this body;
        // capturing it at its minimum width first changes contextual arithmetic.
        let draft = module.add_function(
            &format!("sx.debug.call.{}", id.0),
            ty.fn_type(debug.context, &[]),
            Some(Linkage::Internal),
        );
        ty.declare(debug.context, draft, None);
        let body_builder = debug.context.create_builder();
        body_builder.position_at_end(debug.context.append_basic_block(draft, "entry"));
        let mut body_cache = ProcessValueCache::new(cache.activity_sensitive, cache.meta_free);
        body_cache.debug = Some(debug);
        body_cache.debug_root = cache.debug_root;
        body_cache.hardware_calls = cache.hardware_calls;
        body_cache.debug_arguments = Some(ValueArguments::new(source));
        let returned = inline_body(
            module,
            &body_builder,
            design,
            Some(draft),
            operand,
            source,
            &mut body_cache,
            |builder, cache, active| emit(builder, cache, active, operand),
        )?;
        ty.emit_return(Some(debug), &body_builder, returned)?;
        let arguments = body_cache.debug_arguments.take()?;
        let values = arguments.values;
        let literals = arguments.literals;
        let types = values
            .iter()
            .map(|(_, value)| value.get_type().into())
            .collect::<Vec<_>>();
        let name = draft.get_name().to_string_lossy().into_owned();
        draft.as_global_value().set_name(&format!("{name}.draft"));
        let function = module.add_function(
            &name,
            ty.fn_type(debug.context, &types),
            Some(Linkage::Internal),
        );
        ty.declare(debug.context, function, Some(debug));
        function.set_subprogram(draft.get_subprogram()?);
        if ty.parameter_offset() != 0 {
            draft
                .get_first_param()?
                .into_pointer_value()
                .replace_all_uses_with(function.get_first_param()?.into_pointer_value());
        }
        for (index, (_, placeholder)) in values.iter().enumerate() {
            placeholder.replace_all_uses_with(
                function
                    .get_nth_param(index as u32 + ty.parameter_offset())?
                    .into_int_value(),
            );
            placeholder.as_instruction()?.erase_from_basic_block();
        }
        for block in draft.get_basic_blocks() {
            block.remove_from_function().ok()?;
            function.append_existing_basic_block(block);
        }
        // No call or executable reference to the draft has been emitted.
        unsafe { draft.delete() };
        let formats = values
            .into_iter()
            .map(|(format, _)| format)
            .collect::<Vec<_>>();
        debug
            .value_functions
            .borrow_mut()
            .insert(key.clone(), (function, formats.clone(), literals.clone()));
        (function, formats, literals)
    };
    // Metadata never requests arguments. Emit only formats actually consumed
    // by the body, through exactly the same paths as an ordinary inline call.
    call_value(debug.context, builder, key, cache, |builder, cache| {
        for literal in literals {
            literal_value(module, builder, design, literal, index_sites, cache)?;
        }
        let mut arguments = Vec::with_capacity(formats.len());
        for (value, width, signed, layout) in formats {
            let argument = if let Some(layout) = layout {
                process_value_in_layout(
                    debug.context,
                    module,
                    builder,
                    design,
                    value,
                    &layout,
                    None,
                    index_sites,
                    cache,
                )?
            } else {
                process_value_at(
                    debug.context,
                    module,
                    builder,
                    design,
                    value,
                    width,
                    signed,
                    None,
                    index_sites,
                    cache,
                )?
            };
            arguments.push(argument.into());
        }
        call_return(
            debug.context,
            builder,
            function,
            &arguments,
            "debug.call",
            Some(debug),
        )
    })
}

/// Preserve a consumed constant-string call without evaluating debug bindings.
/// Nonempty results use the ordinary value emitter. Empty results have no
/// packed payload: their source boundary is a void specialization of the same
/// inline body, not a fabricated integer or runtime string handle.
pub(in crate::llvm::process) fn literal_value<'ctx>(
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    id: ProcessValueId,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<()> {
    let Some(debug) = cache.debug else {
        return Some(());
    };
    if literal_argument(design, cache, id).is_some() {
        return Some(());
    }
    let _location =
        ValueLocationGuard::new(builder, cache, design.process_ir.values[id.0 as usize].span);
    let text = process_string(design, id)?;
    let ProcessValueKind::InlineCall { operand, source } =
        &design.process_ir.values[id.0 as usize].kind
    else {
        return Some(());
    };
    if !text.is_empty() {
        process_value(
            debug.context,
            module,
            builder,
            design,
            id,
            None,
            index_sites,
            cache,
        )?;
        return Some(());
    }
    let key = (id, 0, false, None);
    let existing = debug.value_functions.borrow().get(&key).cloned();
    let (function, _, literals) = if let Some(existing) = existing {
        existing
    } else {
        let function = module.add_function(
            &format!("sx.debug.call.{}.empty", id.0),
            debug.context.void_type().fn_type(&[], false),
            Some(Linkage::Internal),
        );
        let body_builder = debug.context.create_builder();
        body_builder.position_at_end(debug.context.append_basic_block(function, "entry"));
        let mut body_cache = ProcessValueCache::new(cache.activity_sensitive, cache.meta_free);
        body_cache.debug = Some(debug);
        body_cache.debug_root = cache.debug_root;
        body_cache.debug_arguments = Some(ValueArguments::new(source));
        let mut emitted = false;
        inline_body(
            module,
            &body_builder,
            design,
            Some(function),
            *operand,
            source,
            &mut body_cache,
            |builder, cache, _| {
                emitted =
                    literal_value(module, builder, design, *operand, index_sites, cache).is_some();
                None
            },
        );
        if !emitted {
            return None;
        }
        body_builder.build_return(None).ok()?;
        let arguments = body_cache.debug_arguments.take()?;
        if !arguments.values.is_empty() {
            return None;
        }
        let entry = (function, Vec::new(), arguments.literals);
        debug
            .value_functions
            .borrow_mut()
            .insert(key, entry.clone());
        entry
    };
    call_value(
        debug.context,
        builder,
        (id, 0, false, None),
        cache,
        |builder, cache| {
            for literal in literals {
                literal_value(module, builder, design, literal, index_sites, cache)?;
            }
            builder.build_call(function, &[], "").ok()?;
            // This is only the shared emitter's execution/readiness marker. The
            // native callee is void; no integer stands in for an empty string.
            Some(debug.context.bool_type().const_int(1, false))
        },
    )?;
    Some(())
}

#[allow(clippy::too_many_arguments)]
fn aggregate_value<'ctx>(
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    id: ProcessValueId,
    width: u32,
    signed: bool,
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<IntValue<'ctx>> {
    let debug = cache.debug?;
    let &(leader, offset, field_width) = debug.aggregate_members.get(&id)?;
    let aggregate = debug.aggregate_returns.get(&leader)?;
    let ProcessValueKind::InlineCall { operand, source } =
        &design.process_ir.values[leader.0 as usize].kind
    else {
        return None;
    };
    let mut source = source.as_ref().clone();
    let root_width = layout_width(&aggregate.layout)?;
    let returned = if !aggregate.fields.is_empty() {
        source.locals.push(siox::ir::ProcessValueBinding {
            name: "$return".into(),
            span: source.result_span,
            scope: None,
            layout: source.result_layout.clone(),
            value: None,
            fields: aggregate.fields.clone(),
        });
        // A partial source result has no full native return ABI. Keep this
        // frame over the common emitter/cache, including captured effects.
        call_value(
            debug.context,
            builder,
            (leader, root_width, false, Some(aggregate.layout.clone())),
            cache,
            |builder, cache| {
                inline_body(
                    module,
                    builder,
                    design,
                    None,
                    *operand,
                    &source,
                    cache,
                    |builder, cache, _| {
                        let returned = aggregate_result(
                            module,
                            builder,
                            design,
                            &aggregate.layout,
                            &aggregate.pieces,
                            index_sites,
                            cache,
                        )?;
                        // Synchronized selections assemble a whole carrier,
                        // without emitting the original per-field selector IDs.
                        // Observe their actual joined bits before this frame ends.
                        let _location = ValueLocationGuard::new(builder, cache, source.result_span);
                        for (operand, offset, layout) in &aggregate.pieces {
                            let part =
                                extract_region(builder, returned, *offset, layout_width(layout)?)?;
                            capture_value(builder, design, cache, *operand, part);
                        }
                        Some(returned)
                    },
                )
            },
        )?
    } else {
        source.aggregate = None;
        inline_value(
            module,
            builder,
            design,
            leader,
            *operand,
            &source,
            root_width,
            false,
            Some(&aggregate.layout),
            None,
            index_sites,
            cache,
            |builder, cache, _, _| {
                aggregate_result(
                    module,
                    builder,
                    design,
                    &aggregate.layout,
                    &aggregate.pieces,
                    index_sites,
                    cache,
                )
            },
        )?
    };
    let field = extract_region(builder, returned, offset, field_width)?;
    if signed && process_value_is_signed(design, id) {
        fit_signed(builder, field, width)
    } else {
        fit(builder, field, width)
    }
}

/// Synchronized field selectors represent one aggregate source return.
/// Select its whole value once, including nested return branches, rather than
/// emitting a separate source return range for every field.
fn aggregate_result<'ctx>(
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    layout: &SourceLayout,
    pieces: &[(ProcessValueId, u32, SourceLayout)],
    index_sites: &HashMap<IndexSite, u32>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) -> Option<IntValue<'ctx>> {
    let debug = cache.debug?;
    let selector = pieces[0].0;
    let selection = |id: ProcessValueId| match design.process_ir.values[id.0 as usize].kind {
        ProcessValueKind::Select {
            condition,
            then_value,
            else_value,
        } => Some((condition, [then_value, else_value])),
        _ => None,
    };
    if let Some((condition, _)) = selection(selector) {
        let arms = pieces
            .iter()
            .map(|(id, offset, layout)| {
                let (other, arms) = selection(*id)?;
                (condition == other).then_some((arms, *offset, layout))
            })
            .collect::<Option<Vec<_>>>();
        if let Some(arms) = arms {
            let condition = process_value(
                debug.context,
                module,
                builder,
                design,
                condition,
                None,
                index_sites,
                cache,
            )?;
            return selected_value(
                module,
                builder,
                design,
                selector,
                as_condition(builder, condition)?,
                Some(layout),
                cache,
                |builder, arm, _, cache| {
                    let pieces = arms
                        .iter()
                        .map(|(ids, offset, layout)| (ids[arm], *offset, (*layout).clone()))
                        .collect::<Vec<_>>();
                    aggregate_result(module, builder, design, layout, &pieces, index_sites, cache)
                },
            );
        }
    }
    let mut result = debug
        .context
        .custom_width_int_type(std::num::NonZeroU32::new(layout_width(layout)?)?)
        .ok()?
        .const_zero();
    for (operand, offset, layout) in pieces {
        let part = process_value_in_layout(
            debug.context,
            module,
            builder,
            design,
            *operand,
            layout,
            None,
            index_sites,
            cache,
        )?;
        result = insert_region(builder, result, part, *offset, layout_width(layout)?)?;
    }
    Some(result)
}

/// A selected alias is evaluated once per actual consumer format. Readiness
/// travels with its value through SSA joins, never through runtime storage.
pub(in crate::llvm::process) fn call_value<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    key: ValueFunctionKey,
    cache: &mut ProcessValueCache<'ctx, '_>,
    emit: impl FnOnce(&Builder<'ctx>, &mut ProcessValueCache<'ctx, '_>) -> Option<IntValue<'ctx>>,
) -> Option<IntValue<'ctx>> {
    if !cache
        .debug
        .is_some_and(|debug| debug.reused_calls.contains(&key.0))
    {
        return emit(builder, cache);
    }
    let ready = context.bool_type().const_int(1, false);
    if let Some((value, available)) = cache.source_results.get(&key).copied() {
        if available.get_zero_extended_constant() == Some(1) {
            return Some(value);
        }
        let missing = builder.build_not(available, "debug.call.missing").ok()?;
        active_value(context, builder, key.1, missing, cache, |builder, cache| {
            let value = emit(builder, cache)?;
            cache.source_results.insert(key.clone(), (value, ready));
            Some(value)
        })?;
        let value = cache.source_results.get(&key)?.0;
        cache.source_results.insert(key, (value, ready));
        Some(value)
    } else {
        let value = emit(builder, cache)?;
        cache.source_results.insert(key, (value, ready));
        Some(value)
    }
}

/// Keep both a result and whether its call actually ran on each predecessor.
fn merge_results<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    arms: [(&ValueResults<'ctx>, BasicBlock<'ctx>); 2],
) -> Option<ValueResults<'ctx>> {
    let mut results = HashMap::new();
    for key in arms[0].0.keys().chain(arms[1].0.keys()) {
        if results.contains_key(key) {
            continue;
        }
        let (value, _) = arms[0].0.get(key).or_else(|| arms[1].0.get(key))?;
        let absent = (
            value.get_type().const_zero(),
            context.bool_type().const_zero(),
        );
        let left = arms[0].0.get(key).copied().unwrap_or(absent);
        let right = arms[1].0.get(key).copied().unwrap_or(absent);
        let merge = |left: IntValue<'ctx>, right: IntValue<'ctx>, name| {
            if left == right {
                Some(left)
            } else {
                let phi = builder.build_phi(left.get_type(), name).ok()?;
                phi.add_incoming(&[(&left, arms[0].1), (&right, arms[1].1)]);
                Some(phi.as_basic_value().into_int_value())
            }
        };
        results.insert(
            key.clone(),
            (
                merge(left.0, right.0, "debug.call.saved")?,
                merge(left.1, right.1, "debug.call.ready")?,
            ),
        );
    }
    Some(results)
}

/// Native source calls execute only on their selected source path. Both
/// shared Process functions and expression frames use this same guard.
pub(in crate::llvm::process) fn active_value<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    width: u32,
    active: IntValue<'ctx>,
    cache: &mut ProcessValueCache<'ctx, '_>,
    emit: impl FnOnce(&Builder<'ctx>, &mut ProcessValueCache<'ctx, '_>) -> Option<IntValue<'ctx>>,
) -> Option<IntValue<'ctx>> {
    match active.get_zero_extended_constant() {
        Some(0) => {
            return Some(
                context
                    .custom_width_int_type(std::num::NonZeroU32::new(width)?)
                    .ok()?
                    .const_zero(),
            )
        }
        Some(1) => return emit(builder, cache),
        _ => {}
    }
    let previous = builder.get_insert_block()?;
    let function = previous.get_parent()?;
    let execute = context.append_basic_block(function, "debug.call.execute");
    let join = context.append_basic_block(function, "debug.call.join");
    builder
        .build_conditional_branch(active, execute, join)
        .ok()?;
    let dominating = cache.dominating_values();
    let saved = cache.source_results.clone();
    builder.position_at_end(execute);
    let returned = emit(builder, cache)?;
    let exit = builder.get_insert_block()?;
    let executed = std::mem::take(&mut cache.source_results);
    builder.build_unconditional_branch(join).ok()?;
    builder.position_at_end(join);
    cache.retain_dominating(&dominating);
    cache.source_results =
        merge_results(context, builder, [(&saved, previous), (&executed, exit)])?;
    let result = builder
        .build_phi(returned.get_type(), "debug.call.result")
        .ok()?;
    result.add_incoming(&[
        (&returned.get_type().const_zero(), previous),
        (&returned, exit),
    ]);
    Some(result.as_basic_value().into_int_value())
}

#[allow(clippy::too_many_arguments)]
pub(in crate::llvm::process) fn inline_body<'ctx>(
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    function: Option<FunctionValue<'ctx>>,
    operand: ProcessValueId,
    source: &ProcessValueInline,
    cache: &mut ProcessValueCache<'ctx, '_>,
    emit: impl FnOnce(
        &Builder<'ctx>,
        &mut ProcessValueCache<'ctx, '_>,
        Option<IntValue<'ctx>>,
    ) -> Option<IntValue<'ctx>>,
) -> Option<IntValue<'ctx>> {
    let debug = cache.debug?;
    let native = function.is_some();
    let function = function.or_else(|| builder.get_insert_block()?.get_parent())?;
    let previous_location = cache.source_location;
    let inline_caller = (!native)
        .then(|| {
            value_location(cache, source.call)
                .or_else(|| debug.generated_location(builder, source.call))
        })
        .flatten();
    // A leaf that does not cover the whole result is a partial evaluation.
    // Keep its declared type on an inline frame, not on the native leaf ABI,
    // and expose only its retained result fragment.
    let partial_path = source.aggregate.as_ref().filter(|(_, path)| {
        !source.result_layout.as_ref().is_some_and(|root| {
            inspection_projection(root, path).is_some_and(|(offset, leaf)| {
                offset == 0
                    && inspection_bits(root)
                        .zip(inspection_bits(leaf))
                        .is_some_and(|(whole, part)| whole == part)
            })
        })
    });
    // Wide basic returns and odd-sized aggregate leaves are not a reliable
    // GDB return ABI. Keep their declared types on an inline source frame over
    // this same body, without changing the executable payload or ABI.
    let result_layout = source
        .result_layout
        .as_ref()
        .filter(|layout| layout.packed_width().is_some())
        .map(std::borrow::Cow::Borrowed)
        .or_else(|| debug.value_debug_layout(design, operand));
    // Large aggregates already use the supported memory-return convention.
    // A word-sized declared return can still have a wider private consumer
    // payload. Up to two words use registers; larger integers use a hidden
    // memory ABI which GDB cannot infer from the smaller source type.
    let wide_native = function
        .get_type()
        .get_return_type()
        .is_some_and(|ty| ty.is_int_type() && ty.into_int_type().get_bit_width() > 128);
    let indirect_frame = cfg!(all(target_arch = "x86_64", not(target_os = "windows")))
        && result_layout.as_ref().is_some_and(|layout| {
            let Some(bits) = inspection_bits(layout) else {
                return false;
            };
            let aggregate = matches!(
                layout.kind,
                LayoutKind::Struct { .. } | LayoutKind::Array { .. }
            );
            if aggregate {
                bits <= 128 && return_alignment(layout).is_none()
            } else {
                bits > 64 || wide_native || return_alignment(layout).is_none()
            }
        });
    let partial = partial_path
        .filter(|_| {
            !source
                .locals
                .iter()
                .any(|binding| binding.name == "$return")
        })
        .map(|(_, path)| {
            let mut source = source.clone();
            source.locals.push(siox::ir::ProcessValueBinding {
                name: "$return".into(),
                span: source.result_span,
                scope: None,
                layout: source.result_layout.clone(),
                value: None,
                fields: vec![(path.clone(), Some(operand))],
            });
            source
        });
    let source = partial.as_ref().unwrap_or(source);
    let (owner, name) = source.name.rsplit_once("::").unwrap_or(("", &source.name));
    let file = debug.file(source.function.file);
    let line = debug
        .sources
        .line_col(source.function.file, source.function.start)
        .0;
    let binding_types = source
        .parameters
        .iter()
        .chain(&source.locals)
        .map(|binding| {
            if let Some(id) = binding.value {
                if dynamic_string_value(design, id) || process_string(design, id).is_some() {
                    return debug.value_debug_type(design, id);
                }
            }
            match &binding.layout {
                Some(layout) => debug.ty(design, layout, needs_mirror(layout)),
                None => debug.value_debug_type(design, binding.value?),
            }
        })
        .collect::<Vec<_>>();
    let result_type = if process_string(design, operand) == Some("") {
        None // This specialization has no executable return payload.
    } else {
        source
            .result_layout
            .as_ref()
            .and_then(|layout| debug.ty(design, layout, needs_mirror(layout)))
            .or_else(|| debug.value_debug_type(design, operand))
    };
    let types = binding_types[..source.parameters.len()]
        .iter()
        .flatten()
        .copied()
        .collect::<Vec<_>>();
    let scope = debug.builder.create_function(
        debug.namespace(&owner.replace("::", ".")),
        name,
        Some(""),
        file,
        line,
        debug
            .builder
            .create_subroutine_type(file, result_type, &types, DIFlags::ZERO),
        true,
        true,
        line,
        DIFlags::ZERO,
        false,
    );
    let caller = if !native {
        inline_caller
    } else if partial_path.is_some() || indirect_frame {
        let file = debug.file(source.call.file);
        let line = debug
            .sources
            .line_col(source.call.file, source.call.start)
            .0;
        let native = debug.builder.create_function(
            debug.namespace(""),
            &partial_path.map_or_else(
                || format!("<return {name}>"),
                |(_, path)| format!("<partial {name}.{path}>"),
            ),
            Some(""),
            file,
            line,
            debug
                .builder
                .create_subroutine_type(file, None, &[], DIFlags::ARTIFICIAL),
            true,
            true,
            line,
            DIFlags::ARTIFICIAL,
            false,
        );
        function.set_subprogram(native);
        Some(debug.location(native, source.call, None))
    } else {
        function.set_subprogram(scope);
        None
    };
    debug.declare_lexical_scopes(
        scope,
        source
            .locals
            .iter()
            .filter_map(|binding| binding.scope.map(|extent| (None, extent)))
            .collect(),
        &[],
    );
    cache.source_location = Some(debug.builder.create_debug_location(
        debug.context,
        line,
        0,
        scope.as_debug_info_scope(),
        caller,
    ));
    // Function-name breakpoints must skip reference-slot initialization. The
    // first visible source instruction follows native-input inspection below.
    let entry_location = debug.builder.create_debug_location(
        debug.context,
        0,
        0,
        scope.as_debug_info_scope(),
        caller,
    );
    // Inline frame ranges must start after their reference reset. Otherwise
    // a function-name stop can inspect stale pointers from the previous loop
    // iteration before this invocation's line-zero setup has executed.
    builder.set_current_debug_location(inline_caller.unwrap_or(entry_location));
    let mut bindings = Vec::new();
    for (index, (binding, ty)) in source
        .parameters
        .iter()
        .chain(&source.locals)
        .zip(binding_types)
        .enumerate()
    {
        let Some(ty) = ty else {
            continue;
        };
        let (line, column) = debug
            .sources
            .line_col(binding.span.file, binding.span.start);
        let scope = if index < source.parameters.len() {
            scope.as_debug_info_scope()
        } else {
            debug.source_scope(scope, binding.span, None).0
        };
        let variable = if index < source.parameters.len() {
            debug.builder.create_parameter_variable(
                scope,
                &binding.name,
                index as u32 + 1,
                debug.file(binding.span.file),
                line,
                ty,
                true,
                DIFlags::ZERO,
            )
        } else {
            debug.builder.create_auto_variable(
                scope,
                &binding.name,
                debug.file(binding.span.file),
                line,
                ty,
                true,
                DIFlags::ZERO,
                0,
            )
        };
        let location =
            debug
                .builder
                .create_debug_location(debug.context, line, column, scope, caller);
        let root = binding
            .layout
            .as_ref()
            .map(std::borrow::Cow::Borrowed)
            .or_else(|| debug.value_debug_layout(design, binding.value?));
        let logic_view = root
            .as_ref()
            .filter(|layout| layout_has_packed_metadata(design, layout))
            .and_then(|layout| logic::inspection_layout(design, layout));
        let mut variables = vec![(variable, false)];
        if let Some(ty) = logic_view
            .as_ref()
            .and_then(|view| debug.ty(design, view, true))
        {
            variables.push((
                debug.builder.create_auto_variable(
                    scope,
                    &format!("{}$logic", binding.name),
                    debug.file(binding.span.file),
                    line,
                    ty,
                    true,
                    DIFlags::ZERO,
                    0,
                ),
                true,
            ));
        }
        for (variable, logic) in variables {
            let pieces = if let Some(id) = binding.value {
                vec![(id, binding.layout.as_ref(), None)]
            } else {
                binding
                    .fields
                    .iter()
                    .filter_map(|(path, id)| {
                        let root = binding.layout.as_ref()?;
                        let (offset, layout) = inspection_projection(root, path)?;
                        let view = if logic { logic_view.as_ref()? } else { root };
                        let (offset, size) = if logic {
                            let (offset, layout) = inspection_projection(view, path)?;
                            (offset, inspection_bits(layout)?)
                        } else {
                            (offset, inspection_bits(layout)?)
                        };
                        // LLVM fragments must describe a proper subregion. A
                        // one-leaf wrapper already supplies the whole variable.
                        let fragment = (offset != 0 || size != inspection_bits(view)?)
                            .then_some((offset, size));
                        Some(((*id)?, Some(layout), fragment))
                    })
                    .collect()
            };
            for (id, layout, fragment) in pieces {
                let literal = process_string(design, id);
                let string = literal.is_some() || dynamic_string_value(design, id);
                let layout = if string {
                    None
                } else {
                    layout
                        .map(std::borrow::Cow::Borrowed)
                        .or_else(|| debug.value_debug_layout(design, id))
                };
                if !string && layout.is_none() {
                    continue;
                }
                let name = format!(
                    "{}.debug.value.{index}.{}.{}",
                    function.get_name().to_string_lossy(),
                    fragment.map_or(0, |(offset, _)| offset),
                    if logic { "logic" } else { "bits" },
                );
                let mut available = false;
                let (buffer, inspection) = if let Some(text) = literal {
                    debug.string_constant_view(module, &name, text);
                    available = true;
                    (module.get_global(&name).unwrap(), None)
                } else if string {
                    let Some(root) = cache.debug_root else {
                        continue;
                    };
                    // A CFG-normalized host call can consume the argument before
                    // this native frame. Borrow its existing persistent place;
                    // inspection never reexecutes the source expression.
                    let place = match design.process_ir.values[id.0 as usize].kind {
                        ProcessValueKind::Local { process, local } => {
                            Some(local_state_name(process, local))
                        }
                        ProcessValueKind::Storage(storage) => Some(storage_state_name(storage)),
                        _ => None,
                    };
                    if let Some(place) = place {
                        let name = debug.string_mirror(module, &place, root);
                        available = true;
                        (module.get_global(&name).unwrap(), None)
                    } else {
                        (
                            debug.string_buffer(module, &name),
                            Some(ValueInspection::String(root, debug.string_query(module))),
                        )
                    }
                } else {
                    let layout = layout.unwrap();
                    let view = if logic {
                        logic::inspection_layout(design, &layout)
                    } else {
                        Some(layout.as_ref().clone())
                    }?;
                    let Some(bytes) =
                        inspection_bits(&view).and_then(|bits| u32::try_from(bits / 8).ok())
                    else {
                        continue;
                    };
                    (
                        debug.inspection_buffer(module, &name, bytes),
                        Some(if logic {
                            ValueInspection::Logic(layout.into_owned())
                        } else {
                            ValueInspection::Packed(layout.into_owned())
                        }),
                    )
                };
                // Allocate references once per process invocation, never in a source
                // loop. Their contents are inspection-only and do not own source state.
                let entry = builder
                    .get_insert_block()
                    .unwrap()
                    .get_parent()
                    .unwrap()
                    .get_first_basic_block()
                    .unwrap();
                let entry_builder = debug.context.create_builder();
                if let Some(first) = entry.get_first_instruction() {
                    entry_builder.position_before(&first);
                } else {
                    entry_builder.position_at_end(entry);
                }
                let pointer = debug.context.ptr_type(AddressSpace::default());
                let reference = entry_builder
                    .build_alloca(pointer, "debug.value.reference")
                    .unwrap();
                builder
                    .build_store(
                        reference,
                        if available {
                            buffer.as_pointer_value()
                        } else {
                            pointer.const_null()
                        },
                    )
                    .unwrap();
                if available || (fragment.is_none() && !logic) {
                    unsafe {
                        inkwell::llvm_sys::debuginfo::LLVMDIBuilderInsertDeclareRecordAtEnd(
                            debug.builder.as_mut_ptr(),
                            reference.as_value_ref(),
                            variable.as_mut_ptr(),
                            debug
                                .builder
                                .create_expression(binding_expression(fragment))
                                .as_mut_ptr(),
                            location.as_mut_ptr(),
                            builder.get_insert_block().unwrap().as_mut_ptr(),
                        );
                    }
                }
                if let Some(inspection) = inspection {
                    bindings.push(ValueBinding {
                        id,
                        variable,
                        location,
                        reference,
                        buffer,
                        inspection,
                        signed: process_value_is_signed(design, id),
                        parameter: index < source.parameters.len(),
                        captured: Cell::new(None),
                        fragment,
                    });
                }
            }
        }
    }
    cache.value_frames.push(ValueFrame {
        function: source.function,
        scope,
        caller,
        bindings,
        branches: source.branches.clone(),
    });
    // Constants need no execution. Other bindings become available only when
    // their ordinary SSA value is actually emitted (never inspect by reevaluation).
    for binding in source.parameters.iter().chain(&source.locals) {
        for id in binding.values() {
            let width = design.process_ir.values[id.0 as usize]
                .bit_width
                .unwrap_or(64);
            if let Some(width) = std::num::NonZeroU32::new(width) {
                if let Ok(ty) = debug.context.custom_width_int_type(width) {
                    if let Some(value) = constant_integer(design, id, ty) {
                        capture_value(builder, design, cache, id, value);
                    }
                }
            }
            if let Some(value) = cache
                .parameters
                .get(&id)
                .copied()
                .or_else(|| cache.emitted.get(&cache.key(id, None, None)).copied())
            {
                capture_value(builder, design, cache, id, value);
            }
        }
    }
    let argument_entry = builder.get_insert_block()?;
    let argument_anchor = argument_entry.get_last_instruction();
    // Inline frames can execute repeatedly in the same native invocation.
    // Reset their return reference at the source entry, not just native entry.
    let expose_return = (!native || indirect_frame) && partial_path.is_none();
    let return_reference = expose_return.then(|| {
        let entry = function.get_first_basic_block().unwrap();
        let allocation = debug.context.create_builder();
        if let Some(first) = entry.get_first_instruction() {
            allocation.position_before(&first);
        } else {
            allocation.position_at_end(entry);
        }
        let pointer = debug.context.ptr_type(AddressSpace::default());
        let reference = |name| {
            let reference = allocation.build_alloca(pointer, name).unwrap();
            builder
                .build_store(reference, pointer.const_null())
                .unwrap();
            reference
        };
        (
            reference("debug.return.reference"),
            result_layout
                .as_ref()
                .filter(|layout| layout_has_packed_metadata(design, layout))
                .map(|_| reference("debug.return.logic.reference")),
        )
    });
    builder.set_current_debug_location(entry_location);
    if cache.debug_arguments.is_some() {
        // Inspection of native inputs is inserted here after discovering the
        // consumed formats. Keep the body in another block: even at -O0,
        // SelectionDAG can reuse a later packed extraction in those entry
        // stores and place a return-line instruction before inputs are visible.
        let body = debug
            .context
            .append_basic_block(function, "debug.arguments.available");
        builder.build_unconditional_branch(body).ok()?;
        builder.position_at_end(body);
    }
    let result = emit(builder, cache, None);
    if let Some(arguments) = &cache.debug_arguments {
        // Native inputs are available at entry, even if their first consumer
        // follows the instruction where GDB stops. Aliases of these same IDs
        // need no execution either. Inspection must not create ABI arguments.
        let argument_builder = debug.context.create_builder();
        if let Some(first) = argument_anchor
            .and_then(|anchor| anchor.get_next_instruction())
            .or_else(|| {
                argument_anchor
                    .is_none()
                    .then(|| argument_entry.get_first_instruction())
                    .flatten()
            })
        {
            argument_builder.position_before(&first);
        } else {
            argument_builder.position_at_end(argument_entry);
        }
        argument_builder.set_current_debug_location(entry_location);
        // The first consumer can be narrower than the source declaration.
        // Prefer the widest already-requested format, never add one for DWARF.
        let mut values = arguments
            .values
            .iter()
            .map(|((id, ..), value)| (*id, *value))
            .collect::<Vec<_>>();
        values
            .sort_by_key(|(id, value)| (id.0, std::cmp::Reverse(value.get_type().get_bit_width())));
        values.dedup_by_key(|(id, _)| *id);
        for (id, value) in values {
            capture_binding(&argument_builder, design, cache, id, value, true);
        }
    }
    let result_span = source.result_span;
    if let Some(value) = result {
        capture_result(
            module,
            builder,
            design,
            operand,
            value,
            result_span,
            partial_path
                .is_none()
                .then_some(result_layout.as_deref())
                .flatten(),
            return_reference,
            cache,
        );
    }
    let location = value_location(cache, result_span)?;
    cache.value_frames.pop();
    cache.source_location = previous_location;
    // Keep the completion edge in the callee's source range so its captured
    // return is inspectable after the last store. The enclosing value-location
    // guard restores the caller before its next source operation.
    builder.set_current_debug_location(location);
    result
}

#[allow(clippy::too_many_arguments)]
fn capture_result<'ctx>(
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    id: ProcessValueId,
    value: IntValue<'ctx>,
    span: Span,
    layout: Option<&SourceLayout>,
    reference: Option<(PointerValue<'ctx>, Option<PointerValue<'ctx>>)>,
    cache: &mut ProcessValueCache<'ctx, '_>,
) {
    let Some(debug) = cache.debug else { return };
    let Some(layout) = layout
        .map(std::borrow::Cow::Borrowed)
        .or_else(|| debug.value_debug_layout(design, id))
    else {
        return;
    };
    let Some(width) = layout.packed_width() else {
        return;
    };
    // A constant/arithmetic return can otherwise have no machine range even
    // at -O0. These inspection writes never become simulation operands.
    let _location = ValueLocationGuard::new(builder, cache, span);
    // Do not present padding of a narrower consumer as a complete result.
    let reference = reference.filter(|_| value.get_type().get_bit_width() >= width);
    if let Some(bytes) = inspection_bits(&layout).and_then(|bits| u32::try_from(bits / 8).ok()) {
        let buffer = debug.inspection_buffer(module, "sx.debug.return", bytes);
        if let Some(value) = fit(builder, value, width) {
            debug.copy_fields(
                builder,
                buffer.as_pointer_value(),
                value,
                &layout,
                &mut 0,
                &mut 0,
            );
            if let Some((reference, logic_reference)) = reference {
                if let (Some(frame), Some(ty), Some(location)) = (
                    cache.value_frames.last(),
                    debug.ty(design, &layout, needs_mirror(&layout)),
                    value_location(cache, span),
                ) {
                    let function = builder.get_insert_block().unwrap().get_parent().unwrap();
                    builder
                        .build_store(reference, buffer.as_pointer_value())
                        .unwrap();
                    let variable = debug.builder.create_auto_variable(
                        frame.scope.as_debug_info_scope(),
                        "$return",
                        debug.file(span.file),
                        debug.sources.line_col(span.file, span.start).0,
                        ty,
                        true,
                        DIFlags::ZERO,
                        0,
                    );
                    unsafe {
                        inkwell::llvm_sys::debuginfo::LLVMDIBuilderInsertDeclareRecordAtEnd(
                            debug.builder.as_mut_ptr(),
                            reference.as_value_ref(),
                            variable.as_mut_ptr(),
                            debug
                                .builder
                                .create_expression(binding_expression(None))
                                .as_mut_ptr(),
                            location.as_mut_ptr(),
                            builder.get_insert_block().unwrap().as_mut_ptr(),
                        );
                    }
                    // Reuse the ordinary binding observer: no missing plane
                    // may be requested just to complete a return's logic view.
                    if let Some((reference, view, ty)) = logic_reference.and_then(|reference| {
                        let view = logic::inspection_layout(design, &layout)?;
                        let ty = debug.ty(design, &view, true)?;
                        Some((reference, view, ty))
                    }) {
                        if let Some(bytes) =
                            inspection_bits(&view).and_then(|bits| u32::try_from(bits / 8).ok())
                        {
                            let binding = ValueBinding {
                                id,
                                variable: debug.builder.create_auto_variable(
                                    frame.scope.as_debug_info_scope(),
                                    "$return$logic",
                                    debug.file(span.file),
                                    debug.sources.line_col(span.file, span.start).0,
                                    ty,
                                    true,
                                    DIFlags::ZERO,
                                    0,
                                ),
                                location,
                                reference,
                                buffer: debug.inspection_buffer(
                                    module,
                                    "sx.debug.return.logic",
                                    bytes,
                                ),
                                inspection: ValueInspection::Logic(layout.as_ref().clone()),
                                signed: false,
                                parameter: false,
                                captured: Cell::new(None),
                                fragment: None,
                            };
                            cache
                                .value_frames
                                .last_mut()
                                .unwrap()
                                .bindings
                                .push(binding);
                            capture_binding(builder, design, cache, id, value, false);
                            cache.value_frames.last_mut().unwrap().bindings.pop();
                        }
                    }
                    let ready = debug
                        .context
                        .append_basic_block(function, "debug.return.available");
                    builder.build_unconditional_branch(ready).unwrap();
                    builder.position_at_end(ready);
                }
            }
        }
    }
}

/// Source return arms and guarded evaluations need distinct machine ranges.
/// Synthesized pure muxes (for example unrolled loop state) share their DAG
/// through ordinary SSA selects; splitting those recreates it in both arms.
pub(in crate::llvm::process) fn requires_source_selection(
    design: &Design,
    cache: &ProcessValueCache<'_, '_>,
    selection: ProcessValueId,
    arms: [ProcessValueId; 2],
) -> bool {
    let Some(frame) = cache.value_frames.last().filter(|_| cache.debug.is_some()) else {
        return false;
    };
    if frame.branches.iter().any(|(id, _)| *id == selection) {
        return true;
    }
    let mut pending = arms.to_vec();
    let mut visited = std::collections::HashSet::new();
    while let Some(id) = pending.pop() {
        // Native formals refer to caller-owned evaluations. Their original
        // source calls/checks do not execute again inside this body. Literal
        // source arguments still have call ranges despite having no payload.
        let captured = cache.parameters.contains_key(&id)
            || cache
                .debug_arguments
                .as_ref()
                .is_some_and(|args| args.ids.contains(&id) && process_string(design, id).is_none());
        if captured || !visited.insert(id) || !cache.requires_activity(id) {
            continue;
        }
        let Some(value) = design.process_ir.values.get(id.0 as usize) else {
            return true;
        };
        if matches!(
            value.kind,
            ProcessValueKind::InlineCall { .. }
                | ProcessValueKind::ForeignCall { .. }
                | ProcessValueKind::HostCall { .. }
                | ProcessValueKind::CheckedIndex { .. }
        ) {
            return true;
        }
        crate::ir::for_each_process_value_dependency(&value.kind, |id| pending.push(id));
    }
    false
}

#[allow(clippy::too_many_arguments)]
pub(in crate::llvm::process) fn selected_value<'ctx>(
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    design: &Design,
    selection: ProcessValueId,
    condition: IntValue<'ctx>,
    layout: Option<&SourceLayout>,
    cache: &mut ProcessValueCache<'ctx, '_>,
    mut emit: impl FnMut(
        &Builder<'ctx>,
        usize,
        ProcessValueId,
        &mut ProcessValueCache<'ctx, '_>,
    ) -> Option<IntValue<'ctx>>,
) -> Option<IntValue<'ctx>> {
    let debug = cache.debug?;
    let ProcessValueKind::Select {
        then_value,
        else_value,
        ..
    } = design.process_ir.values.get(selection.0 as usize)?.kind
    else {
        return None;
    };
    let arms = [then_value, else_value];
    let spans = cache
        .value_frames
        .last()?
        .branches
        .iter()
        .find_map(|(id, spans)| (*id == selection).then_some(*spans))
        .unwrap_or_else(|| arms.map(|id| design.process_ir.values[id.0 as usize].span));
    let function = builder.get_insert_block()?.get_parent()?;
    let then_block = debug
        .context
        .append_basic_block(function, "debug.select.then");
    let else_block = debug
        .context
        .append_basic_block(function, "debug.select.else");
    let join = debug
        .context
        .append_basic_block(function, "debug.select.join");
    builder
        .build_conditional_branch(condition, then_block, else_block)
        .ok()?;
    let dominating = cache.dominating_values();
    let saved = cache.source_results.clone();
    let mut results = Vec::with_capacity(2);
    let mut calls = Vec::with_capacity(2);
    for (arm, ((block, id), span)) in [(then_block, arms[0]), (else_block, arms[1])]
        .into_iter()
        .zip(spans)
        .enumerate()
    {
        builder.position_at_end(block);
        let value = emit(builder, arm, id, cache)?;
        capture_result(
            module, builder, design, id, value, span, layout, None, cache,
        );
        let exit = builder.get_insert_block()?;
        builder.build_unconditional_branch(join).ok()?;
        results.push((value, exit));
        calls.push(std::mem::take(&mut cache.source_results));
        cache.retain_dominating(&dominating);
        cache.source_results = saved.clone();
    }
    builder.position_at_end(join);
    cache.source_results = merge_results(
        debug.context,
        builder,
        [(&calls[0], results[0].1), (&calls[1], results[1].1)],
    )?;
    let result = builder
        .build_phi(results[0].0.get_type(), "debug.select.result")
        .ok()?;
    result.add_incoming(&[(&results[0].0, results[0].1), (&results[1].0, results[1].1)]);
    Some(result.as_basic_value().into_int_value())
}

fn value_location<'ctx>(
    cache: &ProcessValueCache<'ctx, '_>,
    span: Span,
) -> Option<DILocation<'ctx>> {
    let debug = cache.debug?;
    cache
        .value_frames
        .iter()
        .rev()
        .find(|frame| contains(frame.function, span))
        .map(|frame| {
            let (line, column) = debug.sources.line_col(span.file, span.start);
            debug.builder.create_debug_location(
                debug.context,
                line,
                column,
                debug.source_scope(frame.scope, span, None).0,
                frame.caller,
            )
        })
        .or(cache.source_location)
}

pub(in crate::llvm::process) fn capture_value<'ctx>(
    builder: &Builder<'ctx>,
    design: &Design,
    cache: &ProcessValueCache<'ctx, '_>,
    id: ProcessValueId,
    value: IntValue<'ctx>,
) {
    capture_binding(builder, design, cache, id, value, false);
}

/// Only observe planes the ordinary emitter already produced. In particular,
/// inspection never requests a missing metadata plane or repeats a source call.
pub(in crate::llvm::process) fn capture_metadata<'ctx>(
    builder: &Builder<'ctx>,
    design: &Design,
    cache: &ProcessValueCache<'ctx, '_>,
    id: ProcessValueId,
    layout: &SourceLayout,
) {
    if cache.debug.is_none()
        || !cache.value_frames.iter().any(|frame| {
            frame.bindings.iter().any(|binding| {
                binding.id == id && matches!(binding.inspection, ValueInspection::Logic(_))
            })
        })
    {
        return;
    }
    let Some(width) = layout.packed_width() else {
        return;
    };
    let value = cache
        .parameters
        .get(&id)
        .copied()
        .filter(|value| value.get_type().get_bit_width() == width)
        .or_else(|| {
            cache
                .debug_arguments
                .as_ref()?
                .values
                .iter()
                .find_map(|((argument, ..), value)| {
                    (*argument == id && value.get_type().get_bit_width() == width).then_some(*value)
                })
        })
        .or_else(|| {
            cache
                .emitted
                .iter()
                .find_map(|((value_id, active, _), value)| {
                    (*value_id == id
                        && active.is_none()
                        && value.get_type().get_bit_width() == width)
                        .then_some(*value)
                })
        })
        .or_else(|| {
            cache
                .contextual
                .iter()
                .find_map(|((value_id, active, ..), value)| {
                    (*value_id == id
                        && active.is_none()
                        && value.get_type().get_bit_width() == width)
                        .then_some(*value)
                })
        });
    if let Some(value) = value {
        capture_binding(builder, design, cache, id, value, false);
    }
}

/// Recover literal bits without host-integer truncation or executing a value.
fn constant_integer<'ctx>(
    design: &Design,
    id: ProcessValueId,
    ty: inkwell::types::IntType<'ctx>,
) -> Option<IntValue<'ctx>> {
    match &design.process_ir.values.get(id.0 as usize)?.kind {
        ProcessValueKind::Number(ProcessNumber::Integer(words)) => {
            Some(ty.const_int_arbitrary_precision(words))
        }
        ProcessValueKind::InlineCall { operand, .. } => constant_integer(design, *operand, ty),
        ProcessValueKind::Unary {
            operation: ProcessUnaryOp::Neg,
            operand,
        } => Some(constant_integer(design, *operand, ty)?.const_neg()),
        _ => None,
    }
}

fn capture_binding<'ctx>(
    builder: &Builder<'ctx>,
    design: &Design,
    cache: &ProcessValueCache<'ctx, '_>,
    id: ProcessValueId,
    value: IntValue<'ctx>,
    at_entry: bool,
) {
    let Some(debug) = cache.debug else { return };
    let mut local_captured = false;
    for frame in &cache.value_frames {
        for binding in &frame.bindings {
            if binding.id != id {
                continue;
            }
            let mut value = value;
            let width = match &binding.inspection {
                ValueInspection::Packed(layout) => Some(
                    layout.packed_width().unwrap().min(
                        design.process_ir.values[id.0 as usize]
                            .bit_width
                            .unwrap_or_else(|| layout.packed_width().unwrap()),
                    ),
                ),
                ValueInspection::Logic(layout) => layout.packed_width(),
                ValueInspection::String(..) => None,
            };
            if let Some(width) = width.filter(|width| value.get_type().get_bit_width() < *width) {
                // Missing consumer bits are not zeros. Fully known literals
                // can supply them without execution; other values stay absent.
                let Some(width) = std::num::NonZeroU32::new(width) else {
                    continue;
                };
                let Ok(ty) = debug.context.custom_width_int_type(width) else {
                    continue;
                };
                let Some(constant) = constant_integer(design, id, ty) else {
                    continue;
                };
                value = constant;
            }
            let metadata = if let ValueInspection::Logic(layout) = &binding.inspection {
                // Body-local plane values do not dominate the entry insertion
                // point. A proven binary value needs no such plane.
                let meta_free = cache.meta_free.get(id.0 as usize).copied().unwrap_or(false);
                if at_entry && !meta_free {
                    continue;
                }
                let metadata = if at_entry {
                    None
                } else {
                    cache
                        .metadata
                        .iter()
                        .find_map(|((value_id, active, source), value)| {
                            (*value_id == id && active.is_none() && source.kind == layout.kind)
                                .then_some(*value)
                        })
                };
                if metadata.is_none() && !meta_free {
                    continue;
                }
                metadata
            } else {
                None
            };
            let capture = (value, metadata, builder.get_insert_block().unwrap());
            if binding.captured.get() == Some(capture) {
                continue;
            }
            // Arithmetic may use wider carry temporaries than the source
            // binding. Describe its declared value, not the carry bits.
            match &binding.inspection {
                ValueInspection::Packed(layout) => {
                    let width = layout.packed_width().unwrap();
                    let value = if binding.signed && process_layout_is_signed(layout) {
                        fit_signed(builder, value, width)
                    } else {
                        fit(builder, value, width)
                    };
                    let Some(value) = value else {
                        continue;
                    };
                    debug.copy_fields(
                        builder,
                        binding.buffer.as_pointer_value(),
                        value,
                        layout,
                        &mut 0,
                        &mut 0,
                    );
                }
                ValueInspection::String(root, query) => {
                    let Some(value) = fit(builder, value, 64) else {
                        continue;
                    };
                    // The module owns this global and the runtime query.
                    // Keep borrowed content behind the existing inspection ABI.
                    debug.capture_string(
                        builder,
                        *query,
                        *root,
                        value,
                        binding.buffer.as_pointer_value(),
                    );
                }
                ValueInspection::Logic(layout) => {
                    let Some(value) = fit(builder, value, layout.packed_width().unwrap()) else {
                        continue;
                    };
                    debug.copy_logic_fields(
                        builder,
                        design,
                        binding.buffer,
                        value,
                        metadata,
                        layout,
                        &mut 0,
                        &mut 0,
                    );
                }
            }
            builder
                .build_store(binding.reference, binding.buffer.as_pointer_value())
                .unwrap();
            // LLVM 22 inserts a debug record, not an LLVM instruction.
            unsafe {
                inkwell::llvm_sys::debuginfo::LLVMDIBuilderInsertDeclareRecordAtEnd(
                    debug.builder.as_mut_ptr(),
                    binding.reference.as_value_ref(),
                    binding.variable.as_mut_ptr(),
                    debug
                        .builder
                        .create_expression(binding_expression(binding.fragment))
                        .as_mut_ptr(),
                    binding.location.as_mut_ptr(),
                    builder.get_insert_block().unwrap().as_mut_ptr(),
                );
            }
            binding.captured.set(Some(capture));
            local_captured |= !binding.parameter && !at_entry;
        }
    }
    if local_captured {
        // SelectionDAG can schedule later source arithmetic before these
        // inspection stores even at -O0. A debug-only block boundary keeps
        // a completed local visible at the next source statement's stop.
        let previous = builder.get_insert_block().unwrap();
        let next = debug
            .context
            .append_basic_block(previous.get_parent().unwrap(), "debug.value.available");
        builder.build_unconditional_branch(next).unwrap();
        builder.position_at_end(next);
        for frame in &cache.value_frames {
            for binding in &frame.bindings {
                if let Some((value, metadata, block)) = binding.captured.get() {
                    if block == previous {
                        binding.captured.set(Some((value, metadata, next)));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod demand_tests {
    use super::*;

    #[test]
    fn partial_groups_compact_only_jointly_demanded_fields() {
        let mut design = Design::default();
        let span = Span::new(FileId(0), 0..10);
        let scalar = SourceLayout {
            span,
            kind: LayoutKind::Scalar {
                width: 64,
                domain: ScalarDomain::Integer,
                nominal: None,
                value_range: None,
            },
        };
        let layout = SourceLayout {
            span,
            kind: LayoutKind::Struct {
                name: "Triple".into(),
                view: None,
                fields: ["left", "unused", "right"]
                    .into_iter()
                    .map(|name| siox::ir::LayoutField {
                        name: name.into(),
                        direction: None,
                        layout: scalar.clone(),
                    })
                    .collect(),
            },
        };
        let node = |kind| siox::ir::ProcessValue {
            span,
            ty: Some(siox::types::Ty::Integer),
            bit_width: Some(64),
            kind,
        };
        for value in [2, 12] {
            design
                .process_ir
                .values
                .push(node(ProcessValueKind::Number(ProcessNumber::Integer(
                    vec![value],
                ))));
        }
        for (operand, path) in [(0, "left"), (1, "right")] {
            design
                .process_ir
                .values
                .push(node(ProcessValueKind::InlineCall {
                    operand: ProcessValueId(operand),
                    source: Box::new(ProcessValueInline {
                        name: "partial".into(),
                        function: span,
                        call: span,
                        result_span: span,
                        result_layout: Some(layout.clone()),
                        aggregate: Some((1, path.into())),
                        branches: Vec::new(),
                        parameters: Vec::new(),
                        locals: Vec::new(),
                    }),
                }));
        }
        design
            .process_ir
            .values
            .push(node(ProcessValueKind::Binary {
                operation: ProcessBinaryOp::Add,
                left: ProcessValueId(2),
                right: ProcessValueId(3),
            }));
        let (groups, members) = aggregate_returns(&design);
        let group = &groups[&ProcessValueId(2)];
        assert_eq!(layout_width(&group.layout), Some(128));
        assert_eq!(layout_width(&layout), Some(192));
        assert_eq!(
            group.fields,
            vec![
                ("left".into(), Some(ProcessValueId(0))),
                ("right".into(), Some(ProcessValueId(1)))
            ]
        );
        assert_eq!(members[&ProcessValueId(2)], (ProcessValueId(2), 0, 64));
        assert_eq!(members[&ProcessValueId(3)], (ProcessValueId(2), 64, 64));
        // Separate consumers cannot make the retained subset unconditional.
        design
            .process_ir
            .values
            .push(node(ProcessValueKind::RawResize {
                operand: ProcessValueId(2),
            }));
        assert!(aggregate_returns(&design).0.is_empty());
        design.process_ir.values.pop();
        if let ProcessValueKind::InlineCall { source, .. } = &mut design.process_ir.values[3].kind {
            source.aggregate.as_mut().unwrap().1 = "left".into();
        }
        // Duplicate/overlapping projections are not a retained carrier.
        assert!(aggregate_returns(&design).0.is_empty());
    }

    #[test]
    fn inspection_constants_preserve_full_words_and_negation() {
        let mut design = Design::default();
        let span = Span::new(FileId(0), 0..1);
        for kind in [
            ProcessValueKind::Number(ProcessNumber::Integer(vec![u64::MAX])),
            ProcessValueKind::Number(ProcessNumber::Integer(vec![7, 1])),
            ProcessValueKind::Unary {
                operation: ProcessUnaryOp::Neg,
                operand: ProcessValueId(1),
            },
        ] {
            design.process_ir.values.push(siox::ir::ProcessValue {
                span,
                ty: None,
                bit_width: Some(81),
                kind,
            });
        }
        let context = Context::create();
        let ty = context
            .custom_width_int_type(std::num::NonZeroU32::new(81).unwrap())
            .unwrap();
        assert_eq!(
            constant_integer(&design, ProcessValueId(0), ty),
            Some(ty.const_int(u64::MAX, false))
        );
        let wide = ty.const_int_arbitrary_precision(&[7, 1]);
        assert_eq!(constant_integer(&design, ProcessValueId(1), ty), Some(wide));
        assert_eq!(
            constant_integer(&design, ProcessValueId(2), ty),
            Some(wide.const_neg())
        );
        assert_eq!(
            constant_integer(&design, ProcessValueId(1), context.i8_type()),
            Some(context.i8_type().const_int(7, false))
        );
    }

    #[test]
    fn aggregate_order_keeps_checks_calls_and_reads_between_fields() {
        let span = Span::new(FileId(0), 0..1);
        let node = |kind| siox::ir::ProcessValue {
            span,
            ty: None,
            bit_width: Some(8),
            kind,
        };
        let add = |left, right| {
            node(ProcessValueKind::Binary {
                operation: ProcessBinaryOp::Add,
                left: ProcessValueId(left),
                right: ProcessValueId(right),
            })
        };
        let mut design = Design::default();
        design.process_ir.values = vec![
            node(ProcessValueKind::Local {
                process: siox::ir::ProcessId(0),
                local: siox::ir::ProcessLocalId(0),
            }),
            node(ProcessValueKind::Number(ProcessNumber::Integer(vec![1]))),
            node(ProcessValueKind::Number(ProcessNumber::Integer(vec![2]))),
            add(2, 1),
        ];
        let fields = [ProcessValueId(1), ProcessValueId(2)];
        let reverse = vec![fields[1], fields[0]];
        assert!(joint_aggregate_demand(&design, &fields));
        assert_eq!(
            aggregate_evaluation_order(&design, &fields),
            Some(reverse.clone())
        );
        // A whole-value conditional can guard the complete ordered call.
        design
            .process_ir
            .values
            .push(node(ProcessValueKind::Select {
                condition: ProcessValueId(0),
                then_value: ProcessValueId(3),
                else_value: ProcessValueId(0),
            }));
        assert_eq!(aggregate_evaluation_order(&design, &fields), Some(reverse));
        design.process_ir.values.pop();
        // Different first-use orders need different native bodies, not one
        // eager order chosen from the arena or from field declaration order.
        design.process_ir.values.push(add(1, 2));
        assert_eq!(aggregate_evaluation_order(&design, &fields), None);
        design.process_ir.values.pop();
        for barrier in [
            ProcessValueKind::ForeignCall {
                name: "putchar".into(),
                arguments: vec![ProcessValueId(0)],
                float_arguments: vec![false],
                integer_arguments: vec![true],
                float_result: false,
                integer_result: true,
                void_result: false,
            },
            ProcessValueKind::CheckedIndex {
                index: ProcessValueId(0),
                valid: ProcessValueId(0),
                left: 0,
                right: 1,
                span,
            },
            ProcessValueKind::Local {
                process: siox::ir::ProcessId(0),
                local: siox::ir::ProcessLocalId(0),
            },
        ] {
            design.process_ir.values.truncate(3);
            design.process_ir.values.push(node(barrier));
            design.process_ir.values.push(add(1, 3));
            design.process_ir.values.push(add(4, 2));
            assert!(joint_aggregate_demand(&design, &fields));
            assert_eq!(aggregate_evaluation_order(&design, &fields), None);
        }
        // One-bit `and` / `or` can skip a call in the right field entirely.
        design.process_ir.values.truncate(3);
        design.process_ir.values[1] = node(ProcessValueKind::Local {
            process: siox::ir::ProcessId(0),
            local: siox::ir::ProcessLocalId(0),
        });
        design.process_ir.values[1].bit_width = Some(1);
        design.process_ir.values[2].bit_width = Some(1);
        for operation in [ProcessBinaryOp::And, ProcessBinaryOp::Or] {
            design.process_ir.values.truncate(3);
            design
                .process_ir
                .values
                .push(node(ProcessValueKind::Binary {
                    operation,
                    left: fields[0],
                    right: fields[1],
                }));
            assert!(!joint_aggregate_demand(&design, &fields));
        }
    }

    #[test]
    fn nested_aggregate_calls_require_joint_demand_on_every_use() {
        let span = Span::new(FileId(0), 0..1);
        let node = |kind| siox::ir::ProcessValue {
            span,
            ty: None,
            bit_width: Some(8),
            kind,
        };
        let mut design = Design {
            process_ir: siox::ir::ProcessIr {
                values: vec![
                    node(ProcessValueKind::Number(siox::ir::ProcessNumber::Integer(
                        vec![0],
                    ))),
                    node(ProcessValueKind::Number(siox::ir::ProcessNumber::Integer(
                        vec![1],
                    ))),
                    node(ProcessValueKind::Number(siox::ir::ProcessNumber::Integer(
                        vec![2],
                    ))),
                    node(ProcessValueKind::Binary {
                        operation: ProcessBinaryOp::Add,
                        left: ProcessValueId(1),
                        right: ProcessValueId(2),
                    }),
                ],
                ..siox::ir::ProcessIr::default()
            },
            ..Design::default()
        };
        let fields = [ProcessValueId(1), ProcessValueId(2)];
        assert!(joint_aggregate_demand(&design, &fields));
        assert_eq!(constant_selector(&design, ProcessValueId(0)), Some(0));
        // A known-valid checked index remains its value; a failed check is
        // never evidence for choosing a branch or dropping an execution root.
        design
            .process_ir
            .values
            .push(node(ProcessValueKind::CheckedIndex {
                index: ProcessValueId(2),
                valid: ProcessValueId(1),
                left: 0,
                right: 2,
                span,
            }));
        assert_eq!(constant_selector(&design, ProcessValueId(4)), Some(2));
        design
            .process_ir
            .values
            .push(node(ProcessValueKind::Binary {
                operation: ProcessBinaryOp::Eq,
                left: ProcessValueId(4),
                right: ProcessValueId(2),
            }));
        assert_eq!(constant_selector(&design, ProcessValueId(5)), Some(1));
        design.process_ir.values[4] = node(ProcessValueKind::CheckedIndex {
            index: ProcessValueId(2),
            valid: ProcessValueId(0),
            left: 0,
            right: 2,
            span,
        });
        assert_eq!(constant_selector(&design, ProcessValueId(4)), None);
        assert_eq!(constant_selector(&design, ProcessValueId(5)), None);
        design.process_ir.values.pop();
        design.process_ir.values.pop();
        assert_eq!(constant_selector(&design, ProcessValueId(3)), None);
        design.process_ir.values[0].bit_width = Some(0);
        assert_eq!(constant_selector(&design, ProcessValueId(0)), None);
        design.process_ir.values[0].bit_width = Some(8);
        design.process_ir.values[1].ty = Some(siox::types::Ty::Integer);
        assert_eq!(constant_selector(&design, ProcessValueId(1)), None);
        design.process_ir.values[1].ty = None;
        // A whole selected value demands both fields only in its taken arm.
        design
            .process_ir
            .values
            .push(node(ProcessValueKind::Select {
                condition: ProcessValueId(0),
                then_value: ProcessValueId(3),
                else_value: ProcessValueId(0),
            }));
        assert!(joint_aggregate_demand(&design, &fields));
        // The same proof holds with an unknown runtime condition, not just a
        // constant-false branch. It must not demand the untaken whole value.
        design.process_ir.values[0] = node(ProcessValueKind::Local {
            process: siox::ir::ProcessId(0),
            local: siox::ir::ProcessLocalId(0),
        });
        assert_eq!(constant_selector(&design, ProcessValueId(0)), None);
        assert!(joint_aggregate_demand(&design, &fields));
        // An independent use cannot hide behind an otherwise complete consumer.
        design.process_ir.values.push(node(ProcessValueKind::Unary {
            operation: ProcessUnaryOp::Not,
            operand: ProcessValueId(1),
        }));
        assert!(!joint_aggregate_demand(&design, &fields));
        design.process_ir.values.pop();
        design.process_ir.functions.push(siox::ir::ProcessFunction {
            id: siox::ir::ProcessFunctionId(0),
            name: "independent".into(),
            span,
            parameters: Vec::new(),
            result: ProcessValueId(1),
        });
        assert!(!joint_aggregate_demand(&design, &fields));
        design.process_ir.functions.clear();
        design.process_ir.values[3] = node(ProcessValueKind::Select {
            condition: ProcessValueId(0),
            then_value: ProcessValueId(1),
            else_value: ProcessValueId(2),
        });
        assert!(!joint_aggregate_demand(&design, &fields));
    }
}
