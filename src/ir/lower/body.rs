//! Entity implementation-body lowering and instance specialization.

use super::*;

impl<'a> Lowering<'a> {
    /// Lower an entity's impl body: declarations, processes and concurrent
    /// statements.
    pub(super) fn lower_body(
        &mut self,
        entity_id: DefId,
        path: &str,
        env: &HashMap<String, i64>,
        type_env: &HashMap<String, ast::Type>,
        aliases: &HashMap<String, SignalId>,
        is_root: bool,
    ) -> HashMap<String, (SignalId, Option<ast::Direction>)> {
        let Some(edecl) = self.entities.get(&entity_id).copied() else {
            return HashMap::new();
        };

        // Save the caller's scope; give this body a fresh one.
        let saved_instance_path = std::mem::replace(&mut self.cur_instance_path, path.to_string());
        let saved_locals = std::mem::take(&mut self.locals);
        let saved_enum = std::mem::take(&mut self.local_enum);
        let saved_struct = std::mem::take(&mut self.local_struct);
        let saved_struct_repr = std::mem::take(&mut self.local_struct_repr);
        let saved_char = std::mem::take(&mut self.local_char);
        let saved_array = std::mem::take(&mut self.local_array);
        let saved_numeric = std::mem::take(&mut self.local_numeric);
        let saved_instance_arrays = std::mem::take(&mut self.instance_arrays);
        // A Rust-style binder may rename: `impl<M: integer> Counter<M>` calls
        // the entity's first parameter `M` inside its own body. Every lookup
        // below — signal widths as much as expressions — goes through `env`,
        // which is keyed by the entity's declared names, so extend it with
        // each impl's names bound by position, as Rust binds them.
        let mut renamed = env.clone();
        let mut renamed_types = type_env.clone();
        {
            let bodies: Vec<&ast::ImplDecl> =
                self.impls.get(&entity_id).cloned().unwrap_or_default();
            for im in bodies {
                let ast::Type::Generic { args, .. } = &im.target else {
                    continue;
                };
                for (i, arg) in args.iter().enumerate() {
                    let ast::GenericArg::Positional(ast::Expr::Path(path)) = arg else {
                        continue;
                    };
                    let ([seg], Some(param)) =
                        (path.segments.as_slice(), edecl.params.params.get(i))
                    else {
                        continue;
                    };
                    if seg.text == param.name.text {
                        continue;
                    }
                    if let Some(&value) = env.get(&param.name.text) {
                        renamed.insert(seg.text.clone(), value);
                    }
                    if let Some(ty) = type_env.get(&param.name.text) {
                        renamed_types.insert(seg.text.clone(), ty.clone());
                    }
                }
            }
        }
        // Constants declared *inside* an implementation (spec 3.3) were never
        // collected — only module-level ones were — so `const MAX: unsigned[W]
        // = (1 << W) - 1;` compiled and then every read reported the name as
        // unknown. They fold here rather than globally because the spec's own
        // example depends on the entity's parameters, so one declaration is a
        // different number per instance. They go into the *env* as well as the
        // constant tables: array sizes and slice bounds resolve through the
        // env, so a constant missing from it left `let regs: unsigned[8][K]`
        // with no elements at all.
        let saved_consts = self.consts.clone();
        let saved_const_values = self.const_values.clone();
        {
            let body_consts: Vec<&ast::ConstDecl> = self
                .impls
                .get(&entity_id)
                .map(|impls| {
                    impls
                        .iter()
                        .flat_map(|im| &im.items)
                        .filter_map(|item| match item {
                            ast::ImplItem::Const(c) => Some(c),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            for _ in 0..=body_consts.len() {
                let mut progressed = false;
                for c in &body_consts {
                    let mut scope = self.consts.clone();
                    scope.extend(renamed.iter().map(|(k, v)| (k.clone(), *v)));
                    if self.fold_const(&c.name.text, c, &scope) {
                        progressed = true;
                        // Array sizes and slice bounds resolve through the
                        // env, so an integer constant has to reach it too.
                        if let Some(&value) = self.consts.get(&c.name.text) {
                            renamed.insert(c.name.text.clone(), value);
                        }
                    }
                }
                if !progressed {
                    break;
                }
            }
        }
        let env = &renamed;
        let type_env = &renamed_types;
        let saved_env = std::mem::replace(&mut self.cur_env, env.clone());
        let saved_type_env = std::mem::replace(&mut self.cur_type_env, type_env.clone());
        self.lower_stack.push(entity_id);
        // Ports (struct/array-typed ones flatten to leaves), then the port map.
        // An `inout` port aliased to a parent net reuses that net's signal
        // instead of allocating its own: the body's `pin = expr` then drives the
        // shared net (resolving across instances) and reads of `pin` read the
        // resolved value — Verilog's bidirectional-port model.
        for p in &edecl.ports {
            self.add_typed_signal(path, &p.name.text, &p.ty, env, p.span);
        }
        // An aliased `inout` port repoints its (leaf) name at the shared parent
        // net (keeping the type metadata just registered), so the body drives and
        // reads that net directly. The port's own allocated signal is left
        // unused. A scalar port aliases one name (`s`); a struct/array `inout`
        // port aliases each flattened leaf (`s.valid`, `s.data`).
        for (name, &net) in aliases {
            if self.locals.contains_key(name) {
                self.locals.insert(name.clone(), net);
            }
        }
        // The port map. A scalar port is one entry (`s`); a struct/array port
        // flattens to one entry per leaf (`s.valid`, `s.data`, `bus[0]`), each
        // tagged with the port's direction, so a parent can wire every leaf.
        // (Only port signals exist in `locals` at this point — `let` state
        // signals are added below — so the prefix scan can't catch a non-port.)
        let mut ports: HashMap<String, (SignalId, Option<ast::Direction>)> = HashMap::new();
        let mut new_out_ports: Vec<SignalId> = Vec::new();
        for p in &edecl.ports {
            let dot = format!("{}.", p.name.text);
            let idx = format!("{}[", p.name.text);
            // An applied-view port (`bus: Source Stream`) gives each leaf its
            // direction from the view (`out valid; in ready;`); a plain port
            // applies its single direction to every leaf.
            let view = self.view_of(&p.ty).and_then(|k| self.view_dirs.get(&k));
            for (k, &id) in &self.locals {
                if *k == p.name.text || k.starts_with(&dot) || k.starts_with(&idx) {
                    let dir = match view {
                        Some(m) => k
                            .strip_prefix(&dot)
                            .and_then(|field| m.get(field).copied())
                            .or(p.dir),
                        None => p.dir,
                    };
                    // A plain (non-bus-mode) `out` port must be driven inside the
                    // entity; record it for the undriven check. Bus-mode leaves
                    // and `inout` are excluded (their drive model differs).
                    if !edecl.is_extern && view.is_none() && dir == Some(ast::Direction::Out) {
                        new_out_ports.push(id);
                    }
                    ports.insert(k.clone(), (id, dir));
                }
            }
        }
        self.plain_out_ports.extend(new_out_ports);

        // `let` items: instance bindings are collected for recursion; the rest
        // become state signals.
        let impls: Vec<&ast::ImplDecl> = self.impls.get(&entity_id).cloned().unwrap_or_default();
        let mut subinsts: Vec<(String, ast::Type, Vec<ast::ConnectArg>)> = Vec::new();
        // Generate loops (`for i in 0..n { let s: Sub = { .. } }`) unroll here,
        // substituting the loop index into each instance's type args and
        // connections so the flattened element signals (`wires[i]`) resolve.
        for im in &impls {
            for item in &im.items {
                if let ast::ImplItem::Stmt(s) = item {
                    gather_generate(
                        s,
                        env,
                        &[],
                        &self.entities,
                        self.resolved,
                        &self.free_fns,
                        &mut subinsts,
                    );
                }
            }
        }
        for im in &impls {
            for item in &im.items {
                if let ast::ImplItem::Let(l) = item {
                    // `let s: Sub = { .. }` / `let s: Sub [= { .. }]`: a
                    // sub-instance, not a signal. A `let s: T` whose `T` is
                    // *this* entity's type parameter (bound to a concrete type,
                    // e.g. `unsigned[8]`) is a signal even when some entity is also
                    // named `T` — let it fall through to the signal path, where
                    // `add_typed_signal` substitutes `T` via `cur_type_env`.
                    if let Some((cty, args)) = instance_let_parts(l, &self.entities, self.resolved)
                    {
                        let is_type_param =
                            type_head_name(&cty).is_some_and(|h| self.cur_type_env.contains_key(h));
                        if !is_type_param {
                            subinsts.push((l.name.text.clone(), cty, args));
                            continue;
                        }
                    }
                    // `let s: string = "hello";`: the literal sets the range.
                    let unconstrained = match &l.ty {
                        None => true,
                        Some(t) => matches!(
                            self.resolve_alias(t),
                            ast::Type::Indexed { index: None, .. }
                        ),
                    };
                    if unconstrained {
                        if let Some(ast::Expr::StrLit { text, .. }) = &l.value {
                            self.add_char_array(path, &l.name.text, text.chars().count(), l.span);
                            continue;
                        }
                        // `let s: string = read<string>("f.txt");` — the
                        // compiler reads UTF-8; its code-point length sets the
                        // otherwise unconstrained range.
                        if let Some((requested, fpath)) =
                            l.value.as_ref().and_then(Self::fs_read_call)
                        {
                            if self.type_resolves_to(requested, "string") {
                                match std::fs::read_to_string(self.base_dir.join(fpath)) {
                                    Ok(text) => {
                                        let chars: Vec<char> = text.chars().collect();
                                        self.add_char_array(
                                            path,
                                            &l.name.text,
                                            chars.len(),
                                            l.span,
                                        );
                                        for (i, c) in chars.iter().enumerate() {
                                            if let Some(&id) =
                                                self.locals.get(&format!("{}[{i}]", l.name.text))
                                            {
                                                self.out.signals[id.0 as usize].init =
                                                    vec![*c as u32 as u64];
                                            }
                                        }
                                    }
                                    Err(e) => self.sink.emit(
                                        crate::diag::Diagnostic::error(format!(
                                            "read<string>(\"{fpath}\"): {e}"
                                        ))
                                        .with_code(crate::diag::codes::COMPILE_TIME_IO)
                                        .at(l.span),
                                    ),
                                }
                                continue;
                            }
                        }
                    }
                    if let Some(ty) = &l.ty {
                        self.add_typed_signal(path, &l.name.text, ty, env, l.span);
                    } else {
                        self.add_signal(path, &l.name.text, 0, l.span);
                    }
                    // A string initializer on a flattened array
                    // (`let arr: Color[3] = "rgb"`) seeds each element: a
                    // char-enum variant, or a `Char` code point.
                    if let Some(ast::Expr::StrLit { text, .. }) = &l.value {
                        if let Some(indices) = self.local_array.get(&l.name.text).cloned() {
                            for (c, i) in text.chars().zip(&indices) {
                                if let Some(&id) = self.locals.get(&format!("{}[{i}]", l.name.text))
                                {
                                    let en = self.out.signals[id.0 as usize].enum_type.clone();
                                    let v = en
                                        .and_then(|e| self.char_disc(c, &e))
                                        .unwrap_or(c as u32 as u64);
                                    self.out.signals[id.0 as usize].init = vec![v];
                                }
                            }
                        }
                    }
                    // A struct-literal initializer (`let p: P = { .a = 1 }`)
                    // seeds each field signal. The testbench interpreter has
                    // always honoured this; hardware lowering did not, so an
                    // entity-level struct local silently powered on at 0.
                    if let Some(ast::Expr::Construct { args, spread, .. }) = &l.value {
                        let head = l.ty.as_ref().and_then(type_head_name).map(str::to_string);
                        self.seed_struct_literal(
                            &l.name.text,
                            head.as_deref(),
                            args,
                            spread.as_deref(),
                            l.span,
                        );
                    } else if self.local_struct.contains_key(&l.name.text) {
                        // A struct local initialized by anything else. The
                        // scalar fold below never sees these: it is reached
                        // through `locals[name]`, and a struct has signals only
                        // under `name.field`. So `let p: Pair = make(6)` seeded
                        // nothing and every field powered on at zero, silently.
                        if let Some(value) = &l.value {
                            let head = l.ty.as_ref().and_then(type_head_name).map(str::to_string);
                            match self.struct_literal_from_call(value) {
                                Some((fields, spread)) => self.seed_struct_literal(
                                    &l.name.text,
                                    head.as_deref(),
                                    &fields,
                                    spread.as_ref(),
                                    l.span,
                                ),
                                // A default construction (`Pair::new()`,
                                // `Pair()`) names no declared function, and its
                                // structural zeros are the right answer — the
                                // one shape here that must stay silent.
                                None if Self::is_default_construction(value)
                                    && !self.resolves_to_declared_fn(value) => {}
                                // Everything else has no power-on value this
                                // can fold: a body that is not one returned
                                // literal, or a read of another signal. Say so
                                // rather than powering on at zero — the help
                                // names the spelling that works. This matches
                                // the scalar rule exactly, where even a copy
                                // from a constant-initialized local
                                // (`let b: unsigned[8] = a`) is E-P021: an
                                // initializer folds constants, and reading a
                                // signal is not folding.
                                // A whole struct constant (`let p: Pair = K`)
                                // *is* a constant, and the scalar spelling of
                                // it folds, so this seeds rather than reports.
                                None if self.seed_from_struct_const(&l.name.text, value) => {}
                                // A positional literal (`let p: Pair = { 6, 7 }`)
                                // is the named form without the field names.
                                None if head
                                    .as_deref()
                                    .and_then(|h| self.positional_struct_args(h, value))
                                    .is_some() =>
                                {
                                    let args = head
                                        .as_deref()
                                        .and_then(|h| self.positional_struct_args(h, value))
                                        .unwrap_or_default();
                                    self.seed_struct_literal(
                                        &l.name.text,
                                        head.as_deref(),
                                        &args,
                                        None,
                                        l.span,
                                    );
                                }
                                None => {
                                    self.report_non_constant_init(&l.name.text, l.span);
                                }
                            }
                        }
                    }
                    // An array-literal initializer (`let rom: unsigned[8][4] =
                    // [1, 2, 3, 4]`) seeds each element, as the string and
                    // struct-literal forms above do. Without it a lookup table
                    // written this way powered on at 0 in every element and
                    // read back as zeros with no diagnostic.
                    // This used to walk the elements itself, with `enumerate`
                    // for the index and no case for an element that is a
                    // struct. `seed_elements` is the same walk done once: it
                    // takes the indices from the declared range (so a
                    // non-zero-based array seeds the right elements) and seeds
                    // an aggregate element through the struct path.
                    if let Some(ast::Expr::Array { elems, .. }) = &l.value {
                        let name = l.name.text.clone();
                        self.seed_elements(&name, elems.iter().collect(), l.span);
                    }
                    // A value-less internal `let` in a component entity must be
                    // driven; record its leaves for the undriven check. Root
                    // Root entities are excluded: their wires are stimulus fed
                    // externally, so they are not forgotten internal drives.
                    // An instance array (`let stage: Inc[N]`, Inc an entity) is
                    // built element-wise, not driven — never a signal to check.
                    let is_instance_array = l.ty.as_ref().is_some_and(|ty| {
                        type_def_id(ty, self.resolved)
                            .is_some_and(|id| self.entities.contains_key(&id))
                            && !type_head_name(ty)
                                .is_some_and(|head| self.cur_type_env.contains_key(head))
                    });
                    if is_instance_array {
                        self.instance_arrays.insert(l.name.text.clone());
                    }
                    if l.value.is_none() && !is_instance_array && !is_root {
                        let dot = format!("{}.", l.name.text);
                        let idx = format!("{}[", l.name.text);
                        let leaves: Vec<SignalId> = self
                            .locals
                            .iter()
                            .filter(|(k, _)| {
                                **k == l.name.text || k.starts_with(&dot) || k.starts_with(&idx)
                            })
                            .map(|(_, &id)| id)
                            .collect();
                        self.undriven_lets.extend(leaves);
                    }
                    if !is_instance_array && !is_root {
                        let dot = format!("{}.", l.name.text);
                        let idx = format!("{}[", l.name.text);
                        self.unused_lets.extend(
                            self.locals
                                .iter()
                                .filter(|(k, _)| {
                                    **k == l.name.text || k.starts_with(&dot) || k.starts_with(&idx)
                                })
                                .map(|(_, &id)| id),
                        );
                    }
                    // A typed file constructor is owned by elaboration here:
                    // text decodes UTF-8 into Char leaves, while binary packs
                    // little-endian integers and then stores them through the
                    // requested destination representation.
                    if let Some((requested, fpath)) = l.value.as_ref().and_then(Self::fs_read_call)
                    {
                        if self.type_resolves_to(requested, "string") {
                            if let Some(indices) = self.local_array.get(&l.name.text).cloned() {
                                match std::fs::read_to_string(self.base_dir.join(fpath)) {
                                    Ok(text) => {
                                        let chars = text.chars().collect::<Vec<_>>();
                                        if chars.len() > indices.len() {
                                            self.sink.emit(
                                                crate::diag::Diagnostic::error(format!(
                                                    "read<string>(\"{fpath}\"): {} characters do not fit `{}` ({} elements)",
                                                    chars.len(),
                                                    l.name.text,
                                                    indices.len()
                                                ))
                                                .with_code(crate::diag::codes::COMPILE_TIME_IO)
                                                .at(l.span),
                                            );
                                        }
                                        for (position, index) in indices.iter().enumerate() {
                                            if let Some(&id) = self
                                                .locals
                                                .get(&format!("{}[{index}]", l.name.text))
                                            {
                                                self.out.signals[id.0 as usize].init = vec![chars
                                                    .get(position)
                                                    .copied()
                                                    .map(|character| character as u32 as u64)
                                                    .unwrap_or(0)];
                                            }
                                        }
                                    }
                                    Err(error) => self.sink.emit(
                                        crate::diag::Diagnostic::error(format!(
                                            "read<string>(\"{fpath}\"): {error}"
                                        ))
                                        .with_code(crate::diag::codes::COMPILE_TIME_IO)
                                        .at(l.span),
                                    ),
                                }
                            }
                            continue;
                        }

                        let targets = self
                            .local_array
                            .get(&l.name.text)
                            .map(|indices| {
                                indices
                                    .iter()
                                    .filter_map(|index| {
                                        self.locals
                                            .get(&format!("{}[{index}]", l.name.text))
                                            .copied()
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .or_else(|| self.locals.get(&l.name.text).copied().map(|id| vec![id]))
                            .unwrap_or_default();
                        match std::fs::read(self.base_dir.join(fpath)) {
                            Ok(bytes) if !targets.is_empty() => {
                                let element_width =
                                    self.out.signals[targets[0].0 as usize].width.max(1);
                                let element_bytes = element_width.div_ceil(8) as usize;
                                let capacity = element_bytes.saturating_mul(targets.len());
                                if bytes.len() > capacity {
                                    self.sink.emit(
                                        crate::diag::Diagnostic::error(format!(
                                            "read<{}>(\"{fpath}\"): {} bytes do not fit `{}` ({} elements x {element_bytes} bytes)",
                                            crate::syntax::pretty::type_str(requested),
                                            bytes.len(),
                                            l.name.text,
                                            targets.len()
                                        ))
                                        .with_code(crate::diag::codes::COMPILE_TIME_IO)
                                        .at(l.span),
                                    );
                                }
                                for (position, id) in targets.into_iter().enumerate() {
                                    self.out.signals[id.0 as usize].init = file_integer_words(
                                        &bytes,
                                        position.saturating_mul(element_bytes),
                                        element_bytes,
                                        element_width,
                                    );
                                }
                            }
                            Ok(_) => {}
                            Err(error) => self.sink.emit(
                                crate::diag::Diagnostic::error(format!(
                                    "read<{}>(\"{fpath}\"): {error}",
                                    crate::syntax::pretty::type_str(requested)
                                ))
                                .with_code(crate::diag::codes::COMPILE_TIME_IO)
                                .at(l.span),
                            ),
                        }
                        continue;
                    }
                    // A constant initializer is the signal's reset value.
                    if let (Some(v), Some(&id)) = (&l.value, self.locals.get(&l.name.text)) {
                        let en = self.out.signals[id.0 as usize].enum_type.clone();
                        let is_char = self.out.signals[id.0 as usize].char;
                        if let Some(bits) = self.const_init_value(v, en.as_deref(), is_char) {
                            let w = self.out.signals[id.0 as usize].width;
                            let masked = if w > 0 && w < 64 {
                                bits & ((1u64 << w) - 1)
                            } else {
                                bits
                            };
                            self.out.signals[id.0 as usize].init = vec![masked];
                        } else {
                            self.report_non_constant_init(&l.name.text, l.span);
                        }
                        // A metavalue-carrying string init (`"01X0"`) needs a
                        // companion signal to record which elements are `'X'`/… —
                        // the storage half of X/Z vector propagation (stage 1c).
                        if let Some((base, digits)) = Self::bit_string_parts(v) {
                            let (value_words, discs) = self.decode_bit_string_words(base, digits);
                            if value_words.len() > 1 {
                                self.out.signals[id.0 as usize].init = value_words;
                            }
                            if self.has_metavalue(&discs) {
                                self.ensure_meta_companion(id, discs);
                            }
                        }
                    }
                }
            }
        }

        // Sub-instances: lower each under `path.inst`, then wire its ports. An
        // `in` port is driven from the parent's signal; an `out` port drives the
        // parent's. The recursion saves/restores this body's scope, so the
        // parent's names resolve again here.
        for (inst, cty, conns) in &subinsts {
            let Some(sub_id) = type_def_id(cty, self.resolved) else {
                continue;
            };
            // Cyclic instantiation (already diagnosed by the elaborator): don't
            // recurse back into an entity that is still being lowered.
            if self.lower_stack.contains(&sub_id) {
                continue;
            }
            let sub_path = format!("{path}.{inst}");
            let mut sub_env = self.consts.clone();
            sub_env.extend(self.construct_params(cty, sub_id, env));
            let sub_type_env = self.construct_type_params(cty, sub_id);

            // Resolve `inout` connections to the parent net they share *before*
            // lowering the child, so its port aliases to that net. A scalar
            // inout whose parent side isn't a plain signal is left un-aliased
            // (falls back to the in/out wiring below).
            // Normalized `(port, value)` connections (positional bound to port
            // order), used both for inout aliasing and the wiring below.
            let norm = self.norm_conns(conns, sub_id);
            let mut aliases: HashMap<String, SignalId> = HashMap::new();
            if let Some(decl) = self.entities.get(&sub_id).copied() {
                for p in &decl.ports {
                    if p.dir != Some(ast::Direction::Inout) {
                        continue;
                    }
                    let value = norm
                        .iter()
                        .find(|(port, _)| *port == p.name.text)
                        .map(|(_, v)| v.clone());
                    let Some(value) = value else { continue };
                    // Scalar inout: the whole port shares the parent net.
                    if let Some(net) = self.target_signal(&value) {
                        aliases.insert(p.name.text.clone(), net);
                    }
                    // Struct/array inout: alias each leaf of the connected net
                    // (`link.valid`, `bus[0]`) onto the matching port leaf
                    // (`s.valid`, `pin[0]`), so every leaf resolves across the
                    // instances through the shared net.
                    if let Some(net_path) = expr_path(&value) {
                        let dot = format!("{net_path}.");
                        let idx = format!("{net_path}[");
                        for (k, &id) in &self.locals {
                            if let Some(rest) = k.strip_prefix(&dot) {
                                aliases.insert(format!("{}.{}", p.name.text, rest), id);
                            } else if let Some(rest) = k.strip_prefix(&idx) {
                                aliases.insert(format!("{}[{}", p.name.text, rest), id);
                            }
                        }
                    }
                }
            }

            let sub_ports =
                self.lower_body(sub_id, &sub_path, &sub_env, &sub_type_env, &aliases, false);
            // Expose the sub-instance's ports in this scope so `inst.port`
            // (and `stage[i].port`) reads resolve to the child's signal —
            // an output need not be wired to a local to be read.
            for (port, &(sig, _)) in &sub_ports {
                self.locals.entry(format!("{inst}.{port}")).or_insert(sig);
            }
            for (field, value) in &norm {
                let field = field.as_str();
                // The child port's leaves: the port itself (`s`) plus any
                // flattened struct/array members (`s.valid`, `bus[0]`).
                let dot = format!("{field}.");
                let idx = format!("{field}[");
                let mut leaves: Vec<(String, SignalId, Option<ast::Direction>)> = sub_ports
                    .iter()
                    .filter(|(k, _)| **k == *field || k.starts_with(&dot) || k.starts_with(&idx))
                    .map(|(k, &(id, d))| (k.clone(), id, d))
                    .collect();
                if leaves.is_empty() {
                    continue;
                }

                // A scalar port (one leaf named exactly `field`): the connection
                // value may be any expression (`.en = ea`, `.val = 5`).
                if leaves.len() == 1 && leaves[0].0 == *field {
                    let (_, child_id, dir) = leaves[0];
                    // An aliased inout is already wired to the shared net.
                    if dir == Some(ast::Direction::Inout) && aliases.contains_key(field) {
                        continue;
                    }
                    if dir == Some(ast::Direction::Out) {
                        if let Some(target) = self.target_signal(value) {
                            let ctx = self.next_ctx_at(ast::expr_span(value));
                            self.out.drivers.push(Driver {
                                span: self.cur_span,
                                target,
                                cond: None,
                                expr: Expr::Current(child_id),
                                meta: None,
                                ctx,
                            });
                        }
                    } else {
                        let expr = self.lower_expr(value);
                        let ctx = self.next_ctx_at(ast::expr_span(value));
                        self.out.drivers.push(Driver {
                            span: self.cur_span,
                            target: child_id,
                            cond: None,
                            expr,
                            meta: None,
                            ctx,
                        });
                    }
                    continue;
                }

                // A composite (struct/array) port connected to a *literal*
                // has no parent signal to wire leaf-by-leaf, so drive each
                // leaf from the matching field. Without this the whole
                // connection was dropped in silence and the port kept its
                // default — a scalar port has always accepted a value here.
                if expr_path(value).is_none() {
                    if let ast::Expr::Construct { args, .. } = value {
                        let mut fields: HashMap<String, &ast::Expr> = HashMap::new();
                        literal_leaves(args, "", &mut fields);
                        for (k, child_id, dir) in &leaves {
                            if *dir == Some(ast::Direction::Out) {
                                continue;
                            }
                            let Some(field_value) = fields.get(&k[field.len()..]) else {
                                continue;
                            };
                            let expr = self.lower_expr(field_value);
                            let ctx = self.next_ctx_at(ast::expr_span(field_value));
                            self.out.drivers.push(Driver {
                                span: self.cur_span,
                                target: *child_id,
                                cond: None,
                                expr,
                                meta: None,
                                ctx,
                            });
                        }
                    }
                    // An *array* literal on a composite port (`.v = [1, 9]`)
                    // drives one leaf per element. Only the struct form was
                    // handled, so this connection was dropped in silence and
                    // the child read its default — a scalar port has always
                    // accepted a literal here.
                    if let ast::Expr::Array { elems, .. } = value {
                        // Declared index order, numerically: `v[10]` must not
                        // sort before `v[2]`.
                        let mut elements: Vec<(i64, SignalId, Option<ast::Direction>)> = leaves
                            .iter()
                            .filter_map(|(k, id, dir)| {
                                let rest = k.strip_prefix(&idx)?;
                                let index = rest.strip_suffix(']')?.parse::<i64>().ok()?;
                                Some((index, *id, *dir))
                            })
                            .collect();
                        elements.sort_by_key(|(index, _, _)| *index);
                        for ((_, child_id, dir), elem) in elements.iter().zip(elems) {
                            if *dir == Some(ast::Direction::Out) {
                                continue;
                            }
                            let expr = self.lower_expr(elem);
                            let ctx = self.next_ctx_at(ast::expr_span(elem));
                            self.out.drivers.push(Driver {
                                span: self.cur_span,
                                target: *child_id,
                                cond: None,
                                expr,
                                meta: None,
                                ctx,
                            });
                        }
                    }
                    continue;
                }
                // A composite (struct/array) port: wire each leaf to the matching
                // leaf of the parent signal (`.s = link` -> `s.valid`<->`link.valid`).
                // The parent side must be a signal path.
                let Some(base) = expr_path(value) else {
                    continue;
                };
                leaves.sort_by(|a, b| a.0.cmp(&b.0));
                for (k, child_id, dir) in leaves {
                    let suffix = &k[field.len()..]; // ".valid", "[0]"
                    let Some(&parent_id) = self.locals.get(&format!("{base}{suffix}")) else {
                        continue;
                    };
                    // An `inout` leaf is already aliased to this parent net (same
                    // signal), so its drivers fold through `Resolve` directly —
                    // wiring it again would make a self-driver.
                    if parent_id == child_id {
                        continue;
                    }
                    let ctx = self.next_ctx_at(ast::expr_span(value));
                    if dir == Some(ast::Direction::Out) {
                        self.out.drivers.push(Driver {
                            span: self.cur_span,
                            target: parent_id,
                            cond: None,
                            expr: Expr::Current(child_id),
                            meta: None,
                            ctx,
                        });
                    } else {
                        self.out.drivers.push(Driver {
                            span: self.cur_span,
                            target: child_id,
                            cond: None,
                            expr: Expr::Current(parent_id),
                            meta: None,
                            ctx,
                        });
                    }
                }
            }
        }

        // Behaviour: statements in one explicit process share a driver
        // context (source-order override); processes and bare concurrent
        // statements receive separate contexts (parallel-driver resolution).
        for im in &impls {
            for item in &im.items {
                match item {
                    ast::ImplItem::Process(process) => {
                        self.lint_generated_dead_assignments(process.body.stmts.iter());
                        self.cur_ctx += 1;
                        if let Some(name) = &process.name {
                            self.out.process_labels.insert(
                                self.cur_ctx,
                                format!("{}::{}", self.cur_instance_path, name.text),
                            );
                        }
                        for statement in &process.body.stmts {
                            self.lower_stmt(statement, None);
                        }
                    }
                    ast::ImplItem::Stmt(statement) => {
                        self.cur_ctx += 1;
                        self.lower_stmt(statement, None);
                    }
                    _ => {}
                }
            }
        }

        // Restore the caller's scope.
        self.lower_stack.pop();
        self.locals = saved_locals;
        self.local_enum = saved_enum;
        self.local_struct = saved_struct;
        self.local_struct_repr = saved_struct_repr;
        self.local_char = saved_char;
        self.local_array = saved_array;
        self.local_numeric = saved_numeric;
        self.instance_arrays = saved_instance_arrays;
        self.cur_env = saved_env;
        self.cur_type_env = saved_type_env;
        self.consts = saved_consts;
        self.const_values = saved_const_values;
        self.cur_instance_path = saved_instance_path;
        ports
    }
}
