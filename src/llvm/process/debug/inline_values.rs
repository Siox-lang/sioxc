//! Native source frames over the same canonical expressions as ordinary builds.

use super::*;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::basic_block::BasicBlock;
use inkwell::debug_info::DILocalVariable;
use inkwell::types::{
    AnyType, BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType, StructType,
};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use siox::ir::ProcessValueInline;

type AggregateMembers = HashMap<ProcessValueId, (ProcessValueId, u32, u32)>;

/// Executable return ABI, separate from the declaration's DWARF type.
pub(in crate::llvm::process) enum SourceReturn<'ctx> {
    Direct(BasicTypeEnum<'ctx>),
    Memory {
        storage: StructType<'ctx>,
        layout: SourceLayout,
    },
}

impl<'ctx> SourceReturn<'ctx> {
    pub(in crate::llvm::process) fn fn_type(
        &self,
        context: &'ctx Context,
        parameters: &[BasicMetadataTypeEnum<'ctx>],
    ) -> FunctionType<'ctx> {
        match self {
            Self::Direct(ty) => ty.fn_type(parameters, false),
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
    ) {
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
            Self::Memory { storage, layout } => {
                if value.get_type().as_basic_type_enum() != storage.get_field_type_at_index(1)? {
                    return None;
                }
                let output = builder
                    .get_insert_block()?
                    .get_parent()?
                    .get_first_param()?
                    .into_pointer_value();
                // All source fields are present. Copy only the already-emitted
                // bits into the same normalized view used by local inspection.
                debug?.copy_fields(builder, output, value, layout, &mut 0, &mut 0);
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
        builder
            .build_call(function, arguments, name)
            .ok()?
            .try_as_basic_value()
            .basic()
            .and_then(|value| return_bits(builder, value))
    }
}

/// Retain one native source boundary for jointly evaluable aggregate leaves.
/// No binding metadata or discarded field is made an executable dependency.
pub(super) fn aggregate_returns(
    design: &Design,
) -> (HashMap<ProcessValueId, AggregateReturn>, AggregateMembers) {
    let mut pure = Vec::with_capacity(design.process_ir.values.len());
    let mut groups: HashMap<_, Vec<_>> = HashMap::new();
    for (index, value) in design.process_ir.values.iter().enumerate() {
        // ponytail: effectful/checked/nested-call leaves need demand-preserving
        // grouping; eager packing must not introduce effects or source stops.
        let safe = matches!(
            value.kind,
            ProcessValueKind::Number(_)
                | ProcessValueKind::BitString { .. }
                | ProcessValueKind::Char(_)
                | ProcessValueKind::Signal { .. }
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
                if !matches!(operation,
                    ProcessBinaryOp::Div | ProcessBinaryOp::SignedDiv | ProcessBinaryOp::Custom(_))
        );
        pure.push(
            safe && !siox::ir::any_process_value_dependency(&value.kind, |id| {
                !pure.get(id.0 as usize).copied().unwrap_or(false)
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
                (pure[operand.0 as usize]
                    && design.process_ir.values[operand.0 as usize].bit_width
                        == Some(selected.width))
                .then(|| (*operand, selected.offset, selected.layout.clone()))
            })
            .collect::<Option<Vec<_>>>();
        let Some(pieces) = pieces else { continue };
        // A packed native return cannot describe missing fields as unavailable.
        // Keep partial returns leaf-backed rather than inventing zero values.
        let mut regions = pieces
            .iter()
            .map(|(_, offset, layout)| (*offset, layout_width(layout).unwrap()))
            .collect::<Vec<_>>();
        regions.sort_unstable();
        let covered = regions.into_iter().try_fold(0u32, |end, (offset, width)| {
            (offset == end).then(|| end.checked_add(width)).flatten()
        });
        if covered != layout_width(layout) {
            continue;
        }
        for (id, (_, offset, layout)) in group.into_iter().zip(&pieces) {
            members.insert(id, (leader, *offset, layout_width(layout).unwrap()));
        }
        returns.insert(
            leader,
            AggregateReturn {
                layout: layout.clone(),
                pieces,
            },
        );
    }
    (returns, members)
}

/// Use the source ABI only at the native boundary. The common emitter
/// and its exact consumer formats continue to exchange the same integer bits.
pub(in crate::llvm::process) fn source_return_type<'ctx>(
    debug: &DebugInfo<'ctx, '_>,
    design: &Design,
    source: &ProcessValueInline,
    operand: ProcessValueId,
    width: u32,
) -> Option<SourceReturn<'ctx>> {
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
            // ponytail: SysV word-register and >16-byte memory classes;
            // small unaligned/subword layouts still need explicit classification.
            // The native backend targets this host, not a cross-target triple.
            if cfg!(all(target_arch = "x86_64", not(target_os = "windows")))
                && matches!(
                    layout.kind,
                    LayoutKind::Struct { .. } | LayoutKind::Array { .. }
                )
                && layout_width(layout) == Some(width)
            {
                if let Some(inspection) = inspection_bits(layout).filter(|bits| {
                    *bits > 128 && debug.ty(design, layout, needs_mirror(layout)).is_some()
                }) {
                    let bits = debug
                        .context
                        .custom_width_int_type(std::num::NonZeroU32::new(width)?)
                        .ok()?;
                    let bytes = u32::try_from(inspection / 8).ok()?;
                    // Leading source view, then a private execution payload.
                    // A caller cannot reconstruct packed offsets/signs merely
                    // by concatenating the normalized inspection bytes.
                    let storage = debug.context.struct_type(
                        &[
                            debug.context.i8_type().array_type(bytes).into(),
                            bits.into(),
                        ],
                        false,
                    );
                    return Some(SourceReturn::Memory {
                        storage,
                        layout: layout.clone(),
                    });
                }
                let mut fields = Vec::new();
                if width <= 128
                    && return_fields(debug.context, layout, &mut fields).is_some()
                    && fields.iter().any(|field| field.is_float_type())
                {
                    return Some(SourceReturn::Direct(
                        debug.context.struct_type(&fields, false).into(),
                    ));
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

/// Flatten complete 64-bit storage leaves in declaration order, matching
/// SysV's INTEGER/SSE eightbytes without inventing padding or missing fields.
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
        LayoutKind::Scalar { width: 64, .. } | LayoutKind::Packed { width: 64, .. } => {
            fields.push(context.i64_type().into());
        }
        _ => return None,
    }
    Some(())
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
            let bits = builder
                .build_int_truncate(
                    shifted,
                    value.get_type().get_context().i64_type(),
                    "debug.return.word",
                )
                .ok()?;
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
            offset += 64;
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
            let ty = context
                .custom_width_int_type(std::num::NonZeroU32::new(count.checked_mul(64)?)?)
                .ok()?;
            let mut bits = ty.const_zero();
            for index in 0..count {
                let field = builder
                    .build_extract_value(value, index, "debug.return.field")
                    .ok()?;
                let field = return_bits(builder, field)?;
                let field = builder
                    .build_int_z_extend(field, ty, "debug.return.word")
                    .ok()?;
                let field = builder
                    .build_left_shift(
                        field,
                        ty.const_int(u64::from(index) * 64, false),
                        "debug.return.shift",
                    )
                    .ok()?;
                bits = builder.build_or(bits, field, "debug.return.bits").ok()?;
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
    let existing = debug.value_functions.borrow().get(&key).cloned();
    let (function, formats, literals) = if let Some(existing) = existing {
        existing
    } else {
        let ty = source_return_type(debug, design, source, operand, width)?;
        // Let the common emitter request its real operand formats once. A
        // source argument can be used at several widths inside this body;
        // capturing it at its minimum width first changes contextual arithmetic.
        let draft = module.add_function(
            &format!("sx.debug.call.{}", id.0),
            ty.fn_type(debug.context, &[]),
            Some(Linkage::Internal),
        );
        ty.declare(debug.context, draft);
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
            draft,
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
        ty.declare(debug.context, function);
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
        call_return(debug.context, builder, function, &arguments, "debug.call")
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
            function,
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
    source.aggregate = None;
    let root_width = layout_width(&aggregate.layout)?;
    let returned = inline_value(
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
    )?;
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
    function: FunctionValue<'ctx>,
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
    function.set_subprogram(scope);
    debug.declare_lexical_scopes(
        scope,
        source
            .locals
            .iter()
            .filter_map(|binding| binding.scope.map(|extent| (None, extent)))
            .collect(),
        &[],
    );
    let caller = None;
    cache.source_location = Some(debug.builder.create_debug_location(
        debug.context,
        line,
        0,
        scope.as_debug_info_scope(),
        None,
    ));
    builder.set_current_debug_location(cache.source_location.unwrap());
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
            if let Some(number) = process_constant_i64(design, id) {
                let width = design.process_ir.values[id.0 as usize]
                    .bit_width
                    .unwrap_or(64);
                if let Some(width) = std::num::NonZeroU32::new(width) {
                    if let Ok(ty) = debug.context.custom_width_int_type(width) {
                        capture_value(
                            builder,
                            design,
                            cache,
                            id,
                            ty.const_int(number as u64, true),
                        );
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
        argument_builder.set_current_debug_location(debug.location(scope, source.function, None));
        let mut seen = std::collections::HashSet::new();
        for ((id, ..), value) in &arguments.values {
            if seen.insert(*id) {
                capture_binding(&argument_builder, design, cache, *id, *value, true);
            }
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
            source
                .aggregate
                .is_none()
                .then_some(source.result_layout.as_ref())
                .flatten(),
            cache,
        );
    }
    cache.value_frames.pop();
    builder.set_current_debug_location(debug.location(scope, result_span, None));
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
    cache: &ProcessValueCache<'ctx, '_>,
) {
    let Some(debug) = cache.debug else { return };
    let Some(layout) = layout
        .map(std::borrow::Cow::Borrowed)
        .or_else(|| debug.value_debug_layout(design, id))
    else {
        return;
    };
    // A constant/arithmetic return can otherwise have no machine range even
    // at -O0. These inspection writes never become simulation operands.
    let _location = ValueLocationGuard::new(builder, cache, span);
    if let Some(bytes) = inspection_bits(&layout).and_then(|bits| u32::try_from(bits / 8).ok()) {
        let buffer = debug.inspection_buffer(module, "sx.debug.return", bytes);
        if let Some(value) = fit(builder, value, layout.packed_width().unwrap()) {
            debug.copy_fields(
                builder,
                buffer.as_pointer_value(),
                value,
                &layout,
                &mut 0,
                &mut 0,
            );
        }
    }
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
        capture_result(module, builder, design, id, value, span, layout, cache);
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
            let metadata = if let ValueInspection::Logic(layout) = &binding.inspection {
                // Body-local plane values do not dominate the entry insertion
                // point. Keep that companion unavailable until normal emission.
                if at_entry {
                    continue;
                }
                // A narrow consumer cannot establish the binding's omitted
                // numeric bits, even if its complete metadata plane is known.
                if value.get_type().get_bit_width() < layout.packed_width().unwrap() {
                    continue;
                }
                let metadata =
                    cache
                        .metadata
                        .iter()
                        .find_map(|((value_id, active, source), value)| {
                            (*value_id == id && active.is_none() && source.kind == layout.kind)
                                .then_some(*value)
                        });
                if metadata.is_none()
                    && !cache.meta_free.get(id.0 as usize).copied().unwrap_or(false)
                {
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
                    debug.capture_string(builder, *query, *root, value, binding.buffer);
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
