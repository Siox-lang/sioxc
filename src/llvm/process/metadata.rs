//! Waveform and source-location metadata, and `emit_metadata`, the entry
//! point that writes every table the runtime discovers.

use super::*;

#[derive(Default)]
pub(super) struct WaveScope {
    pub(super) children: BTreeMap<String, WaveScope>,
    pub(super) signals: Vec<(usize, String)>,
}

/// Translate a std-defined logic encoding to the four VCD states. Weak values
/// retain their definite value bit, while every unknown and high-impedance
/// discriminant becomes `x` or `z` respectively.
pub(super) fn wave_logic_symbols_for_type(
    design: &Design,
    name: &str,
) -> Option<Vec<(u64, String)>> {
    let symbols = design.enum_syms.get(name)?;
    let encoding = design.logic_encodings.get(name)?;
    let mut values = symbols
        .keys()
        .copied()
        .map(|discriminant| {
            let symbol = if encoding.high_impedance.contains(&discriminant) {
                "z"
            } else if encoding.unknown.contains(&discriminant) {
                "x"
            } else if encoding.value_bits.get(&discriminant).copied()? {
                "1"
            } else {
                "0"
            };
            Some((discriminant, symbol.to_string()))
        })
        .collect::<Option<Vec<_>>>()?;
    values.sort_by_key(|(discriminant, _)| *discriminant);
    Some(values)
}

pub(super) fn wave_logic_symbols(
    design: &Design,
    signal: &siox::ir::Signal,
) -> Option<Vec<(u64, String)>> {
    wave_logic_symbols_for_type(design, signal.enum_type.as_deref()?)
}

pub(super) fn emit_wave_scope_header(
    out: &mut String,
    name: &str,
    scope: &WaveScope,
    design: &Design,
) {
    out.push_str(&format!("$scope module {name} $end\n"));
    for &(id, ref signal_name) in &scope.signals {
        let signal = &design.signals[id];
        let logic = wave_logic_symbols(design, signal).is_some();
        let kind = if signal.real {
            "real"
        } else if signal
            .enum_type
            .as_ref()
            .is_some_and(|name| !logic && design.enum_syms.contains_key(name))
        {
            "string"
        } else {
            "wire"
        };
        let width = if kind == "string" || logic {
            1
        } else {
            signal.width.max(1)
        };
        out.push_str(&format!("$var {kind} {width} v{id} {signal_name} $end\n"));
    }
    for (child_name, child) in &scope.children {
        emit_wave_scope_header(out, child_name, child, design);
    }
    out.push_str("$upscope $end\n");
}

pub(super) fn collect_wave_scopes(
    scope: &WaveScope,
    parent: Option<u32>,
    names: &mut Vec<String>,
    parents: &mut Vec<u32>,
    signal_scopes: &mut HashMap<usize, (u32, String)>,
) {
    for (name, child) in &scope.children {
        let id = u32::try_from(names.len()).expect("waveform scope count exceeds its ABI index");
        names.push(name.clone());
        parents.push(parent.unwrap_or(u32::MAX));
        for &(signal, ref leaf) in &child.signals {
            signal_scopes.insert(signal, (id, leaf.clone()));
        }
        collect_wave_scopes(child, Some(id), names, parents, signal_scopes);
    }
}

/// Emit the design-independent waveform ABI as immutable object data.
///
/// Kinds are `0 = bits`, `1 = real`, `2 = symbolic enum`, `3 = scalar Logic`,
/// and `4 = packed Logic with a discriminant companion plane`. Symbol ranges
/// map either enum discriminants to names or Logic discriminants to one VCD
/// character. Companion and temporary metavalue planes are deliberately not
/// visible as independent waveform signals.
pub(super) fn emit_wave_metadata<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    design: &Design,
) {
    let hidden = design
        .meta_of
        .values()
        .chain(design.metavalue_temps.iter())
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let visible = design
        .signals
        .iter()
        .enumerate()
        .filter(|(id, _)| !hidden.contains(&(*id as u32)))
        .collect::<Vec<_>>();

    let mut root = WaveScope::default();
    for &(id, signal) in &visible {
        let mut parts = signal.path.split('.').collect::<Vec<_>>();
        let signal_name = parts.pop().unwrap_or("signal").to_string();
        if parts.is_empty() {
            parts.push("top");
        }
        let mut scope = &mut root;
        for part in parts {
            scope = scope.children.entry(part.to_string()).or_default();
        }
        scope.signals.push((id, signal_name));
    }
    let mut header =
        String::from("$version siox native test executable $end\n$timescale 1fs $end\n");
    for (name, scope) in &root.children {
        emit_wave_scope_header(&mut header, name, scope, design);
    }
    header.push_str("$enddefinitions $end\n");

    let mut scope_names = Vec::new();
    let mut scope_parents = Vec::new();
    let mut signal_scope_map = HashMap::new();
    collect_wave_scopes(
        &root,
        None,
        &mut scope_names,
        &mut scope_parents,
        &mut signal_scope_map,
    );

    let mut ids = Vec::with_capacity(visible.len());
    let mut widths = Vec::with_capacity(visible.len());
    let mut kinds = Vec::with_capacity(visible.len());
    let mut companions = Vec::with_capacity(visible.len());
    let mut signal_scopes = Vec::with_capacity(visible.len());
    let mut signal_names = Vec::with_capacity(visible.len());
    let mut symbol_offsets = Vec::with_capacity(visible.len() + 1);
    let mut symbol_values = Vec::new();
    let mut symbol_texts = Vec::new();
    for &(id, signal) in &visible {
        ids.push(id as u32);
        widths.push(signal.width.max(1));
        let (scope, name) = signal_scope_map
            .get(&id)
            .expect("visible waveform signal belongs to a scope");
        signal_scopes.push(*scope);
        signal_names.push(name.clone());
        let companion = design.meta_of.get(&(id as u32)).copied();
        companions.push(companion.unwrap_or(u32::MAX));
        symbol_offsets.push(symbol_values.len() as u32);

        let (kind, symbols) = if signal.real {
            (1, Vec::new())
        } else if companion.is_some() {
            let symbols = design
                .array_element_enums
                .get(&(id as u32))
                .and_then(|name| wave_logic_symbols_for_type(design, name))
                .unwrap_or_default();
            (4, symbols)
        } else if let Some(symbols) = wave_logic_symbols(design, signal) {
            (3, symbols)
        } else if let Some(symbols) = signal
            .enum_type
            .as_ref()
            .and_then(|name| design.enum_syms.get(name))
        {
            let mut symbols = symbols
                .iter()
                .map(|(&discriminant, symbol)| (discriminant, symbol.clone()))
                .collect::<Vec<_>>();
            symbols.sort_by_key(|(discriminant, _)| *discriminant);
            (2, symbols)
        } else {
            (0, Vec::new())
        };
        kinds.push(kind);
        for (value, text) in symbols {
            symbol_values.push(value);
            symbol_texts.push(text);
        }
    }
    symbol_offsets.push(symbol_values.len() as u32);

    u32_global(
        context,
        module,
        "sx_wave_signal_count",
        visible.len() as u32,
    );
    u32_table(context, module, "sx_wave_signal_ids", &ids);
    u32_table(context, module, "sx_wave_signal_widths", &widths);
    u8_table(context, module, "sx_wave_signal_kinds", &kinds);
    u32_table(context, module, "sx_wave_signal_companions", &companions);
    u32_global(
        context,
        module,
        "sx_wave_scope_count",
        u32::try_from(scope_names.len()).expect("waveform scope count exceeds its ABI index"),
    );
    u32_table(context, module, "sx_wave_scope_parents", &scope_parents);
    string_table(
        context,
        module,
        "sx_wave_scope_names",
        "sx.wave.scope",
        &scope_names,
    );
    u32_table(context, module, "sx_wave_signal_scopes", &signal_scopes);
    string_table(
        context,
        module,
        "sx_wave_signal_names",
        "sx.wave.signal",
        &signal_names,
    );
    u32_table(context, module, "sx_wave_symbol_offsets", &symbol_offsets);
    u64_table(context, module, "sx_wave_symbol_values", &symbol_values);
    string_table(
        context,
        module,
        "sx_wave_symbol_texts",
        "sx.wave.symbol",
        &symbol_texts,
    );
    public_string(context, module, "sx_wave_vcd_header", &header);
}

/// Embed only source locations referenced by Process runtime operations.
///
/// The fixed runtime performs a `(file, offset)` lookup and never reads source
/// files from disk. Keeping the rendered location in object data makes runtime
/// diagnostics use the same [`SourceMap`](crate::diag::SourceMap) rendering as compiler diagnostics.
pub(super) fn emit_source_locations<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    design: &Design,
    sources: Option<&siox::diag::SourceMap>,
) {
    let mut locations = BTreeMap::new();
    if let Some(sources) = sources {
        for storage in &design.process_ir.storages {
            if storage
                .initializer
                .is_some_and(|initializer| initializer_can_raise_host_error(design, initializer))
            {
                if let Some(location) = sources.location(storage.span) {
                    locations.insert((storage.span.file.0, storage.span.start), location);
                }
            }
        }
        for value in &design.process_ir.values {
            if matches!(
                value.kind,
                ProcessValueKind::HostCall {
                    operation: ProcessHostValueOp::StringIndex,
                    ..
                }
            ) {
                if let Some(location) = sources.location(value.span) {
                    locations.insert((value.span.file.0, value.span.start), location);
                }
            }
        }
        for process in &design.process_ir.processes {
            for block in &process.blocks {
                for instruction in &block.instructions {
                    if let ProcessInstruction::Runtime { span, .. } = instruction {
                        if let Some(location) = sources.location(*span) {
                            locations.insert((span.file.0, span.start), location);
                        }
                    }
                }
            }
        }
    }
    let files = locations.keys().map(|(file, _)| *file).collect::<Vec<_>>();
    let offsets = locations
        .keys()
        .map(|(_, offset)| *offset)
        .collect::<Vec<_>>();
    let texts = locations.into_values().collect::<Vec<_>>();
    u32_global(
        context,
        module,
        "sx_source_location_count",
        u32::try_from(texts.len()).expect("runtime source location count exceeds its ABI index"),
    );
    u32_table(context, module, "sx_source_location_files", &files);
    u32_table(context, module, "sx_source_location_offsets", &offsets);
    string_table(
        context,
        module,
        "sx_source_location_texts",
        "sx.source.location",
        &texts,
    );

    let index_sites = design.index_sites();
    let index_left = index_sites
        .iter()
        .map(|site| site.left as u64)
        .collect::<Vec<_>>();
    let index_right = index_sites
        .iter()
        .map(|site| site.right as u64)
        .collect::<Vec<_>>();
    let index_locations = index_sites
        .iter()
        .map(|site| {
            sources
                .and_then(|sources| sources.location(site.span))
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    u32_global(
        context,
        module,
        "sx_index_site_count",
        u32::try_from(index_sites.len()).expect("runtime index site count exceeds its ABI index"),
    );
    u64_table(context, module, "sx_index_site_left", &index_left);
    u64_table(context, module, "sx_index_site_right", &index_right);
    string_table(
        context,
        module,
        "sx_index_site_locations",
        "sx.index.location",
        &index_locations,
    );

    let range_sites = design.range_sites();
    let range_locations = range_sites
        .iter()
        .map(|span| {
            sources
                .and_then(|sources| sources.location(*span))
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    u32_global(
        context,
        module,
        "sx_range_site_count",
        u32::try_from(range_sites.len()).expect("runtime range site count exceeds its ABI index"),
    );
    string_table(
        context,
        module,
        "sx_range_site_locations",
        "sx.range.location",
        &range_locations,
    );

    let signal_names = design
        .signals
        .iter()
        .map(|signal| signal.path.clone())
        .collect::<Vec<_>>();
    let signal_left = design
        .signals
        .iter()
        .map(|signal| signal.range.unwrap_or_default().0 as u64)
        .collect::<Vec<_>>();
    let signal_right = design
        .signals
        .iter()
        .map(|signal| signal.range.unwrap_or_default().1 as u64)
        .collect::<Vec<_>>();
    let signal_locations = design
        .signals
        .iter()
        .map(|signal| {
            sources
                .and_then(|sources| sources.location(signal.declaration_span))
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    u32_global(
        context,
        module,
        "sx_range_signal_count",
        u32::try_from(design.signals.len())
            .expect("runtime range signal count exceeds its ABI index"),
    );
    string_table(
        context,
        module,
        "sx_range_signal_names",
        "sx.range.signal.name",
        &signal_names,
    );
    u64_table(context, module, "sx_range_signal_left", &signal_left);
    u64_table(context, module, "sx_range_signal_right", &signal_right);
    string_table(
        context,
        module,
        "sx_range_signal_locations",
        "sx.range.signal.location",
        &signal_locations,
    );
}

/// Materialize the runtime-facing test/process descriptor tables.
///
/// All lists use offset + flattened-value tables, avoiding generated symbols
/// whose shape changes per test. Process sensitivity kinds are encoded as
/// `0 = signal` and `1 = persistent storage`; process activation is `0 = once
/// at time zero`, `1 = reactive`. The counts make the sentinel elements of
/// logically empty arrays unobservable.
pub(in crate::llvm) fn emit_metadata<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    design: &Design,
    sources: Option<&siox::diag::SourceMap>,
) {
    let process_ir = &design.process_ir;
    emit_state_helpers(context, module, design);
    emit_wave_metadata(context, module, design);
    emit_source_locations(context, module, design, sources);
    u32_global(
        context,
        module,
        "sx_process_abi_version",
        PROCESS_ABI_VERSION,
    );
    u32_global(
        context,
        module,
        "sx_test_count",
        process_ir.tests.len() as u32,
    );
    u32_global(
        context,
        module,
        "sx_process_count",
        process_ir.processes.len() as u32,
    );
    test_name_table(context, module, design);
    process_entry_table(context, module, design);

    let test_roots = process_ir
        .tests
        .iter()
        .map(|test| test.root.0)
        .collect::<Vec<_>>();
    let mut test_process_offsets = Vec::with_capacity(process_ir.tests.len() + 1);
    let mut test_process_ids = Vec::new();
    for test in &process_ir.tests {
        test_process_offsets.push(test_process_ids.len() as u32);
        test_process_ids.extend(test.processes.iter().map(|process| process.0));
    }
    test_process_offsets.push(test_process_ids.len() as u32);
    u32_table(context, module, "sx_test_roots", &test_roots);
    u32_table(
        context,
        module,
        "sx_test_process_offsets",
        &test_process_offsets,
    );
    u32_table(context, module, "sx_test_process_ids", &test_process_ids);

    let roots = process_ir
        .processes
        .iter()
        .map(|process| process.root.0)
        .collect::<Vec<_>>();
    let owners = process_ir
        .processes
        .iter()
        .map(|process| process.owner.0)
        .collect::<Vec<_>>();
    let initial_blocks = process_ir
        .processes
        .iter()
        .map(|process| process.entry.0)
        .collect::<Vec<_>>();
    let activations = process_ir
        .processes
        .iter()
        .map(|process| match process.activation {
            ProcessActivation::TimeZero => 0,
            ProcessActivation::Reactive { .. } => 1,
        })
        .collect::<Vec<_>>();
    let mut sensitivity_offsets = Vec::with_capacity(process_ir.processes.len() + 1);
    let mut sensitivity_kinds = Vec::new();
    let mut sensitivity_ids = Vec::new();
    for process in &process_ir.processes {
        sensitivity_offsets.push(sensitivity_ids.len() as u32);
        if let ProcessActivation::Reactive { sensitivity } = &process.activation {
            for item in sensitivity {
                match item {
                    ProcessSensitivity::Signal(signal) => {
                        sensitivity_kinds.push(0);
                        sensitivity_ids.push(signal.0);
                    }
                    ProcessSensitivity::Storage(storage) => {
                        sensitivity_kinds.push(1);
                        sensitivity_ids.push(storage.0);
                    }
                }
            }
        }
    }
    sensitivity_offsets.push(sensitivity_ids.len() as u32);

    u32_table(context, module, "sx_process_roots", &roots);
    u32_table(context, module, "sx_process_owners", &owners);
    u32_table(
        context,
        module,
        "sx_process_initial_blocks",
        &initial_blocks,
    );
    u8_table(context, module, "sx_process_activations", &activations);
    u32_table(
        context,
        module,
        "sx_process_sensitivity_offsets",
        &sensitivity_offsets,
    );
    u8_table(
        context,
        module,
        "sx_process_sensitivity_kinds",
        &sensitivity_kinds,
    );
    u32_table(
        context,
        module,
        "sx_process_sensitivity_ids",
        &sensitivity_ids,
    );
}
