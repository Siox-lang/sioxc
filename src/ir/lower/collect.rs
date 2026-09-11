//! Design collection, entity traversal, and testbench layout persistence.

use super::*;

impl<'a> Lowering<'a> {
    pub(super) fn collect_instance_array_facts(
        &mut self,
        hierarchy: &Hierarchy,
        id: crate::elab::InstanceId,
        path: &str,
    ) {
        let instance = hierarchy.instance(id);
        if !instance.instance_arrays.is_empty() {
            self.instance_array_facts
                .insert(path.to_string(), instance.instance_arrays.clone());
        }
        for &child in &instance.children {
            let child_instance = hierarchy.instance(child);
            self.collect_instance_array_facts(
                hierarchy,
                child,
                &format!("{path}.{}", child_instance.name),
            );
        }
    }

    /// A lowering pass reporting into `sink`, over the given resolution.
    pub(super) fn new(sink: &'a mut DiagnosticSink, resolved: &'a Resolved) -> Self {
        Lowering {
            sink,
            resolved,
            expr_types: HashMap::new(),
            base_dir: std::path::PathBuf::new(),
            lint_defaulted: std::collections::HashSet::new(),
            entities: HashMap::new(),
            impls: HashMap::new(),
            inherent_impls: HashMap::new(),
            trait_decls: HashMap::new(),
            implemented_traits: HashMap::new(),
            entity_params: HashMap::new(),
            enum_variants: HashMap::new(),
            enum_first_disc: HashMap::new(),
            new_defaults: HashMap::new(),
            logic_encodings: HashMap::new(),
            structs: HashMap::new(),
            views: HashMap::new(),
            view_dirs: HashMap::new(),
            enum_reprs: HashMap::new(),
            enum_bases: HashMap::new(),
            op_impls: HashMap::new(),
            blanket_array_impls: HashMap::new(),
            suffix_impls: HashMap::new(),
            free_fns: FunctionIndex::new(resolved),
            inline_depth: std::cell::Cell::new(0),
            meta_temps: std::cell::RefCell::new(MetaTemps::inline_only()),
            expanding_structs: std::cell::RefCell::new(std::collections::HashSet::new()),
            depth_exceeded: std::cell::RefCell::new(Vec::new()),
            unresolved_names: std::cell::RefCell::new(Vec::new()),
            block_scopes: std::cell::RefCell::new(Vec::new()),
            unsupported_exprs: std::cell::RefCell::new(Vec::new()),
            instance_array_facts: HashMap::new(),
            cur_instance_path: String::new(),
            unelaborated_instance_uses: std::cell::RefCell::new(Vec::new()),
            self_signal: std::cell::Cell::new(None),
            param_types: std::cell::RefCell::new(HashMap::new()),
            param_integers: std::cell::RefCell::new(HashSet::new()),
            param_widths: std::cell::RefCell::new(HashMap::new()),
            consts: HashMap::new(),
            const_values: HashMap::new(),
            const_arrays: HashMap::new(),
            consts_real: HashMap::new(),
            const_ranges: HashMap::new(),
            aliases: HashMap::new(),
            cur_env: HashMap::new(),
            cur_type_env: HashMap::new(),
            lower_stack: Vec::new(),
            plain_out_ports: Vec::new(),
            undriven_lets: Vec::new(),
            unused_lets: Vec::new(),
            out: Design::default(),
            locals: HashMap::new(),
            local_enum: HashMap::new(),
            local_struct: HashMap::new(),
            local_struct_repr: HashMap::new(),
            local_char: std::collections::HashSet::new(),
            local_array: HashMap::new(),
            bad_operators: std::cell::RefCell::new(Vec::new()),
            bad_conversions: std::cell::RefCell::new(Vec::new()),
            instance_arrays: HashSet::new(),
            cur_span: None,
            reported_oob: HashSet::new(),
            reported_generated_dead_assignments: HashSet::new(),
            local_numeric: HashMap::new(),
            array_families: std::collections::HashSet::new(),
            cur_ctx: 0,
            ctx_span: HashMap::new(),
            sig_type: HashMap::new(),
        }
    }

    /// Index every declaration the lowering needs -- entities, structs, enums,
    /// functions and impls -- so later lookups are by definition rather than by
    /// name.
    pub(super) fn collect(&mut self, modules: &'a [Module]) {
        let mut constant_decls = Vec::new();
        for m in modules {
            for item in &m.items {
                match item {
                    ast::Item::Entity(e) => {
                        if let Some(id) = self.resolved.declared(e.name.span) {
                            self.entities.insert(id, e);
                        }
                    }
                    ast::Item::Fn(f) => {
                        self.free_fns.insert_free(f);
                    }
                    ast::Item::ExternBlock { fns, .. } => {
                        for f in fns {
                            self.free_fns.insert_free(f);
                        }
                    }
                    ast::Item::Struct(s) => {
                        self.structs
                            .insert(self.free_fns.struct_decl_key(&s.name), s);
                    }
                    ast::Item::View(v) => {
                        let target = self
                            .free_fns
                            .type_head_key(&v.target)
                            .unwrap_or_else(|| "<error>".to_string());
                        let key = format!("{}@{target}", self.free_fns.view_decl_key(&v.name));
                        self.views.insert(key.clone(), v);
                        self.view_dirs.insert(
                            key,
                            v.fields
                                .iter()
                                .map(|f| (f.name.text.clone(), f.dir))
                                .collect(),
                        );
                    }
                    // Module constants join the width environment; range
                    // constants (`const BYTE: range = 7..0`) keep their
                    // written direction. Aliases substitute during lowering.
                    ast::Item::Const(c) => {
                        constant_decls.push((self.free_fns.constant_decl_key(c), c));
                    }
                    ast::Item::Using(u) => {
                        if let ast::UsingKind::Alias { name, ty } = &u.kind {
                            self.aliases
                                .insert(self.free_fns.type_alias_decl_key(name), ty.clone());
                        }
                    }
                    ast::Item::Trait(t) => {
                        self.trait_decls
                            .insert(self.free_fns.trait_decl_key(&t.name), t);
                    }
                    ast::Item::Impl(im) if im.trait_.is_none() => {
                        self.register_static_fns(im);
                        if let Some(key) = self.free_fns.type_head_key(&im.target) {
                            self.inherent_impls.entry(key).or_default().push(im);
                        }
                        if let Some(id) = type_def_id(&im.target, self.resolved) {
                            self.impls.entry(id).or_default().push(im);
                        }
                    }
                    // A trait impl's first fn is the operator body for
                    // `impl "+" for T` (spec 3.25); an `impl Suffix<"ns", _>
                    // for T` defines the literal suffix named by its symbol
                    // argument, its `suffix` method inlined at the use site
                    // (spec 3.24).
                    ast::Item::Impl(im) => {
                        let trait_path = im.trait_.as_ref();
                        let trait_key =
                            trait_path.and_then(|path| self.free_fns.trait_path_key(path));
                        let target = self.free_fns.type_head_key(&im.target);
                        if let (Some(tr), Some(ty)) = (trait_key.as_ref(), target.as_ref()) {
                            self.implemented_traits
                                .entry(ty.clone())
                                .or_default()
                                .push(tr.clone());
                        }
                        self.register_static_fns(im);
                        if let (Some(tr), Some(ty)) = (trait_key.as_ref(), target.as_ref()) {
                            if tr == "Suffix" {
                                let symbol = im.trait_args.first().and_then(|a| match a {
                                    ast::GenericArg::Positional(ast::Expr::StrLit {
                                        text, ..
                                    }) => Some(text.clone()),
                                    _ => None,
                                });
                                if let Some(symbol) = symbol {
                                    for it in &im.items {
                                        if let ast::ImplItem::Fn(f) = it {
                                            self.suffix_impls
                                                .insert(symbol.clone(), (ty.clone(), f));
                                        }
                                    }
                                }
                            } else {
                                // `impl Operator<"+", integer, _> for T`: the
                                // symbol keys the impl and the next trait
                                // argument names the rhs operand type. A
                                // non-operator trait (Resolve/New/From) keys by
                                // its own name and reads its first type arg.
                                let op_symbol = (tr == "Operator")
                                    .then(|| im.trait_args.first())
                                    .flatten()
                                    .and_then(|a| match a {
                                        ast::GenericArg::Positional(ast::Expr::StrLit {
                                            text,
                                            ..
                                        }) => Some(text.clone()),
                                        _ => None,
                                    });
                                let input_index = usize::from(op_symbol.is_some());
                                let rhs_arg =
                                    im.trait_args.get(input_index).and_then(|a| match a {
                                        ast::GenericArg::Positional(ast::Expr::Path(p)) => {
                                            self.free_fns.type_path_key(p)
                                        }
                                        ast::GenericArg::PositionalType(ty) => {
                                            self.free_fns.type_head_key(ty)
                                        }
                                        _ => None,
                                    });
                                let operator = op_symbol.unwrap_or_else(|| tr.clone());
                                if is_blanket_array_impl(im) {
                                    let requirement = blanket_requirement(im, &self.free_fns)
                                        .unwrap_or_else(|| operator.clone());
                                    self.blanket_array_impls.insert(operator, requirement);
                                    continue;
                                }
                                for it in &im.items {
                                    if let ast::ImplItem::Fn(f) = it {
                                        self.op_impls
                                            .entry((operator.clone(), ty.clone()))
                                            .or_default()
                                            .push((f, rhs_arg.clone()));
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        // A trait's `self`-less defaults are associated functions the
        // implementing type inherits, callable as `Thing::tag()`. Registering
        // them needs every trait declaration, so it waits until collection is
        // done — a trait may be written after the impl that implements it.
        let inherited: Vec<(String, &'a ast::FnDecl)> = self
            .implemented_traits
            .iter()
            .flat_map(|(ty, traits)| traits.iter().map(move |tr| (ty.clone(), tr)))
            .filter_map(|(ty, tr)| self.trait_decls.get(tr.as_str()).map(|t| (ty, *t)))
            .flat_map(|(ty, t)| {
                t.items
                    .iter()
                    .filter(|f| f.body.is_some() && !f.params.iter().any(|p| p.is_self))
                    .map(move |f| (ty.clone(), f))
            })
            .collect();
        for (ty, f) in inherited {
            // The impl's own statics went in during collection, so this only
            // supplies what it omitted.
            self.free_fns
                .insert_associated_default(format!("{ty}::{}", f.name.text), f);
        }
        // Constants are order-independent. Keep the narrow signed value table
        // for widths/generate conditions, and a separate exact literal table
        // for signal values so a 128-bit constant never passes through i64.
        for _ in 0..=constant_decls.len() {
            let mut progressed = false;
            for (key, constant) in &constant_decls {
                let scope = self.consts.clone();
                progressed |= self.fold_const(key, constant, &scope);
            }
            if !progressed {
                break;
            }
        }
    }

    /// Register an impl's *static* associated fns (those without a `self`
    /// parameter) under a `Type::name` key, so `Unicode::code(c)` is callable
    /// in expressions through the same const-folding/inlining path as a
    /// module-level `fn`. Methods take `self` and dispatch via the receiver.
    pub(super) fn register_static_fns(&mut self, im: &'a ast::ImplDecl) {
        let Some(ty) = self.free_fns.type_head_key(&im.target) else {
            return;
        };
        for it in &im.items {
            if let ast::ImplItem::Fn(f) = it {
                if !f.params.iter().any(|p| p.is_self) {
                    self.free_fns
                        .insert_associated(format!("{ty}::{}", f.name.text), f);
                }
            }
        }
    }

    /// Lower one entity instance: its ports, state and behavior, under
    /// `root_path`.
    pub(super) fn lower_entity(&mut self, entity_id: DefId, root_path: &str) {
        let Some(edecl) = self.entities.get(&entity_id).copied() else {
            return;
        };
        // Extern entities are black boxes.
        if edecl.is_extern {
            return;
        }
        let mut env = self.consts.clone();
        env.extend(
            self.entity_params
                .get(&entity_id)
                .cloned()
                .unwrap_or_default(),
        );
        if is_test_entity(edecl, self.resolved) {
            // A testbench: lower only its DUT instances, each per-instance under
            // the testbench path (`CounterTest.dut.*`), so two instances of one
            // entity are distinct. Stimulus statements are interpreted by the
            // runner, and testbench<->DUT connections go through the runner's
            // signal map — so no top-level connection drivers here. Its local
            // values still need concrete layouts: the native runner consumes
            // the finished IR rather than independently specializing AST
            // declarations.
            self.persist_testbench_layouts(entity_id, root_path, &env);
            self.lower_testbench_duts(entity_id, root_path, &env);
            return;
        }
        // A top-level DUT: signals are entity-qualified (`Counter.count`), and
        // widths come from its first instance's parameters.
        self.lower_body(
            entity_id,
            root_path,
            &env,
            &HashMap::new(),
            &HashMap::new(),
            true,
        );
    }

    /// Persist concrete layouts for testbench-owned values without creating
    /// hardware signals for them. Native execution owns their storage, while
    /// `Design` remains the authoritative source for aggregate shape, ranges,
    /// scalar families, and widths.
    pub(super) fn persist_testbench_layouts(
        &mut self,
        entity_id: DefId,
        entity: &str,
        env: &HashMap<String, i64>,
    ) {
        let impls: Vec<&ast::ImplDecl> = self.impls.get(&entity_id).cloned().unwrap_or_default();
        for im in impls {
            for item in &im.items {
                let ast::ImplItem::Let(declaration) = item else {
                    continue;
                };
                if instance_let_parts(declaration, &self.entities, self.resolved).is_some() {
                    continue;
                }
                let Some(ty) = declaration.ty.as_ref() else {
                    continue;
                };
                let layout = self.source_layout(ty, env);
                self.persist_layout_tree(entity, &declaration.name.text, &layout);
            }
        }
    }

    /// Record the source layout for a value and, recursively, its fields, so
    /// consumers can rebuild the declared shape after flattening.
    pub(super) fn persist_layout_tree(&mut self, entity: &str, name: &str, layout: &SourceLayout) {
        self.out
            .source_layouts
            .insert(format!("{entity}.{name}"), layout.clone());
        match &layout.kind {
            LayoutKind::Struct { fields, .. } => {
                for field in fields {
                    self.persist_layout_tree(
                        entity,
                        &format!("{name}.{}", field.name),
                        &field.layout,
                    );
                }
            }
            LayoutKind::Array {
                range: Some(range),
                element,
            } => {
                for index in loop_range(range.left, range.right) {
                    self.persist_layout_tree(entity, &format!("{name}[{index}]"), element);
                }
            }
            LayoutKind::Array { range: None, .. }
            | LayoutKind::Packed { .. }
            | LayoutKind::Scalar { .. }
            | LayoutKind::Opaque { .. } => {}
        }
    }

    /// Lower each `let inst: Sub = { .. }` DUT of a testbench into its own
    /// namespace `<testbench>.<inst>.*` (with the DUT's internal logic and
    /// sub-instances). No testbench signals, statements, or top connections.
    pub(super) fn lower_testbench_duts(
        &mut self,
        entity_id: DefId,
        name: &str,
        env: &HashMap<String, i64>,
    ) {
        let impls: Vec<&ast::ImplDecl> = self.impls.get(&entity_id).cloned().unwrap_or_default();
        // Every port a testbench name is connected to, across all DUTs — when
        // one name binds an `out` and `in` ports (a DUT feeding another, or
        // its own input), the out drives the ins as real hardware, so the
        // value propagates on every settle without runner involvement.
        let mut bindings: HashMap<
            String,
            Vec<(SignalId, Option<ast::Direction>, crate::diag::Span)>,
        > = HashMap::new();
        for im in &impls {
            for item in &im.items {
                if let ast::ImplItem::Let(l) = item {
                    if let Some((cty, args)) = instance_let_parts(l, &self.entities, self.resolved)
                    {
                        if let Some(sub) = type_def_id(&cty, self.resolved) {
                            let sub_path = format!("{name}.{}", l.name.text);
                            let mut sub_env = self.consts.clone();
                            sub_env.extend(self.construct_params(&cty, sub, env));
                            let sub_tenv = self.construct_type_params(&cty, sub);
                            let sub_ports = self.lower_body(
                                sub,
                                &sub_path,
                                &sub_env,
                                &sub_tenv,
                                &HashMap::new(),
                                false,
                            );
                            for (port, value) in self.norm_conns(&args, sub) {
                                // The testbench name the port binds to; a
                                // literal/expression connection has no name.
                                let Some(tbname) = expr_path(&value) else {
                                    continue;
                                };
                                if let Some(&(sig, dir)) = sub_ports.get(&port) {
                                    bindings.entry(tbname).or_default().push((
                                        sig,
                                        dir,
                                        ast::expr_span(&value),
                                    ));
                                    continue;
                                }
                                // A struct/bus port is not one signal: it is
                                // flattened into leaves (`bus.valid`, ...), so
                                // the scalar lookup above finds nothing and the
                                // binding used to be dropped — two testbench
                                // DUTs sharing a struct local were left
                                // unconnected, silently. Bind each leaf to the
                                // matching leaf of the testbench name.
                                let prefix = format!("{port}.");
                                for (pname, &(sig, dir)) in sub_ports.iter() {
                                    let Some(leaf) = pname.strip_prefix(&prefix) else {
                                        continue;
                                    };
                                    bindings
                                        .entry(format!("{tbname}.{leaf}"))
                                        .or_default()
                                        .push((sig, dir, ast::expr_span(&value)));
                                }
                            }
                        }
                    }
                }
            }
        }
        for (tbname, ports) in &bindings {
            // A tristate net needs one shared node that folds each driver's
            // *expression*; the entity path builds one, this path has no
            // testbench signal to build it on. Connecting an `inout` here used
            // to bind nothing at all and read back high-Z, as though no one
            // were driving — report it instead of simulating a lie.
            if ports
                .iter()
                .any(|(_, d, _)| *d == Some(ast::Direction::Inout))
            {
                let span = ports
                    .iter()
                    .find(|(_, d, _)| *d == Some(ast::Direction::Inout))
                    .map(|(_, _, span)| *span)
                    .expect("an inout binding has a source span");
                self.sink.emit(
                    crate::diag::Diagnostic::error(format!(
                        "`{tbname}` connects an `inout` port between testbench instances"
                    ))
                    .with_code(crate::diag::codes::INVALID_METHOD_CALL)
                    .at(span)
                    .help(
                        "a shared tristate net is built inside an entity — wire the \
                         instances there and drive that entity from the testbench",
                    ),
                );
                continue;
            }
            let outs: Vec<SignalId> = ports
                .iter()
                .filter(|(_, d, _)| *d == Some(ast::Direction::Out))
                .map(|&(s, _, _)| s)
                .collect();
            let ins: Vec<SignalId> = ports
                .iter()
                .filter(|(_, d, _)| *d == Some(ast::Direction::In))
                .map(|&(s, _, _)| s)
                .collect();
            if outs.is_empty() || ins.is_empty() {
                continue;
            }
            // Each out contributes in its own context; several outs onto one
            // name then fold through the type's Resolve (or error), exactly
            // like parallel drivers anywhere else.
            for &o in &outs {
                let ctx = self.next_ctx();
                for &i in &ins {
                    self.out.drivers.push(Driver {
                        span: self.cur_span,
                        target: i,
                        cond: None,
                        expr: Expr::Current(o),
                        meta: None,
                        ctx,
                    });
                }
            }
        }
    }

    /// Lower entity `ename`'s body, naming signals under `path` (the instance
    /// path — `Counter.count` at the top, `Add2.s1.a` for a sub-instance) in the
    /// width environment `env`. Sub-instances (`let s: Sub = { .p = x, .. }`) are
    /// lowered recursively under `path.s` and their port connections become
    /// drivers. Returns each port's (signal, direction) so a parent can wire to
    /// it. Runs in a fresh name scope, restoring the caller's on return.
    /// Fold one constant declaration into the constant tables, returning
    /// whether it produced a value. `scope` is what integer expressions
    /// evaluate against: module constants see the constant table, an
    /// implementation's own constants also see the entity's parameters.
    ///
    /// Shared so the two callers cannot diverge by *kind* — the first version
    /// of implementation constants folded integers only, so a `const` holding
    /// a lookup table or a real reported its own name as unknown while the
    /// module-level spelling of it worked.
    pub(super) fn fold_const(
        &mut self,
        name: &str,
        constant: &ast::ConstDecl,
        scope: &HashMap<String, i64>,
    ) -> bool {
        if self.const_ranges.contains_key(name)
            || self.consts.contains_key(name)
            || self.consts_real.contains_key(name)
            || self.const_values.contains_key(name)
            || self.const_arrays.contains_key(name)
        {
            return false;
        }
        if let ast::Expr::Range { lo, hi, .. } = &constant.value {
            if let (Some(left), Some(right)) =
                (self.eval_const(lo, scope), self.eval_const(hi, scope))
            {
                self.const_ranges.insert(name.to_string(), (left, right));
                return true;
            }
        } else if let ast::Expr::Array { elems, .. } = &constant.value {
            // A constant lookup table. Every element has to fold, or the table
            // is left for a later round of the fixed point (an element may
            // name a constant not yet resolved).
            let values: Option<Vec<Expr>> = elems
                .iter()
                .map(|e| lower_const_value(e, &self.const_values, scope, &self.free_fns))
                .collect();
            if let Some(values) = values {
                self.const_arrays.insert(name.to_string(), values);
                return true;
            }
        } else if let Some(args) = self
            .free_fns
            .type_head_key(&constant.ty)
            .and_then(|head| self.positional_struct_args(&head, &constant.value))
        {
            // `const P: Pair = { 6, 7 };` — the same constant written without
            // field names. The declared type says these braces are a struct
            // literal, so they bind by declaration order.
            let Some(fields) = self.const_struct_fields(&constant.ty, &args) else {
                return false;
            };
            for (field, value) in fields {
                let key = format!("{name}.{field}");
                if let Some(narrow) = self.eval_const(&value, scope) {
                    self.consts.insert(key.clone(), narrow);
                }
                let Some(lowered) =
                    lower_const_value(&value, &self.const_values, scope, &self.free_fns)
                else {
                    return false;
                };
                self.const_values.insert(key, lowered);
            }
            return true;
        } else if let ast::Expr::Construct { args, .. } = &constant.value {
            // `const K: Pair = { .a = 4, .b = 5 };` — a struct constant is one
            // folded value per field, keyed by the dotted path a read spells.
            // Nothing folded it before, so the constant never entered any
            // table: `K.a` reported "has no hardware form" (a message about
            // runtime indices, on a source with no index) and `p = K` reported
            // `K` as an unknown name — both after stage 4 had accepted the
            // declaration with no diagnostic at all.
            let Some(fields) = self.const_struct_fields(&constant.ty, args) else {
                return false;
            };
            for (field, value) in fields {
                let key = format!("{name}.{field}");
                if let Some(narrow) = self.eval_const(&value, scope) {
                    self.consts.insert(key.clone(), narrow);
                }
                let Some(lowered) =
                    lower_const_value(&value, &self.const_values, scope, &self.free_fns)
                else {
                    return false;
                };
                self.const_values.insert(key, lowered);
            }
            return true;
        } else if let ast::Expr::Int { text, .. } = &constant.value {
            if text.contains('.') {
                if let Ok(value) = text.replace('_', "").parse::<f64>() {
                    self.consts_real.insert(name.to_string(), value);
                    return true;
                }
            } else if let Some(value) = integer_const(text) {
                if let Expr::Const(word) = value {
                    if let Ok(narrow) = i64::try_from(word) {
                        self.consts.insert(name.to_string(), narrow);
                    }
                    self.const_values
                        .insert(name.to_string(), Expr::Const(word));
                } else {
                    self.const_values.insert(name.to_string(), value);
                }
                return true;
            }
        } else if let ast::Expr::Path(path) = &constant.value {
            // `const M: Mode = Mode::Fast;` — an enum variant is a value like
            // any other. Only a single-segment path was folded, so the
            // constant never entered the tables and every read of it reported
            // the name as unknown; binding it to a signal first happened to
            // work, which is what made it look supported.
            if path.segments.len() >= 2 {
                if let Some(disc) = self.enum_variant_path(path) {
                    self.consts.insert(name.to_string(), disc as i64);
                    self.const_values
                        .insert(name.to_string(), Expr::Const(disc));
                    return true;
                }
            }
            if let Some(source) = self.free_fns.constant_path_key(path) {
                if let Some(value) = self.const_values.get(&source).cloned() {
                    self.const_values.insert(name.to_string(), value);
                    return true;
                } else if let Some(&value) = self.consts.get(&source) {
                    self.consts.insert(name.to_string(), value);
                    return true;
                }
            }
        } else if let Some(value) =
            lower_const_value(&constant.value, &self.const_values, scope, &self.free_fns)
        {
            self.const_values.insert(name.to_string(), value);
            if let Some(narrow) = self.eval_const(&constant.value, scope) {
                self.consts.insert(name.to_string(), narrow);
            }
            return true;
        } else if let Some(value) = self.eval_const(&constant.value, scope) {
            self.consts.insert(name.to_string(), value);
            return true;
        }
        false
    }

    /// Fold a constant expression in `env`, or `None` when it is not constant.
    pub(super) fn eval_const(
        &self,
        expression: &ast::Expr,
        env: &HashMap<String, i64>,
    ) -> Option<i64> {
        eval_const_fns(expression, env, &self.free_fns, 0)
    }
}
