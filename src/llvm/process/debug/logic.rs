//! Source-enum inspection of packed value/metadata planes; never raw nibbles.

use super::*;

pub(super) enum LogicSource {
    Signal(SignalId),
    State(String),
}

impl LogicSource {
    fn load<'ctx>(
        &self,
        context: &'ctx Context,
        module: &Module<'ctx>,
        builder: &Builder<'ctx>,
        design: &Design,
        width: u32,
    ) -> IntValue<'ctx> {
        match self {
            Self::Signal(signal) => signal_value(
                context,
                module,
                builder,
                design,
                &[*signal],
                ProcessSignalState::Current,
                width,
            ),
            Self::State(name) => state_value(context, module, builder, name, width),
        }
        .expect("declared inspection source")
    }
}

pub(super) struct LogicMirror<'ctx> {
    source: LogicSource,
    metadata: Option<LogicSource>,
    layout: SourceLayout,
    buffer: GlobalValue<'ctx>,
}

/// Replace only packed enum leaves; keep struct fields, array labels and all
/// other scalar types. This layout belongs to debugger snapshots, not the IR.
fn inspection_layout(design: &Design, layout: &SourceLayout) -> Option<SourceLayout> {
    let mut result = layout.clone();
    match &mut result.kind {
        LayoutKind::Packed {
            element_enum: Some(name),
            range: Some(range),
            ..
        } if design.logic_encodings.contains_key(name) => {
            result.kind = LayoutKind::Array {
                range: Some(*range),
                element: Box::new(SourceLayout {
                    span: layout.span,
                    kind: LayoutKind::Scalar {
                        width: 8, // Existing packed metadata ABI: four-bit enum discriminants.
                        domain: ScalarDomain::Enum(name.clone()),
                        nominal: Some(name.clone()),
                        value_range: None,
                    },
                }),
            };
        }
        LayoutKind::Struct { fields, .. } => {
            for field in fields {
                field.layout = inspection_layout(design, &field.layout)?;
            }
        }
        LayoutKind::Array { element, .. } => {
            **element = inspection_layout(design, element)?;
        }
        _ => {}
    }
    Some(result)
}

impl<'ctx> DebugInfo<'ctx, '_> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn logic_global(
        &self,
        module: &Module<'ctx>,
        design: &Design,
        layout: &SourceLayout,
        scope: DIScope<'ctx>,
        name: &str,
        span: Span,
        source: LogicSource,
        metadata: Option<LogicSource>,
    ) {
        if !layout_has_packed_metadata(design, layout) {
            return;
        }
        let Some(view) = inspection_layout(design, layout) else {
            return;
        };
        let Some(ty) = self.ty(design, &view, true) else {
            return;
        };
        let bytes =
            u32::try_from(inspection_bits(&view).unwrap() / 8).expect("validated inspection width");
        let buffer_name = format!("sx.logic.debug.{}", self.logic_mirrors.borrow().len());
        let buffer = self.inspection_buffer(module, &buffer_name, bytes);
        self.logic_mirrors.borrow_mut().push(LogicMirror {
            source,
            metadata,
            layout: layout.clone(),
            buffer,
        });
        let info = self.builder.create_global_variable_expression(
            scope,
            &format!("{name}$logic"),
            "",
            self.file(span.file),
            self.sources.line_col(span.file, span.start).0,
            ty,
            true,
            None,
            None,
            0,
        );
        self.attach_global(module, &buffer_name, info);
    }

    pub(super) fn refresh_logic(
        &self,
        module: &Module<'ctx>,
        design: &Design,
        builder: &Builder<'ctx>,
    ) {
        for mirror in self.logic_mirrors.borrow().iter() {
            let width = mirror.layout.packed_width().unwrap();
            let value = mirror
                .source
                .load(self.context, module, builder, design, width);
            let metadata = mirror.metadata.as_ref().map(|source| {
                source.load(
                    self.context,
                    module,
                    builder,
                    design,
                    width.checked_mul(4).unwrap(),
                )
            });
            self.copy_logic_fields(
                builder,
                design,
                mirror.buffer,
                value,
                metadata,
                &mirror.layout,
                &mut 0,
                &mut 0,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn copy_logic_fields(
        &self,
        builder: &Builder<'ctx>,
        design: &Design,
        buffer: GlobalValue<'ctx>,
        value: IntValue<'ctx>,
        metadata: Option<IntValue<'ctx>>,
        layout: &SourceLayout,
        logical: &mut u32,
        byte: &mut u32,
    ) {
        match &layout.kind {
            LayoutKind::Packed {
                width,
                range: Some(range),
                ..
            } if packed_logic_layout(design, layout).is_some() => {
                let (_, encoding) = packed_logic_layout(design, layout).unwrap();
                for ordinal in 0..*width {
                    let label = if range.ascending() {
                        range.left + i64::from(ordinal)
                    } else {
                        range.left - i64::from(ordinal)
                    };
                    let position = *logical + packed_label_position(*range, label).unwrap();
                    let bit = extract_region(builder, value, position, 1).unwrap();
                    let meta = metadata
                        .map(|value| extract_region(builder, value, position * 4, 4).unwrap());
                    let discriminant =
                        packed_discriminant(self.context, builder, encoding, bit, meta, 8).unwrap();
                    let pointer = unsafe {
                        builder
                            .build_in_bounds_gep(
                                self.context.i8_type(),
                                buffer.as_pointer_value(),
                                &[self.context.i64_type().const_int(u64::from(*byte), false)],
                                "debug.logic.element",
                            )
                            .unwrap()
                    };
                    builder
                        .build_store(pointer, discriminant)
                        .unwrap()
                        .set_alignment(1)
                        .unwrap();
                    *byte += 1;
                }
                *logical += width;
            }
            LayoutKind::Struct { fields, .. } => {
                for field in fields {
                    self.copy_logic_fields(
                        builder,
                        design,
                        buffer,
                        value,
                        metadata,
                        &field.layout,
                        logical,
                        byte,
                    );
                }
            }
            LayoutKind::Array {
                range: Some(range),
                element,
            } => {
                for _ in 0..range.len().unwrap() {
                    self.copy_logic_fields(
                        builder, design, buffer, value, metadata, element, logical, byte,
                    );
                }
            }
            _ => self.copy_fields(builder, buffer, value, layout, logical, byte),
        }
    }
}
