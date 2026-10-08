//! Direct source DWARF for resumable Process entries and their existing state.

use super::*;
use std::cell::{Cell, RefCell};
use std::path::Path;

use inkwell::debug_info::{
    AsDIScope, DICompileUnit, DIFile, DIFlags, DIFlagsConstants, DIGlobalVariableExpression,
    DILocation, DIScope, DISubprogram, DIType, DWARFEmissionKind, DWARFSourceLanguage,
    DebugInfoBuilder,
};
use inkwell::module::FlagBehavior;
use inkwell::targets::TargetData;
use inkwell::values::{AsValueRef, BasicValue, GlobalValue};
use siox::diag::{FileId, SourceMap, Span};
use siox::ir::ScalarDomain;

mod inline_values;
mod logic;
mod parameters;
mod strings;
pub(super) use inline_values::{
    active_value, argument_value, call_value, capture_value, inline_body, inline_value,
    literal_value, return_bits, return_value, selected_value, source_return_type, ValueArguments,
    ValueFrame, ValueLocationGuard,
};
use logic::{LogicMirror, LogicSource};
use parameters::ParameterLocation;
use strings::StringMirror;

// DWARF standard opcodes/encodings, not Siox values or runtime layout limits.
const DW_OP_PLUS_UCONST: i64 = 0x23;
const DW_OP_DEREF_SIZE: i64 = 0x94;
const DW_OP_CONSTU: i64 = 0x10;
const DW_OP_AND: i64 = 0x1a;
const DW_OP_SHL: i64 = 0x24;
const DW_OP_SHR: i64 = 0x25;
const DW_OP_SHRA: i64 = 0x26;
const DW_OP_STACK_VALUE: i64 = 0x9f;
const DW_ATE_FLOAT: u32 = 0x04;
const DW_ATE_SIGNED: u32 = 0x05;
const DW_ATE_UNSIGNED: u32 = 0x07;

type ScopeKey = (usize, Option<u32>);
type LexicalScopes<'ctx> = Vec<(Span, DIScope<'ctx>)>;
type InlineFrame<'ctx> = (DISubprogram<'ctx>, Option<DILocation<'ctx>>);
type ParameterLocations<'ctx> = Vec<Vec<ParameterLocation<'ctx>>>;
pub(super) type ValueFunctionKey = (ProcessValueId, u32, bool, Option<SourceLayout>);
pub(super) type ValueResults<'ctx> = HashMap<ValueFunctionKey, (IntValue<'ctx>, IntValue<'ctx>)>;
type ValueFunction<'ctx> = (
    FunctionValue<'ctx>,
    Vec<ValueFunctionKey>,
    Vec<ProcessValueId>,
);

struct AggregateReturn {
    layout: SourceLayout,
    /// Only retained executable operands, at their declared packed offsets.
    pieces: Vec<(ProcessValueId, u32, SourceLayout)>,
}

pub(in crate::llvm) struct DebugInfo<'ctx, 's> {
    context: &'ctx Context,
    builder: DebugInfoBuilder<'ctx>,
    unit: DICompileUnit<'ctx>,
    sources: &'s SourceMap,
    files: RefCell<HashMap<FileId, DIFile<'ctx>>>,
    namespaces: RefCell<HashMap<String, DIScope<'ctx>>>,
    lexical_scopes: RefCell<HashMap<ScopeKey, LexicalScopes<'ctx>>>,
    file_scopes: RefCell<HashMap<(ScopeKey, FileId), DIScope<'ctx>>>,
    inline_scopes: RefCell<HashMap<usize, Vec<InlineFrame<'ctx>>>>,
    parameter_locations: RefCell<HashMap<usize, ParameterLocations<'ctx>>>,
    parameter_parents: RefCell<HashMap<usize, Vec<Option<u32>>>>,
    target: TargetData,
    mirrors: RefCell<Vec<Mirror<'ctx>>>,
    logic_mirrors: RefCell<Vec<LogicMirror<'ctx>>>,
    string_mirrors: RefCell<Vec<StringMirror<'ctx>>>,
    refresh: Cell<Option<FunctionValue<'ctx>>>,
    /// Calls in otherwise-pure selections still need taken-path source ranges.
    value_activity: Vec<bool>,
    /// Only multiply-consumed source calls need readiness across branch joins.
    reused_calls: std::collections::HashSet<ProcessValueId>,
    aggregate_returns: HashMap<ProcessValueId, AggregateReturn>,
    aggregate_members: HashMap<ProcessValueId, (ProcessValueId, u32, u32)>,
    value_functions: RefCell<HashMap<ValueFunctionKey, ValueFunction<'ctx>>>,
    hardware_processes: HashMap<u32, siox::ir::ProcessId>,
    hardware_frames: RefCell<HashMap<(usize, siox::ir::ProcessId), DISubprogram<'ctx>>>,
}

struct Mirror<'ctx> {
    source: GlobalValue<'ctx>,
    source_byte: u64,
    buffer: GlobalValue<'ctx>,
    layout: SourceLayout,
}

impl<'ctx, 's> DebugInfo<'ctx, 's> {
    pub fn new(
        context: &'ctx Context,
        module: &Module<'ctx>,
        sources: &'s SourceMap,
        design: &Design,
    ) -> Result<Self, String> {
        let source = sources
            .get(FileId(0))
            .ok_or("DWARF requires an entry source file")?;
        let (name, directory) = file_parts(&source.name);
        let (builder, unit) = module.create_debug_info_builder(
            true,
            // Siox has no DWARF language code. C++ gives native debuggers
            // the matching namespace/field expression syntax, not a C backend.
            DWARFSourceLanguage::CPlusPlus,
            &name,
            &directory,
            concat!("sioxc ", env!("CARGO_PKG_VERSION")),
            false,
            "--debug",
            0,
            "",
            DWARFEmissionKind::Full,
            0,
            false,
            false,
            "",
            "",
        );
        module.add_basic_value_flag(
            "Debug Info Version",
            FlagBehavior::Warning,
            context.i32_type().const_int(
                u64::from(inkwell::debug_info::debug_metadata_version()),
                false,
            ),
        );
        module.add_basic_value_flag(
            "Dwarf Version",
            FlagBehavior::Warning,
            context.i32_type().const_int(4, false),
        );
        let machine = crate::llvm::aot::host_target_machine()?;
        let target = machine.get_target_data();
        module.set_triple(&machine.get_triple());
        module.set_data_layout(&target.get_data_layout());
        let mut value_activity = Vec::with_capacity(design.process_ir.values.len());
        let mut uses = vec![0u8; design.process_ir.values.len()];
        for value in &design.process_ir.values {
            crate::ir::for_each_process_value_dependency(&value.kind, |id| {
                if let Some(count) = uses.get_mut(id.0 as usize) {
                    *count = count.saturating_add(1);
                }
            });
            value_activity.push(
                matches!(value.kind, ProcessValueKind::InlineCall { .. })
                    || crate::ir::process::process_value_dependencies(&value.kind)
                        .iter()
                        .any(|id| value_activity.get(id.0 as usize).copied().unwrap_or(false)),
            );
        }
        let (aggregate_returns, aggregate_members) = inline_values::aggregate_returns(design);
        let reused_calls = design
            .process_ir
            .values
            .iter()
            .enumerate()
            .filter(|(index, value)| {
                uses[*index] > 1 && matches!(value.kind, ProcessValueKind::InlineCall { .. })
            })
            .map(|(index, _)| ProcessValueId(index as u32))
            .chain(aggregate_returns.keys().copied())
            .collect();
        let debug = Self {
            context,
            builder,
            unit,
            sources,
            target,
            files: RefCell::new(HashMap::from([(FileId(0), unit.get_file())])),
            namespaces: RefCell::new(HashMap::new()),
            lexical_scopes: RefCell::new(HashMap::new()),
            file_scopes: RefCell::new(HashMap::new()),
            inline_scopes: RefCell::new(HashMap::new()),
            parameter_locations: RefCell::new(HashMap::new()),
            parameter_parents: RefCell::new(HashMap::new()),
            mirrors: RefCell::new(Vec::new()),
            logic_mirrors: RefCell::new(Vec::new()),
            string_mirrors: RefCell::new(Vec::new()),
            refresh: Cell::new(None),
            value_activity,
            reused_calls,
            aggregate_returns,
            aggregate_members,
            value_functions: RefCell::new(HashMap::new()),
            hardware_processes: design
                .process_ir
                .processes
                .iter()
                .filter(|process| process.region != siox::ir::ProcessRegion::Procedural)
                .flat_map(|process| {
                    let event = match process.region {
                        siox::ir::ProcessRegion::Event { driver_context, .. } => {
                            Some((driver_context, process.id))
                        }
                        _ => None,
                    };
                    event
                        .into_iter()
                        .chain(process.blocks.iter().flat_map(move |block| {
                            block.instructions.iter().filter_map(move |instruction| {
                                match instruction {
                                    ProcessInstruction::Assign {
                                        driver_context: Some(context),
                                        ..
                                    } => Some((*context, process.id)),
                                    _ => None,
                                }
                            })
                        }))
                })
                .collect(),
            hardware_frames: RefCell::new(HashMap::new()),
        };
        debug.globals(module, design);
        Ok(debug)
    }

    fn file(&self, file: FileId) -> DIFile<'ctx> {
        if let Some(file) = self.files.borrow().get(&file) {
            return *file;
        }
        let Some(source) = self.sources.get(file) else {
            return self.unit.get_file();
        };
        let (name, directory) = file_parts(&source.name);
        let metadata = self.builder.create_file(&name, &directory);
        self.files.borrow_mut().insert(file, metadata);
        metadata
    }

    fn namespace(&self, path: &str) -> DIScope<'ctx> {
        if path.is_empty() {
            return self.unit.as_debug_info_scope();
        }
        if let Some(scope) = self.namespaces.borrow().get(path) {
            return *scope;
        }
        let (parent, name) = path.rsplit_once('.').unwrap_or(("", path));
        let scope = self
            .builder
            .create_namespace(self.namespace(parent), name, false)
            .as_debug_info_scope();
        self.namespaces.borrow_mut().insert(path.into(), scope);
        scope
    }

    /// A bounded native helper can contain several source hardware processes.
    /// Generated scheduler instructions have a real native scope, but no
    /// source statement. Declare it before emitting any calls in that body.
    pub fn generated_location(
        &self,
        builder: &Builder<'ctx>,
        span: Span,
    ) -> Option<DILocation<'ctx>> {
        let function = builder.get_insert_block()?.get_parent()?;
        let file = self.file(span.file);
        let root = function.get_subprogram().unwrap_or_else(|| {
            let root = self.builder.create_function(
                self.unit.as_debug_info_scope(),
                &function.get_name().to_string_lossy(),
                Some(""),
                file,
                0,
                self.builder
                    .create_subroutine_type(file, None, &[], DIFlags::ZERO),
                true,
                true,
                0,
                DIFlags::ZERO,
                false,
            );
            function.set_subprogram(root);
            root
        });
        Some(self.builder.create_debug_location(
            self.context,
            0,
            0,
            root.as_debug_info_scope(),
            None,
        ))
    }

    /// Keep each process as its own logical frame, not the helper's first owner.
    pub fn hardware_location(
        &self,
        builder: &Builder<'ctx>,
        design: &Design,
        context: u32,
        span: Option<Span>,
    ) -> Option<DILocation<'ctx>> {
        let process = design
            .process_ir
            .processes
            .get(self.hardware_processes.get(&context)?.0 as usize)?;
        let caller = self.generated_location(builder, process.span)?;
        let root = builder.get_insert_block()?.get_parent()?.get_subprogram()?;
        let file = self.file(process.span.file);
        let line = self
            .sources
            .line_col(process.span.file, process.span.start)
            .0;
        let key = (root.as_mut_ptr() as usize, process.id);
        let scope = self.hardware_frames.borrow().get(&key).copied();
        let scope = scope.unwrap_or_else(|| {
            let label = process
                .label
                .clone()
                .unwrap_or_else(|| format!("process {}", process.id.0));
            let (owner, name) = label.rsplit_once("::").unwrap_or(("", &label));
            let scope = self.builder.create_function(
                self.namespace(owner),
                name,
                Some(""),
                file,
                line,
                self.builder
                    .create_subroutine_type(file, None, &[], DIFlags::ZERO),
                true,
                true,
                line,
                DIFlags::ZERO,
                false,
            );
            self.hardware_frames.borrow_mut().insert(key, scope);
            scope
        });
        let span = span.unwrap_or(process.span);
        let (line, column) = self.sources.line_col(span.file, span.start);
        Some(self.builder.create_debug_location(
            self.context,
            line,
            column,
            scope.as_debug_info_scope(),
            Some(caller),
        ))
    }

    pub fn location(
        &self,
        root: DISubprogram<'ctx>,
        span: Span,
        inline: Option<u32>,
    ) -> DILocation<'ctx> {
        let (line, column) = self.sources.line_col(span.file, span.start);
        let (scope, caller) = self.source_scope(root, span, inline);
        self.builder
            .create_debug_location(self.context, line, column, scope, caller)
    }

    fn source_scope(
        &self,
        root: DISubprogram<'ctx>,
        span: Span,
        inline: Option<u32>,
    ) -> (DIScope<'ctx>, Option<DILocation<'ctx>>) {
        let key = (root.as_mut_ptr() as usize, inline);
        let frame = inline.and_then(|id| {
            self.inline_scopes
                .borrow()
                .get(&key.0)
                .and_then(|frames| frames.get(id as usize))
                .copied()
        });
        let scope = frame.map_or(root, |(scope, _)| scope);
        let lexical = self.lexical_scopes.borrow().get(&key).and_then(|scopes| {
            scopes
                .iter()
                .filter(|(extent, _)| contains(*extent, span))
                .min_by_key(|(extent, _)| extent.end - extent.start)
                .map(|(_, scope)| *scope)
        });
        let scope = lexical.unwrap_or_else(|| {
            if let Some(scope) = self.file_scopes.borrow().get(&(key, span.file)) {
                return *scope;
            }
            let file_scope = self
                .builder
                .create_lexical_block(scope.as_debug_info_scope(), self.file(span.file), 0, 0)
                .as_debug_info_scope();
            self.file_scopes
                .borrow_mut()
                .insert((key, span.file), file_scope);
            file_scope
        });
        (scope, frame.and_then(|(_, call)| call))
    }

    fn declare_lexical_scopes(
        &self,
        root: DISubprogram<'ctx>,
        mut extents: Vec<(Option<u32>, Span)>,
        frames: &[InlineFrame<'ctx>],
    ) -> Vec<(Option<u32>, Span, DIScope<'ctx>)> {
        extents.sort_by_key(|(inline, span)| {
            (
                *inline,
                std::cmp::Reverse(span.end - span.start),
                span.file.0,
                span.start,
            )
        });
        extents.dedup();
        let mut scopes = Vec::<(Option<u32>, Span, DIScope<'ctx>)>::new();
        for (inline, extent) in extents {
            let base = inline
                .map_or(root, |id| frames[id as usize].0)
                .as_debug_info_scope();
            let parent = scopes
                .iter()
                .filter(|(id, span, _)| *id == inline && contains(*span, extent))
                .min_by_key(|(_, span, _)| span.end - span.start)
                .map_or(base, |(_, _, scope)| *scope);
            let (line, column) = self.sources.line_col(extent.file, extent.start);
            scopes.push((
                inline,
                extent,
                self.builder
                    .create_lexical_block(parent, self.file(extent.file), line, column)
                    .as_debug_info_scope(),
            ));
        }
        for (inline, span, lexical) in &scopes {
            self.lexical_scopes
                .borrow_mut()
                .entry((root.as_mut_ptr() as usize, *inline))
                .or_default()
                .push((*span, *lexical));
        }
        scopes
    }

    fn basic(&self, name: &str, width: u32, encoding: u32) -> DIType<'ctx> {
        self.builder
            .create_basic_type(name, u64::from(width), encoding, DIFlags::ZERO)
            .expect("nonempty debug type name")
            .as_type()
    }

    fn ty(&self, design: &Design, layout: &SourceLayout, normalized: bool) -> Option<DIType<'ctx>> {
        let width = layout.packed_width()?;
        let file = self.file(layout.span.file);
        let line = self.sources.line_col(layout.span.file, layout.span.start).0;
        Some(match &layout.kind {
            LayoutKind::Scalar {
                domain: ScalarDomain::Enum(name),
                ..
            } => {
                let symbols = design.enum_syms.get(name)?;
                let mut symbols = symbols.iter().collect::<Vec<_>>();
                symbols.sort_by_key(|(value, _)| **value);
                let variants = symbols
                    .iter()
                    .map(|(value, name)| self.builder.create_enumerator(name, **value as i64, true))
                    .collect::<Vec<_>>();
                self.builder
                    .create_enumeration_type(
                        self.unit.as_debug_info_scope(),
                        name,
                        file,
                        line,
                        u64::from(width.div_ceil(8) * 8),
                        0,
                        &variants,
                        self.basic("discriminant", width, DW_ATE_UNSIGNED),
                    )
                    .as_type()
            }
            LayoutKind::Scalar {
                domain,
                nominal,
                value_range,
                ..
            } => {
                let (fallback, encoding) = match domain {
                    ScalarDomain::Real => ("real", DW_ATE_FLOAT),
                    ScalarDomain::Integer => (
                        "integer",
                        if value_range.is_some_and(|(left, _)| left >= 0) {
                            DW_ATE_UNSIGNED
                        } else {
                            DW_ATE_SIGNED
                        },
                    ),
                    ScalarDomain::Character => ("Char", DW_ATE_UNSIGNED),
                    _ => ("bits", DW_ATE_UNSIGNED),
                };
                self.basic(nominal.as_deref().unwrap_or(fallback), width, encoding)
            }
            LayoutKind::Packed { family, .. } => self.basic(
                &format!("{family}[{width}]"),
                width,
                if process_layout_is_signed(layout) {
                    DW_ATE_SIGNED
                } else {
                    DW_ATE_UNSIGNED
                },
            ),
            LayoutKind::Struct { name, fields, .. } => {
                let mut offset = 0u64;
                let mut members = Vec::new();
                for field in fields {
                    let ty = self.ty(design, &field.layout, normalized)?;
                    let size = if normalized {
                        inspection_bits(&field.layout)?
                    } else {
                        field.layout.bit_width()?
                    };
                    members.push(
                        self.builder
                            .create_member_type(
                                self.unit.as_debug_info_scope(),
                                &field.name,
                                file,
                                line,
                                size,
                                0,
                                offset,
                                if !offset.is_multiple_of(8) || !size.is_multiple_of(8) {
                                    DIFlags::BIT_FIELD
                                } else {
                                    DIFlags::ZERO
                                },
                                ty,
                            )
                            .as_type(),
                    );
                    offset += size;
                }
                self.builder
                    .create_struct_type(
                        self.unit.as_debug_info_scope(),
                        name,
                        file,
                        line,
                        if normalized {
                            inspection_bits(layout)?
                        } else {
                            u64::from(width.div_ceil(8) * 8)
                        },
                        0,
                        DIFlags::ZERO,
                        None,
                        &members,
                        0,
                        None,
                        "",
                    )
                    .as_type()
            }
            LayoutKind::Array {
                range: Some(range),
                element,
            } if (normalized || element.bit_width()?.is_multiple_of(8)) && range.ascending() => {
                self.builder
                    .create_array_type(
                        self.ty(design, element, normalized)?,
                        if normalized {
                            inspection_bits(layout)?
                        } else {
                            u64::from(width)
                        },
                        0,
                        std::slice::from_ref(&(range.left..range.right.checked_add(1)?)),
                    )
                    .as_type()
            }
            LayoutKind::Array {
                range: Some(range),
                element,
            } => {
                // C++ debuggers assume positive array strides. Preserve
                // descending Siox labels as named elements instead of
                // advertising an array whose indexing reverses the data.
                let element_ty = self.ty(design, element, normalized)?;
                let size = if normalized {
                    inspection_bits(element)?
                } else {
                    element.bit_width()?
                };
                let mut members = Vec::new();
                for ordinal in 0..range.len()? {
                    let index = if range.ascending() {
                        i128::from(range.left) + i128::from(ordinal)
                    } else {
                        i128::from(range.left) - i128::from(ordinal)
                    };
                    let offset = ordinal.checked_mul(size)?;
                    members.push(
                        self.builder
                            .create_member_type(
                                self.unit.as_debug_info_scope(),
                                &format!("[{index}]"),
                                file,
                                line,
                                size,
                                0,
                                offset,
                                if !offset.is_multiple_of(8) || !size.is_multiple_of(8) {
                                    DIFlags::BIT_FIELD
                                } else {
                                    DIFlags::ZERO
                                },
                                element_ty,
                            )
                            .as_type(),
                    );
                }
                self.builder
                    .create_struct_type(
                        self.unit.as_debug_info_scope(),
                        &format!("array[{}..{}]", range.left, range.right),
                        file,
                        line,
                        if normalized {
                            inspection_bits(layout)?
                        } else {
                            u64::from(width.div_ceil(8) * 8)
                        },
                        0,
                        DIFlags::ZERO,
                        None,
                        &members,
                        0,
                        None,
                        "",
                    )
                    .as_type()
            }
            LayoutKind::Array { .. } => return None,
            LayoutKind::Opaque { name, .. } => self.basic(name, width, DW_ATE_UNSIGNED),
        })
    }

    fn signal_bits(&self, _design: &Design, signal: SignalId) -> u64 {
        #[cfg(not(feature = "bitpack"))]
        {
            let state = self
                .context
                .get_struct_type("sx.state")
                .expect("declared signal state");
            self.target
                .offset_of_element(&state, signal.0)
                .expect("signal field")
                * 8
        }
        #[cfg(feature = "bitpack")]
        {
            let (slots, _) = crate::llvm::emit::pack_layout(_design);
            let (word, bit) = slots[signal.0 as usize];
            u64::from(word) * 64 + u64::from(bit)
        }
    }

    fn globals(&self, module: &Module<'ctx>, design: &Design) {
        for (id, signal) in design.signals.iter().enumerate() {
            if design
                .meta_of
                .values()
                .any(|companion| *companion == id as u32)
                || design.metavalue_temps.contains(&(id as u32))
            {
                continue;
            }
            let (owner, name) = signal.path.rsplit_once('.').unwrap_or(("", &signal.path));
            let id = SignalId(id as u32);
            let Some(layout) = signal_layout(design, &[id]) else {
                continue;
            };
            self.logic_global(
                module,
                design,
                layout,
                self.namespace(owner),
                name,
                signal.declaration_span,
                LogicSource::Signal(id),
                design
                    .meta_of
                    .get(&id.0)
                    .map(|id| LogicSource::Signal(SignalId(*id))),
            );
            let normalized = needs_mirror(layout);
            let Some(ty) = self.ty(design, layout, normalized) else {
                continue;
            };
            let offset = self.signal_bits(design, id);
            let (global, expression) = if normalized {
                (
                    self.mirror_at(
                        module,
                        "cur",
                        &format!("sx.signal.{}", id.0),
                        layout,
                        offset / 8,
                    ),
                    None,
                )
            } else {
                (
                    "cur".into(),
                    Some(self.expression(layout, offset, cfg!(feature = "bitpack"))),
                )
            };
            let info = self.builder.create_global_variable_expression(
                self.namespace(owner),
                name,
                "",
                self.file(signal.declaration_span.file),
                self.sources
                    .line_col(signal.declaration_span.file, signal.declaration_span.start)
                    .0,
                ty,
                true,
                expression,
                None,
                0,
            );
            self.attach_global(module, &global, info);
        }
        for storage in &design.process_ir.storages {
            let global = storage_state_name(storage.id);
            if module.get_global(&global).is_none() {
                continue;
            }
            let owner = design
                .process_ir
                .processes
                .iter()
                .find(|p| p.owner == storage.owner)
                .and_then(|p| p.label.as_deref())
                .and_then(|name| name.rsplit_once("::"))
                .map_or("", |(owner, _)| owner);
            if runtime_handle_layout(storage.layout.as_ref()).is_some()
                || runtime_handle_width(storage.ty.as_ref()).is_some()
            {
                self.string_global(
                    module,
                    &global,
                    self.namespace(owner),
                    &storage.name,
                    storage.span,
                    design
                        .process_ir
                        .processes
                        .iter()
                        .find(|p| p.owner == storage.owner)
                        .map_or(storage.owner.0, |p| p.root.0),
                );
                continue;
            }
            let Some(layout) = storage.layout.as_ref() else {
                continue;
            };
            self.logic_global(
                module,
                design,
                layout,
                self.namespace(owner),
                &storage.name,
                storage.span,
                LogicSource::State(global.clone()),
                module
                    .get_global(&storage_meta_name(storage.id))
                    .map(|_| LogicSource::State(storage_meta_name(storage.id))),
            );
            let normalized = needs_mirror(layout);
            let Some(ty) = self.ty(design, layout, normalized) else {
                continue;
            };
            let global = if normalized {
                self.mirror(module, &global, layout)
            } else {
                global
            };
            let info = self.builder.create_global_variable_expression(
                self.namespace(owner),
                &storage.name,
                "",
                self.file(storage.span.file),
                self.sources
                    .line_col(storage.span.file, storage.span.start)
                    .0,
                ty,
                true,
                (!normalized).then(|| self.expression(layout, 0, false)),
                None,
                0,
            );
            self.attach_global(module, &global, info);
        }
    }

    fn mirror(&self, module: &Module<'ctx>, name: &str, layout: &SourceLayout) -> String {
        self.mirror_at(module, name, name, layout, 0)
    }

    fn mirror_at(
        &self,
        module: &Module<'ctx>,
        source: &str,
        name: &str,
        layout: &SourceLayout,
        source_byte: u64,
    ) -> String {
        let mirror_name = format!("{name}.debug");
        let bytes = u32::try_from(inspection_bits(layout).expect("concrete inspection layout") / 8)
            .expect("validated packed width");
        let buffer = self.inspection_buffer(module, &mirror_name, bytes);
        self.mirrors.borrow_mut().push(Mirror {
            source: module.get_global(source).expect("declared state"),
            source_byte,
            buffer,
            layout: layout.clone(),
        });
        mirror_name
    }

    fn inspection_buffer(
        &self,
        module: &Module<'ctx>,
        name: &str,
        bytes: u32,
    ) -> GlobalValue<'ctx> {
        let ty = self.context.i8_type().array_type(bytes);
        let buffer = module.add_global(ty, None, name);
        buffer.set_initializer(&ty.const_zero());
        buffer.set_linkage(Linkage::Internal);
        if self.refresh.get().is_none() {
            self.refresh.set(Some(module.add_function(
                "sx.debug.refresh",
                self.context.void_type().fn_type(&[], false),
                Some(Linkage::Internal),
            )));
        }
        buffer
    }

    pub fn sync(&self, builder: &Builder<'ctx>) {
        if let Some(refresh) = self.refresh.get() {
            builder.unset_current_debug_location();
            builder.build_call(refresh, &[], "").unwrap();
        }
    }

    fn copy_fields(
        &self,
        builder: &Builder<'ctx>,
        buffer: GlobalValue<'ctx>,
        value: IntValue<'ctx>,
        layout: &SourceLayout,
        logical: &mut u32,
        byte: &mut u32,
    ) {
        match &layout.kind {
            LayoutKind::Struct { fields, .. } => {
                for field in fields {
                    self.copy_fields(builder, buffer, value, &field.layout, logical, byte);
                }
            }
            LayoutKind::Array {
                range: Some(range),
                element,
            } => {
                for _ in 0..range.len().unwrap() {
                    self.copy_fields(builder, buffer, value, element, logical, byte);
                }
            }
            _ => {
                let width = layout.packed_width().unwrap();
                let part = extract_region(builder, value, *logical, width).unwrap();
                let ty = self
                    .context
                    .custom_width_int_type(
                        std::num::NonZeroU32::new(width.div_ceil(8) * 8).unwrap(),
                    )
                    .unwrap();
                let part = if ty == part.get_type() {
                    part
                } else if process_layout_is_signed(layout) {
                    builder
                        .build_int_s_extend(part, ty, "debug.signed")
                        .unwrap()
                } else {
                    builder
                        .build_int_z_extend(part, ty, "debug.unsigned")
                        .unwrap()
                };
                let pointer = unsafe {
                    builder
                        .build_in_bounds_gep(
                            self.context.i8_type(),
                            buffer.as_pointer_value(),
                            &[self.context.i64_type().const_int(u64::from(*byte), false)],
                            "debug.field",
                        )
                        .unwrap()
                };
                builder
                    .build_store(pointer, part)
                    .unwrap()
                    .set_alignment(1)
                    .unwrap();
                *logical += width;
                *byte += width.div_ceil(8);
            }
        }
    }

    fn expression(
        &self,
        layout: &SourceLayout,
        offset: u64,
        packed_word: bool,
    ) -> inkwell::debug_info::DIExpression<'ctx> {
        self.builder
            .create_expression(self.expression_ops(layout, offset, packed_word))
    }

    fn expression_ops(&self, layout: &SourceLayout, offset: u64, packed_word: bool) -> Vec<i64> {
        let width = layout.packed_width().expect("concrete debug layout");
        let mut ops = vec![DW_OP_PLUS_UCONST, (offset / 8) as i64];
        // Native integer slots round up to bytes; bitpacked signals may start
        // inside a byte. Compute a value for these scalars so neither padding
        // nor a neighbouring signal changes what the debugger reports.
        if width <= 64 && (!offset.is_multiple_of(8) || !width.is_multiple_of(8)) {
            let (bytes, shift) = if packed_word {
                ops[1] = ((offset / 64) * 8) as i64;
                (8, offset % 64)
            } else {
                (u64::from(width.div_ceil(8)), offset % 8)
            };
            ops.extend([DW_OP_DEREF_SIZE, bytes as i64]);
            if shift != 0 {
                ops.extend([DW_OP_CONSTU, shift as i64, DW_OP_SHR]);
            }
            if process_layout_is_signed(layout) {
                let shift = i64::from(64 - width);
                ops.extend([
                    DW_OP_CONSTU,
                    shift,
                    DW_OP_SHL,
                    DW_OP_CONSTU,
                    shift,
                    DW_OP_SHRA,
                ]);
            } else if width != 64 {
                ops.extend([DW_OP_CONSTU, ((1u64 << width) - 1) as i64, DW_OP_AND]);
            }
            ops.push(DW_OP_STACK_VALUE);
        }
        ops
    }

    fn attach_global(
        &self,
        module: &Module<'ctx>,
        name: &str,
        info: DIGlobalVariableExpression<'ctx>,
    ) {
        // Multiple source signals share one state global. LLVM expects each
        // expression attached directly, not wrapped in an MDTuple.
        unsafe {
            inkwell::llvm_sys::core::LLVMGlobalAddMetadata(
                module
                    .get_global(name)
                    .expect("declared state")
                    .as_value_ref(),
                self.context.get_kind_id("dbg"),
                inkwell::llvm_sys::core::LLVMValueAsMetadata(
                    info.as_metadata_value(self.context).as_value_ref(),
                ),
            );
        }
    }

    pub fn process(
        &self,
        module: &Module<'ctx>,
        design: &Design,
        process: &ProcessCfg,
        function: FunctionValue<'ctx>,
        builder: &Builder<'ctx>,
    ) -> DISubprogram<'ctx> {
        let file = self.file(process.span.file);
        let line = self
            .sources
            .line_col(process.span.file, process.span.start)
            .0;
        let label = process
            .label
            .clone()
            .unwrap_or_else(|| format!("process {}", process.id.0));
        let (owner, name) = label.rsplit_once("::").unwrap_or(("", &label));
        let scope = self.builder.create_function(
            self.namespace(owner),
            name,
            // No native linkage spelling: Inkwell's None defaults to the
            // unqualified name, which hides the elaborated namespace in GDB.
            Some(""),
            file,
            line,
            self.builder.create_subroutine_type(
                file,
                Some(self.basic("process status", 8, DW_ATE_UNSIGNED)),
                &[self.basic("resume block", 32, DW_ATE_UNSIGNED)],
                DIFlags::ZERO,
            ),
            true,
            true,
            line,
            DIFlags::ZERO,
            false,
        );
        function.set_subprogram(scope);
        let key = scope.as_mut_ptr() as usize;
        let frames = process
            .inline_calls
            .iter()
            .map(|call| {
                let (owner, name) = call.name.rsplit_once("::").unwrap_or(("", &call.name));
                let file = self.file(call.function.file);
                let line = self
                    .sources
                    .line_col(call.function.file, call.function.start)
                    .0;
                let parameter_types = call
                    .parameters
                    .iter()
                    .filter_map(|parameter| self.value_debug_type(design, parameter.value))
                    .collect::<Vec<_>>();
                (
                    self.builder.create_function(
                        self.namespace(&owner.replace("::", ".")),
                        name,
                        Some(""),
                        file,
                        line,
                        self.builder.create_subroutine_type(
                            file,
                            call.result
                                .and_then(|result| self.value_debug_type(design, result)),
                            &parameter_types,
                            DIFlags::ZERO,
                        ),
                        true,
                        true,
                        line,
                        DIFlags::ZERO,
                        false,
                    ),
                    None,
                )
            })
            .collect::<Vec<_>>();
        self.inline_scopes.borrow_mut().insert(key, frames.clone());
        self.parameter_parents.borrow_mut().insert(
            key,
            process
                .inline_calls
                .iter()
                .map(|call| call.parent)
                .collect(),
        );
        let extents = process
            .locals
            .iter()
            .filter_map(|local| {
                local
                    .scope
                    .map(|span| (process.local_inline_call(local.id), span))
            })
            .collect::<Vec<_>>();
        let scopes = self.declare_lexical_scopes(scope, extents, &frames);
        for (id, call) in process.inline_calls.iter().enumerate() {
            let location = self.location(scope, call.call, call.parent);
            self.inline_scopes.borrow_mut().get_mut(&key).unwrap()[id].1 = Some(location);
        }
        for local in &process.locals {
            if local.source.is_none() {
                continue;
            }
            let global_name = local_state_name(process.id, local.id);
            if module.get_global(&global_name).is_none() {
                continue;
            }
            let inline = process.local_inline_call(local.id);
            let base = inline
                .map_or(scope, |id| frames[id as usize].0)
                .as_debug_info_scope();
            let local_scope = local
                .scope
                .and_then(|span| {
                    scopes
                        .iter()
                        .find(|(id, extent, _)| *id == inline && *extent == span)
                })
                .map_or(base, |(_, _, scope)| *scope);
            if runtime_handle_layout(local.layout.as_ref()).is_some()
                || runtime_handle_width(local.ty.as_ref()).is_some()
            {
                self.string_global(
                    module,
                    &global_name,
                    local_scope,
                    &local.name,
                    local.span,
                    process.root.0,
                );
                continue;
            }
            let fallback;
            let layout = if let Some(layout) = local
                .layout
                .as_ref()
                .or_else(|| local.ty.as_ref().and_then(|ty| layout_for_type(design, ty)))
            {
                layout
            } else {
                let Some(width) = local_width(design, process.id, local.id) else {
                    continue;
                };
                let domain = match local.ty.as_ref() {
                    Some(siox::types::Ty::Integer) => ScalarDomain::Integer,
                    Some(siox::types::Ty::Real) => ScalarDomain::Real,
                    Some(siox::types::Ty::Char) => ScalarDomain::Character,
                    _ => ScalarDomain::Bits,
                };
                fallback = SourceLayout {
                    span: local.span,
                    kind: LayoutKind::Scalar {
                        width,
                        domain,
                        nominal: None,
                        value_range: None,
                    },
                };
                &fallback
            };
            self.logic_global(
                module,
                design,
                layout,
                local_scope,
                &local.name,
                local.span,
                LogicSource::State(global_name.clone()),
                module
                    .get_global(&local_meta_name(process.id, local.id))
                    .map(|_| LogicSource::State(local_meta_name(process.id, local.id))),
            );
            let normalized = needs_mirror(layout);
            let Some(ty) = self.ty(design, layout, normalized) else {
                continue;
            };
            let global_name = if normalized {
                self.mirror(module, &global_name, layout)
            } else {
                global_name
            };
            // Locals live in persistent global slots, not on a native stack.
            // Describe them as lexically scoped static variables: dbg.declare
            // of a global address loses its location in LLVM's O0 selection.
            let variable = self.builder.create_global_variable_expression(
                local_scope,
                &local.name,
                "",
                self.file(local.span.file),
                self.sources.line_col(local.span.file, local.span.start).0,
                ty,
                true,
                (!normalized).then(|| self.expression(layout, 0, false)),
                None,
                0,
            );
            self.attach_global(module, &global_name, variable);
        }
        self.parameters(module, design, process, builder, scope);
        scope
    }

    pub fn finish(&self, module: &Module<'ctx>, design: &Design) {
        if let Some(function) = self.refresh.get() {
            let builder = self.context.create_builder();
            builder.position_at_end(self.context.append_basic_block(function, "refresh"));
            for mirror in self.mirrors.borrow().iter() {
                let width = mirror.layout.packed_width().unwrap();
                let ty = self
                    .context
                    .custom_width_int_type(std::num::NonZeroU32::new(width).unwrap())
                    .unwrap();
                let pointer = unsafe {
                    builder
                        .build_in_bounds_gep(
                            self.context.i8_type(),
                            mirror.source.as_pointer_value(),
                            &[self.context.i64_type().const_int(mirror.source_byte, false)],
                            "debug.source",
                        )
                        .unwrap()
                };
                let load = builder.build_load(ty, pointer, "debug.packed").unwrap();
                load.as_instruction_value()
                    .unwrap()
                    .set_alignment(1)
                    .unwrap();
                let value = load.into_int_value();
                self.copy_fields(
                    &builder,
                    mirror.buffer,
                    value,
                    &mirror.layout,
                    &mut 0,
                    &mut 0,
                );
            }
            self.refresh_logic(module, design, &builder);
            self.refresh_strings(module, &builder);
            builder.build_return(None).unwrap();
        }
        self.builder.finalize();
    }
}

fn file_parts(name: &str) -> (String, String) {
    // A relative import directory is relative to the compiler's working
    // directory, not to the entry source's DW_AT_comp_dir.
    let path = std::path::absolute(name).unwrap_or_else(|_| Path::new(name).to_path_buf());
    (
        path.file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned(),
        path.parent()
            .unwrap_or(Path::new("."))
            .to_string_lossy()
            .into_owned(),
    )
}

fn contains(extent: Span, span: Span) -> bool {
    extent.file == span.file && extent.start <= span.start && span.start < extent.end
}

fn inspection_bits(layout: &SourceLayout) -> Option<u64> {
    match &layout.kind {
        LayoutKind::Struct { fields, .. } => fields.iter().try_fold(0u64, |total, field| {
            total.checked_add(inspection_bits(&field.layout)?)
        }),
        LayoutKind::Array { range, element } => range
            .as_ref()?
            .len()?
            .checked_mul(inspection_bits(element)?),
        _ => Some(u64::from(layout.packed_width()?.div_ceil(8)) * 8),
    }
}

fn needs_mirror(layout: &SourceLayout) -> bool {
    (matches!(
        layout.kind,
        LayoutKind::Array { .. } | LayoutKind::Struct { .. }
    ) && inspection_bits(layout) != layout.bit_width())
        // DWARF addresses expose whole bytes, but LLVM iN stores do not
        // specify the unused bits of the final byte. Normalize both signs.
        || layout
            .packed_width()
            .is_some_and(|width| width > 64 && !width.is_multiple_of(8))
}
