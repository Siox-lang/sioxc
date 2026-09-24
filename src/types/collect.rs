//! Declaration collection: the registries `Checker` builds before the
//! checking walk, so it can answer questions about items it has not reached.

use super::*;

impl<'a> Checker<'a> {
    /// A checker over `modules`, seeded with the standard attribute targets so
    /// `std::attrs` validates like any other declaration.
    pub(super) fn new(
        sink: &'a mut DiagnosticSink,
        resolved: &'a Resolved,
        modules: &[Module],
    ) -> Self {
        // Seed the std::attrs targets so the standard attributes validate while
        // `std/` is still empty (mirrors the builtins seeded in siox-resolve).
        let mut attr_targets = HashMap::new();
        for (name, targets) in [
            ("test", &["entity"][..]),
            ("keep", &["let", "port"][..]),
            ("library", &["entity"][..]),
            ("name", &["entity"][..]),
            ("precedence", &["impl"][..]),
        ] {
            attr_targets.insert(
                name.to_string(),
                targets.iter().map(|s| s.to_string()).collect(),
            );
        }
        let mut attr_value_kinds = HashMap::new();
        for (name, ty) in [
            ("test", AttrValueTy::Bool),
            ("keep", AttrValueTy::Bool),
            ("library", AttrValueTy::Str),
            ("name", AttrValueTy::Str),
            ("precedence", AttrValueTy::Integer),
        ] {
            attr_value_kinds.insert(name.to_string(), ty);
        }
        let file_modules: HashMap<crate::diag::FileId, String> = modules
            .iter()
            .map(|module| {
                let name = module
                    .path
                    .segments
                    .iter()
                    .map(|segment| segment.text.as_str())
                    .collect::<Vec<_>>()
                    .join("::");
                (module.span.file, name)
            })
            .collect();
        let entity_names = modules
            .iter()
            .flat_map(|module| module.items.iter())
            .filter_map(|item| match item {
                Item::Entity(entity) => resolved
                    .declared(entity.name.span)
                    .and_then(|id| resolved.qualified_name(id)),
                _ => None,
            })
            .collect();
        let trait_visibility = modules
            .iter()
            .flat_map(|module| module.items.iter())
            .filter_map(|item| match item {
                Item::Trait(trait_) => resolved
                    .declared(trait_.name.span)
                    .and_then(|id| {
                        let definition = resolved.def(id)?;
                        if is_compiler_trait(resolved, id) {
                            Some(definition.name.clone())
                        } else {
                            resolved.qualified_name(id)
                        }
                    })
                    .map(|key| {
                        (
                            key,
                            (
                                trait_.is_pub,
                                file_modules
                                    .get(&trait_.span.file)
                                    .cloned()
                                    .unwrap_or_else(|| format!("<file:{}>", trait_.span.file.0)),
                                trait_.name.span,
                            ),
                        )
                    }),
                _ => None,
            })
            .collect();
        Checker {
            test_entities: HashSet::new(),
            in_testbench: std::cell::Cell::new(false),
            in_fn_body: std::cell::Cell::new(false),
            in_match_arm: std::cell::Cell::new(false),
            type_params: std::cell::RefCell::new(HashSet::new()),
            sink,
            resolved,
            entities: HashMap::new(),
            attr_targets,
            attr_value_kinds,
            trait_impls: HashMap::new(),
            trait_defaults: HashMap::new(),
            trait_impls_by_type: HashMap::new(),
            trait_required: HashMap::new(),
            trait_visibility,
            operator_sigs: HashMap::new(),
            index_sigs: HashMap::new(),
            operator_precedence: HashMap::new(),
            enum_variants: HashMap::new(),
            own_variants: HashMap::new(),
            enum_bases: HashMap::new(),
            structs: HashMap::new(),
            field_visibility: HashMap::new(),
            struct_field_types: HashMap::new(),
            field_decl_types: HashMap::new(),
            array_bounds: std::cell::RefCell::new(HashMap::new()),
            views: HashMap::new(),
            array_families: HashSet::new(),
            array_elements: HashMap::new(),
            blanket_array_impls: HashMap::new(),
            generic_fns: HashMap::new(),
            fn_arity: HashMap::new(),
            fn_param_types: HashMap::new(),
            fn_return_types: HashMap::new(),
            const_types: HashMap::new(),
            suffix_types: HashMap::new(),
            prefix_types: HashMap::new(),
            aliases: HashMap::new(),
            expanding: std::cell::RefCell::new(HashSet::new()),
            methods: HashMap::new(),
            method_param_types: HashMap::new(),
            method_has_self: HashMap::new(),
            method_visibility: HashMap::new(),
            private_entity_members: HashMap::new(),
            view_dirs: HashMap::new(),
            expr_types: std::cell::RefCell::new(HashMap::new()),
            checked_struct_literals: std::cell::RefCell::new(HashSet::new()),
            current_self_ty: std::cell::RefCell::new(None),
            current_impl_owner: std::cell::RefCell::new(None),
            file_modules,
            entity_names,
        }
    }

    /// Finish checking and hand back the recorded expression types.
    pub(super) fn finish(self) -> Typed {
        Typed {
            expr_types: self.expr_types.into_inner(),
        }
    }

    /// First pass: record entity port types and declared attribute targets.
    pub(super) fn collect(&mut self, modules: &[Module]) {
        // Two passes: gather type declarations (structs, enums, aliases,
        // attrs, impls) first, so entity-port typing below can already see
        // e.g. `struct unsigned : Logic[]` regardless of module/item order.
        for m in modules {
            for item in &m.items {
                if matches!(item, Item::Entity(_)) {
                    continue;
                }
                self.collect_decl(item);
            }
        }
        // A newtype deriving from another nominal array family is itself one
        // (`struct Byte : unsigned[8]`); resolve that transitively before typing
        // ports, so such a type is treated as a numeric vector.
        // A trait's defaulted methods belong to every type that implements it
        // and did not provide its own. Collection order is not guaranteed — a
        // trait may be declared after the impl — so this is a second pass.
        let mut inherited: Vec<InheritedMethodSignature> = Vec::new();
        for (ty, traits) in &self.trait_impls_by_type {
            for tr in traits {
                let Some(methods) = self.trait_defaults.get(tr) else {
                    continue;
                };
                for (name, ret, params, has_self) in methods {
                    inherited.push((
                        ty.clone(),
                        name.clone(),
                        ret.clone(),
                        params.clone(),
                        *has_self,
                    ));
                }
            }
        }
        for (ty, name, ret, params, has_self) in inherited {
            let key = (ty, name);
            self.methods.entry(key.clone()).or_insert(ret);
            self.method_param_types.entry(key.clone()).or_insert(params);
            self.method_has_self.entry(key).or_insert(has_self);
        }
        self.resolve_array_families();
        self.resolve_array_elements();
        for m in modules {
            for item in &m.items {
                if let Item::Entity(e) = item {
                    let Some(entity_key) = self.declaration_key(e.name.span) else {
                        continue;
                    };
                    let ports = e
                        .ports
                        .iter()
                        .map(|p| PortInfo {
                            name: p.name.text.clone(),
                            ty: self.ast_ty(&p.ty),
                            dir: p.dir,
                            view: self
                                .type_key(&p.ty)
                                .filter(|key| self.views.contains_key(key)),
                            range: self.declared_range(&p.ty),
                            index_bounds: self.declared_index_bounds(&p.ty),
                        })
                        .collect();
                    if e.attrs.iter().any(|attribute| {
                        crate::resolve::is_enabled_std_test_attribute(self.resolved, attribute)
                    }) {
                        self.test_entities.insert(entity_key.clone());
                    }
                    self.entities.insert(entity_key, ports);
                }
            }
        }
        // (inherited enum variants are expanded in collect_decl's tail)
        self.break_derivation_cycles();
        self.expand_inherited_variants();
    }

    /// Collect one non-entity type declaration.
    pub(super) fn collect_decl(&mut self, item: &Item) {
        match item {
            Item::AttrDecl(a) => {
                let targets = a.targets.iter().map(|t| t.text.clone()).collect();
                self.attr_targets.insert(a.name.text.clone(), targets);
                let kind = match type_head_name(&a.ty) {
                    Some("Bool") => AttrValueTy::Bool,
                    Some("string") | Some("str") => AttrValueTy::Str,
                    Some("integer") => AttrValueTy::Integer,
                    _ => AttrValueTy::Other,
                };
                self.attr_value_kinds.insert(a.name.text.clone(), kind);
            }
            Item::Impl(im) => {
                // Record every impl method by (type head, name) with its
                // declared return type, so `recv.method(args)` types (spec 3.20).
                if let Some(ty) = self.type_key(&im.target) {
                    for it in &im.items {
                        match it {
                            ImplItem::Fn(f) => {
                                let key = (ty.clone(), f.name.text.clone());
                                self.methods.insert(key.clone(), f.ret.clone());
                                self.method_param_types.insert(
                                    key.clone(),
                                    f.params
                                        .iter()
                                        .filter(|parameter| !parameter.is_self)
                                        .map(|parameter| parameter.ty.clone())
                                        .collect(),
                                );
                                self.method_has_self.insert(
                                    key,
                                    f.params.iter().any(|parameter| parameter.is_self),
                                );
                                let trait_contract = im
                                    .trait_
                                    .as_ref()
                                    .and_then(|path| self.trait_key(path))
                                    .and_then(|key| self.trait_visibility.get(&key).cloned());
                                self.method_visibility.insert(
                                    (ty.clone(), f.name.text.clone()),
                                    MemberVisibility {
                                        is_pub: trait_contract
                                            .as_ref()
                                            .map(|(is_pub, _, _)| *is_pub)
                                            .unwrap_or(f.is_pub || im.trait_.is_some()),
                                        type_private: im.trait_.is_none(),
                                        owner: ty.clone(),
                                        module: trait_contract
                                            .as_ref()
                                            .map(|(_, module, _)| module.clone())
                                            .unwrap_or_else(|| self.module_of(im.span)),
                                        span: trait_contract
                                            .map(|(_, _, span)| span)
                                            .unwrap_or(f.name.span),
                                    },
                                );
                            }
                            ImplItem::Let(declaration) if self.entity_names.contains(&ty) => {
                                self.private_entity_members.insert(
                                    (ty.clone(), declaration.name.text.clone()),
                                    declaration.name.span,
                                );
                            }
                            ImplItem::Const(constant) if self.entity_names.contains(&ty) => {
                                self.private_entity_members.insert(
                                    (ty.clone(), constant.name.text.clone()),
                                    constant.name.span,
                                );
                            }
                            ImplItem::ModeField { name, .. } if self.entity_names.contains(&ty) => {
                                self.private_entity_members
                                    .insert((ty.clone(), name.text.clone()), name.span);
                            }
                            _ => {}
                        }
                    }
                }
                // Record `impl Trait for Type` so trait-driven checks (e.g.
                // conditions) can ask "does T implement Trait?".
                if let (Some(tr), Some(ty)) = (&im.trait_, self.type_key(&im.target)) {
                    if let Some(key) = self.trait_key(tr) {
                        self.trait_impls_by_type.entry(ty).or_default().push(key);
                    }
                }
                if let Some(tr) = &im.trait_ {
                    let trait_name = self.trait_key(tr);
                    let target = self.type_key(&im.target);
                    if let (Some(mut t), Some(ty)) = (trait_name, target) {
                        // `impl Operator<"<sym>", Input, Output> for T`: the
                        // first trait argument is the operator symbol, which
                        // keys the impl. A user operator (a non-standard symbol)
                        // must declare `#[precedence = N]`; the standard symbols
                        // carry built-in precedence.
                        let operator = if t == "Operator" {
                            im.trait_args.first().and_then(|a| match a {
                                GenericArg::Positional(Expr::StrLit { text, .. }) => {
                                    Some(text.clone())
                                }
                                _ => None,
                            })
                        } else {
                            None
                        };
                        if let Some(symbol) = &operator {
                            t = symbol.clone();
                            if crate::syntax::ast::is_reserved_operator(symbol) {
                                let hint = if crate::syntax::ast::is_comparison_operator(symbol) {
                                    " — derive comparisons from a `<=>` impl instead"
                                } else {
                                    " and cannot be overloaded"
                                };
                                self.error(
                                    codes::TYPE_MISMATCH,
                                    im.span,
                                    format!("`{symbol}` is reserved by the language{hint}"),
                                );
                            } else if !crate::syntax::ast::is_builtin_operator(symbol) {
                                let precedence = im.attrs.iter().find_map(|a| {
                                    (a.name
                                        .segments
                                        .last()
                                        .is_some_and(|n| n.text == "precedence"))
                                    .then_some(a)
                                    .and_then(|a| a.value.as_ref())
                                    .and_then(|v| match v {
                                        Expr::Int { text, span } => text
                                            .replace('_', "")
                                            .parse::<u8>()
                                            .ok()
                                            .map(|p| (p, *span)),
                                        _ => None,
                                    })
                                });
                                match precedence {
                                    Some((value, span)) => {
                                        if let Some((previous, previous_span)) =
                                            self.operator_precedence.get(symbol).copied()
                                        {
                                            if previous != value {
                                                self.sink.emit(
                                                    Diagnostic::error(format!(
                                                        "custom operator `{symbol}` has precedence {value}, but another implementation uses {previous}"
                                                    ))
                                                    .with_code(codes::TYPE_MISMATCH)
                                                    .at(span)
                                                    .label(previous_span, "previous precedence declared here"),
                                                );
                                            }
                                        } else {
                                            self.operator_precedence
                                                .insert(symbol.clone(), (value, span));
                                        }
                                    }
                                    None => self.error(
                                        codes::TYPE_MISMATCH,
                                        im.span,
                                        format!(
                                            "custom operator `{symbol}` requires `#[precedence = N]`"
                                        ),
                                    ),
                                }
                            }
                        }
                        // `impl Suffix<"ns", _> for T` / `impl Prefix<"x", _>
                        // for T`: the first trait argument is the affix symbol
                        // and the impl target `T` is what the literal produces
                        // (spec 3.24). std owns which affixes exist.
                        if t == "Suffix" || t == "Prefix" {
                            if let Some(GenericArg::Positional(Expr::StrLit { text, .. })) =
                                im.trait_args.first()
                            {
                                let table = if t == "Suffix" {
                                    &mut self.suffix_types
                                } else {
                                    &mut self.prefix_types
                                };
                                table.entry(text.clone()).or_default().push(ty.clone());
                            }
                        }
                        // Operator overload signature: `input`/`output` are the
                        // 2nd/3rd trait arguments (after the symbol), falling
                        // back to the `apply` method's rhs-param / return types.
                        if operator.is_some() {
                            let arg_name = |index: usize| {
                                im.trait_args.get(index + 1).and_then(|a| match a {
                                    GenericArg::Positional(Expr::Path(p)) => self.path_key(p),
                                    GenericArg::PositionalType(ty) => self.type_key(ty),
                                    _ => None,
                                })
                            };
                            let input = arg_name(0).or_else(|| {
                                im.items.iter().find_map(|item| match item {
                                    ImplItem::Fn(f) => f
                                        .params
                                        .iter()
                                        .find(|p| !p.is_self)
                                        .and_then(|p| p.ty.as_ref())
                                        .and_then(|ty| self.type_key(ty)),
                                    _ => None,
                                })
                            });
                            let output = arg_name(1).or_else(|| {
                                im.items.iter().find_map(|item| match item {
                                    ImplItem::Fn(f) => {
                                        f.ret.as_ref().and_then(|ty| self.type_key(ty))
                                    }
                                    _ => None,
                                })
                            });
                            self.operator_sigs
                                .entry((t.clone(), ty.clone()))
                                .or_default()
                                .push((input, output));
                        }
                        if matches!(t.as_str(), "Index" | "IndexAssign") {
                            let arg_name = |index: usize| {
                                im.trait_args.get(index).and_then(|a| match a {
                                    GenericArg::Positional(Expr::Path(p)) => self.path_key(p),
                                    GenericArg::PositionalType(ty) => self.type_key(ty),
                                    _ => None,
                                })
                            };
                            let index_type = arg_name(0);
                            let value_type = arg_name(1);
                            self.index_sigs
                                .entry((t.clone(), ty.clone()))
                                .or_default()
                                .push((index_type, value_type));
                        }
                        if is_blanket_array_impl(im) {
                            let requirement =
                                self.blanket_requirement(im).unwrap_or_else(|| t.clone());
                            let supported = is_liftable_array_key(&t);
                            if !supported {
                                self.error(
                                    codes::TYPE_MISMATCH,
                                    im.span,
                                    format!(
                                        "element-wise array forwarding is not implemented for `{t}`"
                                    ),
                                );
                            }
                            let matching_bound = requirement == t;
                            if supported && !matching_bound {
                                self.error(
                                    codes::TYPE_MISMATCH,
                                    im.span,
                                    format!(
                                        "element-wise `{t}` forwarding requires the element bound `{t}`, found `{requirement}`"
                                    ),
                                );
                            }
                            if supported && matching_bound {
                                self.blanket_array_impls.insert(t, requirement);
                            }
                        } else {
                            self.trait_impls.entry(t).or_default().insert(ty);
                        }
                    }
                }
            }
            Item::Trait(t) => {
                let Some(trait_key) = self
                    .resolved
                    .declared(t.name.span)
                    .and_then(|id| self.trait_definition_key(id))
                else {
                    return;
                };
                let required = t
                    .items
                    .iter()
                    .filter(|f| f.body.is_none())
                    .map(|f| f.name.text.clone())
                    .collect();
                self.trait_required.insert(trait_key.clone(), required);
                self.trait_defaults.insert(
                    trait_key,
                    t.items
                        .iter()
                        .filter(|f| f.body.is_some())
                        .map(|f| {
                            (
                                f.name.text.clone(),
                                f.ret.clone(),
                                f.params
                                    .iter()
                                    .filter(|parameter| !parameter.is_self)
                                    .map(|parameter| parameter.ty.clone())
                                    .collect(),
                                f.params.iter().any(|parameter| parameter.is_self),
                            )
                        })
                        .collect(),
                );
            }
            Item::Enum(e) => {
                let Some(enum_key) = self.declaration_key(e.name.span) else {
                    return;
                };
                let vars: Vec<String> = e.variants.iter().map(|v| v.name.text.clone()).collect();
                self.own_variants.insert(enum_key.clone(), vars.clone());
                self.enum_variants.insert(enum_key.clone(), vars);
                if let Some(t) = &e.repr {
                    if let Some(base) = self.type_key(t) {
                        self.enum_bases.insert(enum_key, base);
                    }
                }
            }
            Item::ExternBlock { fns, .. } => {
                for f in fns {
                    let Some(id) = self.resolved.declared(f.name.span) else {
                        continue;
                    };
                    self.fn_arity
                        .insert(id, f.params.iter().filter(|p| !p.is_self).count());
                    self.fn_param_types.insert(
                        id,
                        f.params
                            .iter()
                            .filter(|p| !p.is_self)
                            .map(|p| p.ty.clone())
                            .collect(),
                    );
                    self.fn_return_types.insert(id, f.ret.clone());
                }
            }
            Item::Const(constant) => {
                if let Some(id) = self.resolved.declared(constant.name.span) {
                    self.const_types.insert(id, constant.ty.clone());
                }
            }
            Item::Fn(f) if !f.generics.params.is_empty() => {
                let Some(id) = self.resolved.declared(f.name.span) else {
                    return;
                };
                self.fn_arity
                    .insert(id, f.params.iter().filter(|p| !p.is_self).count());
                let params = f
                    .params
                    .iter()
                    .filter(|p| !p.is_self)
                    .map(|p| p.ty.clone())
                    .collect();
                self.generic_fns
                    .insert(id, (f.generics.params.clone(), params));
                self.fn_param_types.insert(
                    id,
                    f.params
                        .iter()
                        .filter(|p| !p.is_self)
                        .map(|p| p.ty.clone())
                        .collect(),
                );
                self.fn_return_types.insert(id, f.ret.clone());
            }
            Item::Fn(f) => {
                let Some(id) = self.resolved.declared(f.name.span) else {
                    return;
                };
                self.fn_arity
                    .insert(id, f.params.iter().filter(|p| !p.is_self).count());
                // Concrete functions can validate every parameter directly;
                // the generic arm above records the same shape and defers only
                // type-parameter substitution to each call site.
                self.fn_param_types.insert(
                    id,
                    f.params
                        .iter()
                        .filter(|p| !p.is_self)
                        .map(|p| p.ty.clone())
                        .collect(),
                );
                self.fn_return_types.insert(id, f.ret.clone());
            }
            Item::Struct(st) => {
                let Some(struct_key) = self.declaration_key(st.name.span) else {
                    return;
                };
                let fields = st.fields.iter().map(|f| f.name.text.clone()).collect();
                self.structs
                    .insert(struct_key.clone(), (st.base.clone(), fields));
                self.field_decl_types.insert(
                    struct_key.clone(),
                    st.fields
                        .iter()
                        .map(|f| (f.name.text.clone(), f.ty.clone()))
                        .collect(),
                );
                let module = self.module_of(st.span);
                for field in &st.fields {
                    self.field_visibility.insert(
                        (struct_key.clone(), field.name.text.clone()),
                        MemberVisibility {
                            is_pub: field.is_pub,
                            type_private: true,
                            owner: struct_key.clone(),
                            module: module.clone(),
                            span: field.name.span,
                        },
                    );
                }
                self.struct_field_types.insert(
                    struct_key,
                    st.fields
                        .iter()
                        .filter_map(|f| {
                            Some((f.name.text.clone(), self.type_key(&f.ty)?, f.name.span))
                        })
                        .collect(),
                );
            }
            Item::View(v) => {
                let Some(key) = self
                    .declaration_key(v.name.span)
                    .zip(self.type_key(&v.target))
                    .map(|(view, target)| format!("{view}@{target}"))
                else {
                    return;
                };
                if self.views.insert(key.clone(), v.target.clone()).is_some() {
                    self.error(
                        codes::DUPLICATE_ITEM,
                        v.name.span,
                        format!(
                            "view `{}` is declared more than once for the same backing type",
                            v.name.text
                        ),
                    );
                }
                let dirs = v
                    .fields
                    .iter()
                    .map(|f| (f.name.text.clone(), f.dir))
                    .collect();
                self.view_dirs.insert(key, dirs);
            }
            Item::Using(u) => {
                if let UsingKind::Alias { name, ty } = &u.kind {
                    if let Some(alias_key) = self.declaration_key(name.span) {
                        self.aliases.insert(alias_key, ty.clone());
                    }
                }
            }
            _ => {}
        }
    }

    /// Nominal enum derivation: prepend base variants (spec derived types).
    /// A base that isn't a known enum is a numeric repr — ignore it.
    pub(super) fn expand_inherited_variants(&mut self) {
        let names: Vec<String> = self.enum_variants.keys().cloned().collect();
        for name in &names {
            let mut chain = Vec::new();
            let mut cur = name.clone();
            let mut prefix: Vec<String> = Vec::new();
            while let Some(base) = self.enum_bases.get(&cur).cloned() {
                if !self.enum_variants.contains_key(&base) || chain.contains(&base) {
                    break; // numeric repr, or cycle
                }
                chain.push(base.clone());
                cur = base;
            }
            for anc in chain.iter().rev() {
                if let Some(vs) = self.own_variants.get(anc) {
                    prefix.extend(vs.iter().cloned());
                }
            }
            if !prefix.is_empty() {
                let own = self.enum_variants.get(name).cloned().unwrap_or_default();
                prefix.extend(own);
                self.enum_variants.insert(name.clone(), prefix);
            }
        }
    }

    /// Derived-enum validation (spec 3.28): `enum B(A);` is a newtype over
    /// `A`'s variants, so `A` must itself be an enum. An enum carries no
    /// storage annotation — its width is derived from its variants and
    /// discriminants, and a specific wire width belongs to whatever carries
    /// the value (a port, a field, a function's return type), each of which
    /// already declares a type.
    ///
    /// Extension needs no check: the newtype form takes no body, so adding
    /// variants is not expressible. The older `enum B : A { … }` spelling is
    /// reported by the parser, which owns that message.
    pub(super) fn check_enum(&mut self, e: &EnumDecl) {
        let Some(repr) = &e.repr else { return };
        let Some(head) = self.type_key(repr) else {
            return;
        };
        if self.own_variants.contains_key(&head) {
            return;
        }
        let name = &e.name.text;
        let shown = self.key_leaf(&head);
        self.error_with_help(
            codes::TYPE_MISMATCH,
            e.name.span,
            format!("`{shown}` is not an enum, so `{name}` cannot derive from it"),
            format!(
                "an enum's width is derived from its variants — write `enum {name} \
                 {{ … }}` and, where a specific width is needed, declare it at the \
                 boundary that carries the value (a port, a field, or a function's \
                 return type) and convert there"
            ),
        );
    }

    /// Drop the base of any struct that (transitively) derives from itself.
    /// Resolve reports the cycle; this makes the table *acyclic* so the many
    /// walkers over it — field collection, width, vector-family fixpoint —
    /// terminate. Guarding each one individually was whack-a-mole: the crash
    /// was a stack overflow that aborted the process, so any missed walker is
    /// another core dump.
    pub(super) fn break_derivation_cycles(&mut self) {
        let names: Vec<String> = self.structs.keys().cloned().collect();
        let mut cyclic: Vec<String> = Vec::new();
        for name in names {
            let mut seen: HashSet<String> = HashSet::new();
            let mut cur = name.clone();
            while seen.insert(cur.clone()) {
                let Some(next) = self
                    .structs
                    .get(&cur)
                    .and_then(|(b, _)| b.as_ref())
                    .and_then(|ty| self.type_key(ty))
                else {
                    break;
                };
                if next == name {
                    cyclic.push(name.clone());
                    break;
                }
                cur = next;
            }
        }
        for name in cyclic {
            if let Some((base, _)) = self.structs.get_mut(&name) {
                *base = None;
            }
        }
    }

    /// The (transitive) field names of a struct-shaped base type.
    pub(super) fn base_struct_fields(&self, ty: &Type) -> Vec<String> {
        self.base_struct_fields_at(ty, &mut HashSet::new())
    }

    /// The field names of a struct head, including those inherited from a
    /// nominal derivation base.
    pub(super) fn base_struct_fields_named(&self, head: &str) -> Vec<String> {
        self.base_struct_fields_named_at(head, &mut HashSet::new())
    }

    /// How many fields a struct definition has, for positional construction.
    pub(super) fn struct_field_count(&self, id: crate::resolve::DefId) -> Option<usize> {
        let key = self.definition_key(id)?;
        self.struct_field_count_at(&key, &mut HashSet::new())
    }

    /// As [`Self::struct_field_count`], guarding against a derivation cycle.
    pub(super) fn struct_field_count_at(
        &self,
        name: &str,
        seen: &mut HashSet<String>,
    ) -> Option<usize> {
        if !seen.insert(name.to_string()) {
            return None;
        }
        let (base, own) = self.structs.get(name)?;
        let inherited = match base.as_ref().and_then(|ty| self.type_key(ty)) {
            Some(base) if self.structs.contains_key(&base) => {
                self.struct_field_count_at(&base, seen)?
            }
            _ => 0,
        };
        seen.remove(name);
        inherited.checked_add(own.len())
    }

    /// Cycle-safe because resolution reports cyclic derivation but checking
    /// continues best-effort.
    pub(super) fn base_struct_fields_at(
        &self,
        ty: &Type,
        seen: &mut HashSet<String>,
    ) -> Vec<String> {
        let Some(head) = self.type_key(ty) else {
            return Vec::new();
        };
        self.base_struct_fields_named_at(&head, seen)
    }

    /// As [`Self::base_struct_fields_named`], guarding against a derivation cycle.
    pub(super) fn base_struct_fields_named_at(
        &self,
        head: &str,
        seen: &mut HashSet<String>,
    ) -> Vec<String> {
        let mut out = Vec::new();
        if !seen.insert(head.to_string()) {
            return out;
        }
        if let Some((base, own)) = self.structs.get(head) {
            if let Some(base) = base.as_ref().and_then(|ty| self.type_key(ty)) {
                out.extend(self.base_struct_fields_named_at(&base, seen));
            }
            out.extend(own.iter().cloned());
        }
        seen.remove(head);
        out
    }

    /// A struct field's declared type, following the derivation chain so a
    /// field inherited from a base struct types the same as its own.
    pub(super) fn field_decl_ty(&self, head: &str, field: &str) -> Option<Type> {
        let mut current = head.to_string();
        let mut seen: HashSet<String> = HashSet::new();
        loop {
            if !seen.insert(current.clone()) {
                return None;
            }
            if let Some(ty) = self
                .field_decl_types
                .get(&current)
                .and_then(|m| m.get(field))
            {
                return Some(ty.clone());
            }
            match self
                .structs
                .get(&current)
                .and_then(|(base, _)| base.clone())
            {
                Some(base) => current = self.type_key(&base)?,
                None => return None,
            }
        }
    }

    /// Whether `name` is a nominal newtype over an array, directly or through
    /// another nominal array family. There is no signedness — that lives in
    /// the family's operator impls.
    pub(super) fn is_array_family(&self, name: &str) -> bool {
        self.array_families.contains(name)
    }

    /// Fixpoint: a newtype directly over `T[]`, or over an already-known array
    /// family, is itself an array family. `struct Byte(unsigned[8])` therefore
    /// inherits unsigned's representation without a marker trait.
    pub(super) fn resolve_array_families(&mut self) {
        loop {
            let mut changed = false;
            let names: Vec<String> = self.structs.keys().cloned().collect();
            for name in names {
                if self.array_families.contains(&name) {
                    continue;
                }
                let Some((base, fields)) = self.structs.get(&name) else {
                    continue;
                };
                if !fields.is_empty() {
                    continue;
                }
                let is_array = match base {
                    // The indexed type is the representation itself. Its
                    // element may be scalar or aggregate; lowering preserves
                    // that base shape.
                    Some(Type::Indexed { .. }) => true,
                    Some(Type::Path(p)) => self
                        .path_key(p)
                        .is_some_and(|head| self.array_families.contains(&head)),
                    _ => false,
                };
                if is_array {
                    self.array_families.insert(name);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// Resolve each array family's element type, following newtype chains to the
    /// underlying element.
    pub(super) fn resolve_array_elements(&mut self) {
        for family in self.array_families.clone() {
            let mut current = family.clone();
            let mut seen = HashSet::new();
            while seen.insert(current.clone()) {
                let Some((base, _)) = self.structs.get(&current) else {
                    break;
                };
                let Some(base) = base else {
                    break;
                };
                let Some(head) = self.type_key(base) else {
                    break;
                };
                if matches!(base, Type::Indexed { .. }) && !self.array_families.contains(&head) {
                    self.array_elements.insert(family.clone(), head);
                    break;
                }
                current = head;
            }
        }
    }

    /// Whether `owner` implements the trait `key`, directly or through a blanket
    /// impl.
    pub(super) fn has_impl(&self, key: &str, owner: &str) -> bool {
        if self
            .trait_impls
            .get(key)
            .is_some_and(|types| types.contains(owner))
        {
            return true;
        }
        let Some(element) = self.array_elements.get(owner) else {
            return false;
        };
        self.blanket_array_impls
            .get(key)
            .is_some_and(|requirement| {
                self.trait_impls
                    .get(requirement)
                    .is_some_and(|types| types.contains(element))
            })
    }
}
