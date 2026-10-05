//! Fail-closed support checks for values and runtime instructions.

use super::*;

/// Immutable arena facts shared by support preflight and every emitted CFG.
pub(super) struct ProcessValueSupport {
    pub(super) values: Vec<bool>,
    pub(super) meta_free: Vec<bool>,
}

impl std::ops::Deref for ProcessValueSupport {
    type Target = [bool];

    fn deref(&self) -> &Self::Target {
        &self.values
    }
}

/// Assertions, warnings, and prints use a deliberately small fixed ABI.
/// Accept formatting only when finalized Process metadata is sufficient to
/// render the value without consulting source syntax.
pub(super) fn runtime_instruction_supported(
    design: &Design,
    operation: &ProcessRuntimeOp,
    arguments: &[ProcessValueId],
    format: &Option<Vec<ProcessFormatPart>>,
    values: &[bool],
) -> bool {
    let format_supported = |parts: &[ProcessFormatPart]| {
        parts.iter().all(|part| match part {
            ProcessFormatPart::Text(_) => true,
            ProcessFormatPart::Value {
                value,
                kind: ProcessDisplayKind::String,
            } => {
                process_string(design, *value).is_some()
                    || process_empty_string(design, *value)
                    || values.get(value.0 as usize).copied().unwrap_or(false)
                        && process_value_layout(design, *value)
                            .and_then(process_fixed_string_layout)
                            .is_some()
            }
            ProcessFormatPart::Value { value, kind } => {
                values.get(value.0 as usize).copied().unwrap_or(false)
                    && design
                        .process_ir
                        .values
                        .get(value.0 as usize)
                        .and_then(|value| value.bit_width)
                        .is_some_and(|width| {
                            width > 0
                                && width <= super::super::emit::LLVM_MAX_INT_BITS
                                && match kind {
                                    ProcessDisplayKind::Real => width == 64,
                                    ProcessDisplayKind::Character => width <= 32,
                                    ProcessDisplayKind::Enum(name) => {
                                        width <= 64 && design.enum_syms.contains_key(name)
                                    }
                                    ProcessDisplayKind::Unsigned | ProcessDisplayKind::Signed => {
                                        true
                                    }
                                    ProcessDisplayKind::String => false,
                                }
                        })
            }
        })
    };
    let condition = arguments
        .first()
        .and_then(|id| values.get(id.0 as usize))
        .copied()
        .unwrap_or(false);
    match operation {
        ProcessRuntimeOp::Assert | ProcessRuntimeOp::Warn => {
            condition
                && format.as_ref().map_or_else(
                    || {
                        arguments.len() <= 2
                            && arguments
                                .get(1)
                                .is_none_or(|message| process_string(design, *message).is_some())
                    },
                    |format| format_supported(format),
                )
        }
        ProcessRuntimeOp::Print => format.as_ref().map_or_else(
            || arguments.len() == 1 && process_string(design, arguments[0]).is_some(),
            |format| format_supported(format),
        ),
        ProcessRuntimeOp::Seed => {
            format.is_none()
                && arguments.len() == 1
                && arguments.first().is_some_and(|argument| {
                    values.get(argument.0 as usize).copied().unwrap_or(false)
                        && design
                            .process_ir
                            .values
                            .get(argument.0 as usize)
                            .and_then(|value| value.bit_width)
                            .is_some_and(|width| (1..=64).contains(&width))
                })
        }
        ProcessRuntimeOp::Call(_) => false,
    }
}

/// Emit externally visible `const char *const[]` test names. The logical
/// length is always `sx_test_count`; an empty design carries one null sentinel.
pub(super) fn test_name_table<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    design: &Design,
) {
    let pointer = context.ptr_type(AddressSpace::default());
    let values = design
        .process_ir
        .tests
        .iter()
        .enumerate()
        .map(|(index, test)| {
            private_string(
                context,
                module,
                &format!("sx.test.name.{index}"),
                &test.qualified_name,
            )
        })
        .collect::<Vec<_>>();
    let fallback = [pointer.const_null()];
    let initializer = pointer.const_array(if values.is_empty() {
        &fallback
    } else {
        &values
    });
    let global = module.add_global(initializer.get_type(), None, "sx_test_names");
    global.set_initializer(&initializer);
    global.set_constant(true);
}

/// Compute which arena nodes the direct scalar emitter can lower. Values are
/// dependency ordered, so this stays iterative and cannot overflow the Rust
/// stack on a large generated expression graph.
pub(super) fn process_value_supported_in_layout(
    design: &Design,
    id: ProcessValueId,
    layout: &SourceLayout,
    supported: &ProcessValueSupport,
) -> bool {
    let Some(width) = process_value_width_in_layout(design, id, layout) else {
        return false;
    };
    if width > super::super::emit::LLVM_MAX_INT_BITS {
        return false;
    }
    let Some(value) = design.process_ir.values.get(id.0 as usize) else {
        return false;
    };
    let has = |id: ProcessValueId| supported.get(id.0 as usize).copied().unwrap_or(false);
    match &value.kind {
        ProcessValueKind::Default => true,
        ProcessValueKind::Storage(storage) => {
            storage_state_width(design, *storage).is_some_and(|storage_width| {
                storage_width == width
                    || matches!(
                        layout.kind,
                        LayoutKind::Scalar { .. } | LayoutKind::Packed { .. }
                    ) && has(id)
            })
        }
        ProcessValueKind::StorageState {
            storage,
            state: ProcessSignalState::Old,
        } => storage_state_width(design, *storage) == Some(width),
        ProcessValueKind::StorageState { .. } => false,
        ProcessValueKind::Local { process, local } => local_width(design, *process, *local)
            .is_some_and(|local_width| {
                local_width == width
                    || matches!(
                        layout.kind,
                        LayoutKind::Scalar { .. } | LayoutKind::Packed { .. }
                    ) && has(id)
            }),
        ProcessValueKind::Signal { signals, state } => {
            if matches!(state, ProcessSignalState::Event) {
                width == 1 && !signals.is_empty()
            } else {
                let stored_width = signals.iter().try_fold(0u32, |total, signal| {
                    total.checked_add(design.signal_width(*signal)?)
                });
                stored_width == Some(width)
                    || signals.len() == 1
                        && stored_width.is_some()
                        && matches!(
                            layout.kind,
                            LayoutKind::Scalar {
                                domain: siox::ir::ScalarDomain::Integer,
                                ..
                            }
                        )
                        && has(id)
            }
        }
        ProcessValueKind::Field { base, field } => {
            let Some(base_layout) = process_value_layout(design, *base) else {
                return false;
            };
            field_slice(base_layout, field).is_some_and(|selected| {
                selected.width == width
                    && process_value_supported_in_layout(design, *base, base_layout, supported)
            })
        }
        ProcessValueKind::Index { base, index } => {
            let Some(base_layout) = process_value_layout(design, *base) else {
                return false;
            };
            let selected = match process_constant_i64(design, *index) {
                Some(index) => array_slice(base_layout, index),
                None => match &base_layout.kind {
                    LayoutKind::Array { element, .. } => Some(LayoutSlice {
                        layout: element,
                        offset: 0,
                        width: layout_width(element).unwrap_or(0),
                    }),
                    _ => None,
                },
            };
            has(*index)
                && selected.is_some_and(|selected| {
                    selected.width == width
                        && process_value_supported_in_layout(design, *base, base_layout, supported)
                })
        }
        ProcessValueKind::String(text) => {
            if let LayoutKind::Packed {
                width,
                element_enum: Some(element),
                ..
            } = &layout.kind
            {
                return u32::try_from(text.chars().count()).ok() == Some(*width)
                    && design.logic_encodings.contains_key(element)
                    && design.enum_syms.get(element).is_some_and(|symbols| {
                        text.chars().all(|character| {
                            let quoted = format!("'{character}'");
                            symbols
                                .values()
                                .any(|symbol| symbol == &quoted || symbol == &character.to_string())
                        })
                    });
            }
            let LayoutKind::Array {
                range: Some(range),
                element,
            } = &layout.kind
            else {
                return false;
            };
            range.len().and_then(|length| usize::try_from(length).ok())
                == Some(text.chars().count())
                && matches!(
                    element.kind,
                    LayoutKind::Scalar {
                        domain: siox::ir::ScalarDomain::Character,
                        ..
                    }
                )
                && layout_width(element).is_some()
        }
        ProcessValueKind::Construct { fields, spread, .. } => {
            let LayoutKind::Struct {
                fields: layout_fields,
                ..
            } = &layout.kind
            else {
                return false;
            };
            if spread.is_some_and(|spread| {
                !process_value_supported_in_layout(design, spread, layout, supported)
            }) {
                return false;
            }
            let mut positional = 0usize;
            fields.iter().all(|field| {
                let index = match &field.name {
                    Some(name) => layout_fields
                        .iter()
                        .position(|candidate| candidate.name == *name),
                    None => {
                        let index = positional;
                        positional = positional.saturating_add(1);
                        Some(index)
                    }
                };
                index
                    .and_then(|index| layout_fields.get(index))
                    .zip(field.value)
                    .is_some_and(|(field_layout, value)| {
                        process_value_supported_in_layout(
                            design,
                            value,
                            &field_layout.layout,
                            supported,
                        )
                    })
            })
        }
        ProcessValueKind::Array(elements) => {
            let LayoutKind::Array {
                range: Some(range),
                element,
            } = &layout.kind
            else {
                return false;
            };
            range.len().and_then(|len| usize::try_from(len).ok()) == Some(elements.len())
                && elements.iter().all(|element_value| {
                    process_value_supported_in_layout(design, *element_value, element, supported)
                })
        }
        ProcessValueKind::Select {
            condition,
            then_value,
            else_value,
        } => {
            has(*condition)
                && process_value_supported_in_layout(design, *then_value, layout, supported)
                && process_value_supported_in_layout(design, *else_value, layout, supported)
        }
        ProcessValueKind::Match { scrutinee, arms } => {
            has(*scrutinee)
                && !arms.is_empty()
                && arms.iter().all(|arm| {
                    process_pattern_supported(&arm.pattern)
                        && process_value_supported_in_layout(design, arm.value, layout, supported)
                })
        }
        _ => has(id),
    }
}

pub(super) fn process_packed_meta_supported(
    design: &Design,
    id: ProcessValueId,
    layout: &SourceLayout,
    supported: &ProcessValueSupport,
) -> bool {
    let Some((width, _)) = packed_logic_layout(design, layout) else {
        return false;
    };
    let Some(value) = design.process_ir.values.get(id.0 as usize) else {
        return false;
    };
    if width == 1 && process_scalar_logic_encoding(design, id).is_some() {
        return supported.get(id.0 as usize).copied().unwrap_or(false);
    }
    if supported
        .meta_free
        .get(id.0 as usize)
        .copied()
        .unwrap_or(false)
    {
        return true;
    }
    if aggregate_metadata_projection(design, id) {
        return process_aggregate_projection_meta_supported(design, id, layout, supported);
    }
    match &value.kind {
        ProcessValueKind::Number(_)
        | ProcessValueKind::BitString { .. }
        | ProcessValueKind::Default => true,
        ProcessValueKind::String(text) => {
            let LayoutKind::Packed {
                element_enum: Some(element),
                ..
            } = &layout.kind
            else {
                return false;
            };
            u32::try_from(text.chars().count()).ok() == Some(width)
                && design.logic_encodings.contains_key(element)
                && design.enum_syms.get(element).is_some_and(|symbols| {
                    text.chars().all(|character| {
                        let quoted = format!("'{character}'");
                        symbols
                            .values()
                            .any(|symbol| symbol == &quoted || symbol == &character.to_string())
                    })
                })
        }
        ProcessValueKind::Storage(storage) => {
            storage_meta_width(design, *storage) == width.checked_mul(4)
        }
        ProcessValueKind::StorageState {
            storage,
            state: ProcessSignalState::Old,
        } => storage_meta_width(design, *storage) == width.checked_mul(4),
        ProcessValueKind::Local { process, local } => {
            local_meta_width(design, *process, *local) == width.checked_mul(4)
        }
        ProcessValueKind::Signal { signals, state } => {
            !matches!(state, ProcessSignalState::Event)
                && signals.iter().try_fold(0u32, |total, signal| {
                    total.checked_add(design.signal_width(*signal)?)
                }) == Some(width)
        }
        ProcessValueKind::Unary {
            operation: ProcessUnaryOp::Not,
            operand,
        } => process_packed_meta_supported(design, *operand, layout, supported),
        ProcessValueKind::Unary {
            operation: ProcessUnaryOp::Neg,
            operand,
        } => packed_arithmetic_operand_layout(design, layout, *operand).is_some_and(
            |operand_layout| {
                process_packed_meta_supported(design, *operand, &operand_layout, supported)
            },
        ),
        ProcessValueKind::Unary {
            operation: ProcessUnaryOp::RealToInteger | ProcessUnaryOp::IntegerToReal,
            ..
        } => false,
        ProcessValueKind::Binary {
            operation,
            left,
            right,
        } => match operation {
            ProcessBinaryOp::And | ProcessBinaryOp::Or | ProcessBinaryOp::Xor => {
                process_packed_meta_supported(design, *left, layout, supported)
                    && process_packed_meta_supported(design, *right, layout, supported)
            }
            ProcessBinaryOp::Add
            | ProcessBinaryOp::Sub
            | ProcessBinaryOp::Mul
            | ProcessBinaryOp::Div
            | ProcessBinaryOp::SignedAdd
            | ProcessBinaryOp::SignedSub
            | ProcessBinaryOp::SignedMul
            | ProcessBinaryOp::SignedDiv => {
                packed_arithmetic_operand_layout(design, layout, *left).is_some_and(|left_layout| {
                    process_packed_meta_supported(design, *left, &left_layout, supported)
                }) && packed_arithmetic_operand_layout(design, layout, *right).is_some_and(
                    |right_layout| {
                        process_packed_meta_supported(design, *right, &right_layout, supported)
                    },
                )
            }
            ProcessBinaryOp::Shl | ProcessBinaryOp::Shr => {
                packed_arithmetic_operand_layout(design, layout, *left).is_some_and(|left_layout| {
                    process_packed_meta_supported(design, *left, &left_layout, supported)
                }) && supported.get(right.0 as usize).copied().unwrap_or(false)
            }
            ProcessBinaryOp::ArithmeticShr => {
                packed_arithmetic_operand_layout(design, layout, *left).is_some_and(|left_layout| {
                    process_packed_meta_supported(design, *left, &left_layout, supported)
                }) && process_constant_i64(design, *right).is_some_and(|right| right >= 0)
            }
            _ => false,
        },
        ProcessValueKind::PackedSlice { base, .. } => process_value_layout(design, *base)
            .is_some_and(|base_layout| {
                process_packed_meta_supported(design, *base, base_layout, supported)
            }),
        ProcessValueKind::Index { base, index } if width == 1 => {
            supported.get(index.0 as usize).copied().unwrap_or(false)
                && process_value_layout(design, *base).is_some_and(|base_layout| {
                    packed_logic_layout(design, base_layout).is_some()
                        && process_packed_meta_supported(design, *base, base_layout, supported)
                })
        }
        ProcessValueKind::Select {
            condition,
            then_value,
            else_value,
        } => {
            supported
                .get(condition.0 as usize)
                .copied()
                .unwrap_or(false)
                && process_packed_meta_supported(design, *then_value, layout, supported)
                && process_packed_meta_supported(design, *else_value, layout, supported)
        }
        ProcessValueKind::Match { scrutinee, arms } => {
            supported
                .get(scrutinee.0 as usize)
                .copied()
                .unwrap_or(false)
                && !arms.is_empty()
                && arms.iter().all(|arm| {
                    process_pattern_supported(&arm.pattern)
                        && process_packed_meta_supported(design, arm.value, layout, supported)
                })
        }
        ProcessValueKind::Concat(parts) => {
            let LayoutKind::Packed {
                family,
                element_enum,
                ..
            } = &layout.kind
            else {
                return false;
            };
            parts.iter().all(|part| {
                let Some(part_value) = design.process_ir.values.get(part.0 as usize) else {
                    return false;
                };
                let Some(part_width) = part_value.bit_width else {
                    return false;
                };
                let part_layout = SourceLayout {
                    span: part_value.span,
                    kind: LayoutKind::Packed {
                        width: part_width,
                        family: family.clone(),
                        range: Some(LayoutRange {
                            left: 0,
                            right: i64::from(part_width) - 1,
                        }),
                        element_enum: element_enum.clone(),
                    },
                };
                process_packed_meta_supported(design, *part, &part_layout, supported)
            })
        }
        ProcessValueKind::RawResize { operand } => packed_operand_layout(design, layout, *operand)
            .is_some_and(|operand_layout| {
                process_packed_meta_supported(design, *operand, &operand_layout, supported)
            }),
        _ => false,
    }
}

pub(super) fn process_value_supported_for_target(
    design: &Design,
    target: ProcessValueId,
    assigned: ProcessValueId,
    supported: &ProcessValueSupport,
) -> bool {
    let value_supported = match process_value_layout(design, target) {
        Some(layout) => process_value_supported_in_layout(design, assigned, layout, supported),
        None => supported.get(assigned.0 as usize).copied().unwrap_or(false),
    };
    if !value_supported {
        return false;
    }
    let root = static_place(design, target)
        .map(|place| place.root)
        .or_else(|| dynamic_place(design, target).map(|place| place.root));
    let Some(root @ (StaticPlaceRoot::Local(_, _) | StaticPlaceRoot::Storage(_))) = root else {
        return true;
    };
    let Some(root_layout) = place_root_layout(design, root) else {
        return true;
    };
    if !layout_has_packed_metadata(design, root_layout) {
        return true;
    }
    if packed_bit_place_layout(design, target).is_some() {
        return design
            .process_ir
            .values
            .get(assigned.0 as usize)
            .and_then(|value| value.bit_width)
            .is_some_and(|width| width != 0 && width <= 4);
    }
    process_value_layout(design, target)
        .is_some_and(|layout| process_value_meta_supported(design, assigned, layout, supported))
}

pub(super) fn supported_process_values(design: &Design) -> ProcessValueSupport {
    let mut supported = ProcessValueSupport {
        values: Vec::with_capacity(design.process_ir.values.len()),
        meta_free: meta_free_process_values(design),
    };
    let has = |supported: &[bool], id: ProcessValueId| {
        supported.get(id.0 as usize).copied().unwrap_or(false)
    };
    for (index, value) in design.process_ir.values.iter().enumerate() {
        let id = ProcessValueId(index as u32);
        let packed_width = value
            .bit_width
            .is_some_and(|width| width != 0 && width <= super::super::emit::LLVM_MAX_INT_BITS);
        let shape = match &value.kind {
            ProcessValueKind::Number(_)
            | ProcessValueKind::BitString { .. }
            | ProcessValueKind::Char(_)
            | ProcessValueKind::Default => true,
            ProcessValueKind::Signal { signals, state } => {
                !signals.is_empty()
                    && signals
                        .iter()
                        .all(|signal| design.signals.get(signal.0 as usize).is_some())
                    && if matches!(state, ProcessSignalState::Event) {
                        value.bit_width == Some(1)
                    } else {
                        let stored_width = signals.iter().try_fold(0u32, |width, signal| {
                            width.checked_add(design.signal_width(*signal)?)
                        });
                        stored_width == value.bit_width
                            || signals.len() == 1
                                && matches!(value.ty, Some(siox::types::Ty::Integer))
                                && stored_width.is_some()
                    }
            }
            ProcessValueKind::BitSlice { base, high, low } => {
                has(&supported, *base)
                    && low <= high
                    && high
                        .checked_sub(*low)
                        .and_then(|width| width.checked_add(1))
                        == value.bit_width
            }
            ProcessValueKind::PackedSlice { base, left, right } => {
                let width = left
                    .abs_diff(*right)
                    .checked_add(1)
                    .and_then(|width| u32::try_from(width).ok());
                has(&supported, *base)
                    && width == value.bit_width
                    && process_value_layout(design, *base).is_some_and(|layout| {
                        matches!(layout.kind, LayoutKind::Packed { range: Some(range), .. }
                            if packed_label_position(range, *left).is_some()
                                && packed_label_position(range, *right).is_some())
                    })
            }
            ProcessValueKind::CheckedIndex { index, valid, .. } => {
                has(&supported, *index)
                    && has(&supported, *valid)
                    && value.bit_width
                        == design
                            .process_ir
                            .values
                            .get(index.0 as usize)
                            .and_then(|index| index.bit_width)
            }
            ProcessValueKind::TableLookup { table, index } => {
                has(&supported, *index)
                    && design.lookup_tables.get(table.0).is_some_and(|table| {
                        !table.values.is_empty()
                            && table.values.len() <= u32::MAX as usize
                            && (1..=64).contains(&table.element_width)
                    })
            }
            ProcessValueKind::ForeignCall {
                arguments,
                float_arguments,
                integer_arguments,
                ..
            } => {
                arguments.len() == float_arguments.len()
                    && arguments.len() == integer_arguments.len()
                    && arguments.iter().all(|argument| has(&supported, *argument))
            }
            ProcessValueKind::HostCall {
                operation,
                arguments,
            } => {
                let scalar = |argument: ProcessValueId| {
                    has(&supported, argument)
                        && design
                            .process_ir
                            .values
                            .get(argument.0 as usize)
                            .and_then(|argument| argument.bit_width)
                            .is_some_and(|width| (1..=64).contains(&width))
                };
                match (operation, arguments.as_slice()) {
                    (ProcessHostValueOp::Random | ProcessHostValueOp::Uniform, []) => true,
                    (ProcessHostValueOp::RandomRange, [left, right]) => {
                        scalar(*left) && scalar(*right)
                    }
                    (
                        ProcessHostValueOp::ReadUtf8
                        | ProcessHostValueOp::ReadUtf8Fixed
                        | ProcessHostValueOp::ReadBinary
                        | ProcessHostValueOp::FileExists,
                        [path],
                    ) => process_string(design, *path).is_some(),
                    (ProcessHostValueOp::StringLength, [handle]) => scalar(*handle),
                    (ProcessHostValueOp::StringIndex, [handle, index]) => {
                        scalar(*handle) && scalar(*index)
                    }
                    (ProcessHostValueOp::StringEqualsUtf8, [handle, literal]) => {
                        scalar(*handle) && process_string(design, *literal).is_some()
                    }
                    _ => false,
                }
            }
            ProcessValueKind::Local { process, local } => {
                local_width(design, *process, *local).is_some() && value.bit_width.is_some()
            }
            ProcessValueKind::Storage(storage) => {
                storage_state_width(design, *storage).is_some()
                    && value.bit_width.is_some()
                    && design
                        .process_ir
                        .storages
                        .get(storage.0 as usize)
                        .is_some_and(|storage| {
                            storage.initializer.is_none_or(|initializer| {
                                storage.layout.as_ref().map_or_else(
                                    || has(&supported, initializer),
                                    |layout| {
                                        if layout_width(layout).is_none() {
                                            has(&supported, initializer)
                                        } else {
                                            process_value_supported_in_layout(
                                                design,
                                                initializer,
                                                layout,
                                                &supported,
                                            ) && (!layout_has_packed_metadata(design, layout)
                                                || process_value_meta_supported(
                                                    design,
                                                    initializer,
                                                    layout,
                                                    &supported,
                                                ))
                                        }
                                    },
                                )
                            })
                        })
            }
            ProcessValueKind::StorageState { storage, state } => match state {
                ProcessSignalState::Current => false,
                ProcessSignalState::Old => {
                    storage_state_width(design, *storage).is_some() && value.bit_width.is_some()
                }
                ProcessSignalState::Event => {
                    design.process_ir.storages.get(storage.0 as usize).is_some()
                        && value.bit_width == Some(1)
                }
            },
            ProcessValueKind::Unary { operand, .. } | ProcessValueKind::RawResize { operand } => {
                has(&supported, *operand)
            }
            ProcessValueKind::Binary {
                operation,
                left,
                right,
            } => {
                process_empty_string_comparison(design, operation, *left, *right).is_some()
                    || !matches!(operation, ProcessBinaryOp::Custom(_))
                        && has(&supported, *left)
                        && has(&supported, *right)
            }
            ProcessValueKind::Select {
                condition,
                then_value,
                else_value,
            } => {
                has(&supported, *condition)
                    && has(&supported, *then_value)
                    && has(&supported, *else_value)
            }
            ProcessValueKind::MetaCompare {
                operands, inner, ..
            } => {
                has(&supported, *inner)
                    && !operands.is_empty()
                    && operands
                        .iter()
                        .all(|operand| process_meta_operand_supported(design, *operand, &supported))
            }
            ProcessValueKind::Match { scrutinee, arms } => {
                has(&supported, *scrutinee)
                    && !arms.is_empty()
                    && process_value_layout(design, id).map_or_else(
                        || {
                            arms.iter().all(|arm| {
                                process_pattern_supported(&arm.pattern)
                                    && has(&supported, arm.value)
                            })
                        },
                        |layout| {
                            arms.iter().all(|arm| {
                                process_pattern_supported(&arm.pattern)
                                    && process_value_supported_in_layout(
                                        design, arm.value, layout, &supported,
                                    )
                            })
                        },
                    )
            }
            ProcessValueKind::Attribute { base, attribute } => {
                process_layout_attribute(design, *base, attribute).is_some()
            }
            ProcessValueKind::Field { base, .. } => {
                has(&supported, *base) && process_value_layout(design, id).is_some()
            }
            ProcessValueKind::Index { base, index } => {
                if !has(&supported, *base) || !has(&supported, *index) {
                    false
                } else {
                    let selected = process_constant_i64(design, *index);
                    match process_value_layout(design, *base) {
                        Some(
                            layout @ SourceLayout {
                                kind: LayoutKind::Array { .. },
                                ..
                            },
                        ) => selected.map_or_else(
                            || {
                                matches!(
                                    &layout.kind,
                                    LayoutKind::Array { element, .. }
                                        if layout_width(element) == value.bit_width
                                )
                            },
                            |index| array_slice(layout, index).is_some(),
                        ),
                        Some(SourceLayout {
                            kind:
                                LayoutKind::Packed {
                                    range: Some(range),
                                    element_enum,
                                    ..
                                },
                            ..
                        }) => {
                            selected.is_none_or(|index| {
                                (range.left.min(range.right)..=range.left.max(range.right))
                                    .contains(&index)
                            }) && match element_enum {
                                Some(element) => {
                                    value.bit_width.is_some_and(|width| width != 0)
                                        && design.logic_encodings.contains_key(element)
                                        && process_value_layout(design, *base).is_some_and(
                                            |layout| {
                                                process_packed_meta_supported(
                                                    design, *base, layout, &supported,
                                                )
                                            },
                                        )
                                }
                                None => value.bit_width == Some(1),
                            }
                        }
                        _ => false,
                    }
                }
            }
            ProcessValueKind::Construct { fields, spread, .. } => {
                process_value_layout(design, id).is_some()
                    && fields
                        .iter()
                        .all(|field| field.value.is_some_and(|value| has(&supported, value)))
                    && spread.is_none_or(|spread| has(&supported, spread))
            }
            ProcessValueKind::Array(elements) => {
                process_value_layout(design, id).is_some()
                    && elements.iter().all(|element| has(&supported, *element))
            }
            ProcessValueKind::Concat(parts) => {
                parts.iter().all(|part| has(&supported, *part))
                    && parts.iter().try_fold(0u32, |width, part| {
                        width.checked_add(design.process_ir.values.get(part.0 as usize)?.bit_width?)
                    }) == value.bit_width
            }
            ProcessValueKind::String(text) => u32::try_from(text.chars().count())
                .ok()
                .and_then(|length| length.checked_mul(32))
                .is_some_and(|width| width != 0 && value.bit_width == Some(width)),
            ProcessValueKind::Suffixed { .. }
            | ProcessValueKind::Definition(_)
            | ProcessValueKind::Intrinsic(_)
            | ProcessValueKind::Range { .. }
            | ProcessValueKind::Call { .. }
            | ProcessValueKind::Invalid => false,
        };
        supported.values.push(packed_width && shape);
    }
    supported
}
