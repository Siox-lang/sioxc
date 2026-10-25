//! Trait contracts, impl bodies and their process blocks, and the
//! environment an impl is checked in.

use super::*;

impl<'a> Checker<'a> {
    /// Type-check an `impl` block.
    pub(super) fn check_impl(&mut self, im: &ImplDecl) {
        // Stimulus primitives are meaningful in a testbench; in an entity body
        // lowering dropped them without a word.
        let saved_tb = self.in_testbench.replace(
            self.declarations
                .entity_type_id(&im.target)
                .is_some_and(|id| self.test_entities.contains(&id)),
        );
        let saved_params = self.push_type_params(im.params.params.iter().map(|p| &p.name.text));
        let concrete_self = self.ast_ty(self_ty(im));
        let saved_self = self.current_self_ty.replace(Some(concrete_self));
        let saved_impl_owner = self.current_impl_owner.replace(self.type_key(&im.target));
        let backing = self
            .type_key(self_ty(im))
            .unwrap_or_else(|| "<error>".to_string());
        let entity = self.declarations.entity_type_id(self_ty(im));
        for item in &im.items {
            if let ImplItem::Attr(declaration) = item {
                self.check_type_attr_decl(declaration, im.trait_.is_some());
                continue;
            }
            if let ImplItem::Process(process) = item {
                if im.trait_.is_some() || entity.is_none() {
                    self.error_with_help(
                        codes::PROCESS_PLACEMENT,
                        process.span,
                        "`process` is only allowed in an inherent entity implementation"
                            .to_string(),
                        "use a function body for sequential software/type behavior".to_string(),
                    );
                }
                continue;
            }
            let ImplItem::Fn(function) = item else {
                continue;
            };
            if function.is_pub && im.trait_.is_some() {
                self.error_with_help(
                    codes::PRIVATE_MEMBER,
                    function.name.span,
                    "trait implementation methods inherit the trait's visibility".to_string(),
                    "remove `pub` from this method".to_string(),
                );
            }
            if function.is_pub
                && entity.is_some()
                && function.params.iter().any(|parameter| parameter.is_self)
            {
                self.error_with_help(
                    codes::PRIVATE_MEMBER,
                    function.name.span,
                    format!(
                        "entity method `{}::{}` cannot be public yet",
                        self.key_leaf(&backing),
                        function.name.text
                    ),
                    "expose behavior through ports; cross-hierarchy method calls do not yet have defined hardware semantics".to_string(),
                );
            }
        }
        self.check_impl_inner(im);
        self.current_impl_owner.replace(saved_impl_owner);
        self.current_self_ty.replace(saved_self);
        *self.type_params.borrow_mut() = saved_params;
        self.in_testbench.set(saved_tb);
    }

    /// The body of [`Self::check_impl`], separated so the stimulus-context guard can
    /// wrap it.
    pub(super) fn check_impl_inner(&mut self, im: &ImplDecl) {
        // The supported constrained array implementations are lowered by the
        // element-wise vector machinery. Still validate their metadata here;
        // abstract `T[]` has no standalone runtime representation for the
        // ordinary body checker.
        if is_blanket_array_impl(im) {
            let sym = HashMap::new();
            for attr in &im.attrs {
                self.check_attr_target(
                    attr,
                    "impl",
                    crate::resolve::attributes::type_identity(&im.target, self.resolved),
                );
                self.check_attr_value(attr);
                if let Some(value) = &attr.value {
                    self.check_expr(value, &sym);
                }
            }
            return;
        }
        let (dirs, sym, ranged) = self.impl_env(im);
        let index_bounds = self.array_bounds.borrow().clone();
        for a in &im.attrs {
            self.check_attr_target(
                a,
                "impl",
                crate::resolve::attributes::type_identity(&im.target, self.resolved),
            );
            self.check_attr_value(a);
            if let Some(v) = &a.value {
                self.check_expr(v, &sym);
            }
        }
        for item in &im.items {
            match item {
                ImplItem::Const(c) => {
                    self.check_const_not_entity(c);
                    self.check_expr(&c.value, &sym);
                }
                ImplItem::Let(l) => {
                    // A signal named like one of the entity's ports: reads
                    // went to the port, the `let` was ignored, and the
                    // waveform held two `a`s in one scope.
                    if im.trait_.is_none() {
                        let port = self
                            .declarations
                            .entity_type_id(&im.target)
                            .and_then(|id| self.entities.get(&id))
                            .is_some_and(|ports| ports.iter().any(|p| p.name == l.name.text));
                        if port {
                            self.error_with_help(
                                codes::DUPLICATE_ITEM,
                                l.name.span,
                                format!("`{}` is already a port of this entity", l.name.text),
                                "rename the signal; a port and a signal share one namespace"
                                    .to_string(),
                            );
                        }
                    }
                    self.require_let_annotation(l);
                    self.check_struct_literal_fields(l, &sym);
                    self.check_signal_reset_value(l);
                    // Resolution owns implementation-member coherence across
                    // this block and every split inherent impl of the type.
                    // Per-instance attributes: valid for `let` targets or when
                    // a named target matches the declaration's type (the
                    // instance's entity, or the annotated type head).
                    let type_id = match &l.value {
                        Some(Expr::Construct { ty: Some(t), .. }) => {
                            crate::resolve::attributes::type_identity(t, self.resolved)
                        }
                        _ => l.ty.as_ref().and_then(|ty| {
                            crate::resolve::attributes::type_identity(ty, self.resolved)
                        }),
                    };
                    for a in &l.attrs {
                        if !self
                            .attr_identity(a)
                            .is_some_and(|id| self.attr_targets.contains_key(&id))
                        {
                            // Resolution already reported the unknown attribute.
                            continue;
                        }
                        self.check_attr_target(a, "let", type_id);
                        self.check_attr_value(a);
                    }
                    if let Some(v) = &l.value {
                        self.check_init(l.ty.as_ref(), v, &sym);
                        // A signal's initializer is elaborated: compile-time
                        // `read`/`exists` belong there.
                        let saved = self.in_initializer.replace(true);
                        self.check_expr(v, &sym);
                        self.in_initializer.set(saved);
                    }
                }
                ImplItem::Fn(f) => {
                    if let Some(b) = &f.body {
                        let saved =
                            self.push_type_params(f.generics.params.iter().map(|p| &p.name.text));
                        // A method inlines into the body it is called from,
                        // so everything that body may not drive, it may not
                        // drive either: the entity's `in` ports and its
                        // `const`s, plus the `in` leaves of a view's role.
                        // Method bodies were checked with no restrictions at
                        // all, so each of those was accepted inside a method
                        // and rejected three lines away, written inline.
                        let mut body_dirs = self.self_view_dirs(&im.target);
                        body_dirs.illegal.extend(dirs.illegal.iter().cloned());
                        body_dirs
                            .plain_in_roots
                            .extend(dirs.plain_in_roots.iter().cloned());
                        body_dirs.consts.extend(dirs.consts.iter().cloned());
                        let mut body_ranged = ranged.clone();
                        // A parameter shadows the impl-level name it repeats,
                        // so `fn twice(a: unsigned[8]) { a = a + a; }` writes
                        // its own argument, not the entity's `in` port `a`.
                        let params: HashSet<&str> = f
                            .params
                            .iter()
                            .filter_map(|p| p.name.as_ref())
                            .map(|n| n.text.as_str())
                            .collect();
                        let shadowed = |name: &String| {
                            params.contains(name.split(['.', '[']).next().unwrap_or(name))
                        };
                        body_dirs.illegal.retain(|n| !shadowed(n));
                        body_dirs.plain_in_roots.retain(|n| !shadowed(n));
                        body_dirs.consts.retain(|n| !shadowed(n));
                        body_ranged.retain(|n, _| !shadowed(n));
                        // Types for what the function itself declares: its
                        // parameters, and `self` as the impl's target. Without
                        // them the body had no types at all, so the strict
                        // assignment-width rule never fired inside a method —
                        // `self.data = wide` silently truncated a 16-bit
                        // argument into an 8-bit field.
                        let mut body_sym: HashMap<String, Ty> = HashMap::new();
                        let mut body_index_bounds: HashMap<String, (i64, i64)> =
                            self.array_bounds.borrow().clone();
                        for param in &f.params {
                            if param.is_self {
                                body_sym.insert("self".to_string(), self.ast_ty(self_ty(im)));
                            } else if let (Some(n), Some(t)) = (&param.name, &param.ty) {
                                body_sym.insert(n.text.clone(), self.ast_ty(t));
                                if let Some(range) = self.declared_range(t) {
                                    body_ranged.insert(n.text.clone(), range);
                                }
                                body_index_bounds.remove(&n.text);
                                if let Some(range) = self.declared_index_bounds(t) {
                                    body_index_bounds.insert(n.text.clone(), range);
                                }
                            }
                        }
                        let expected = f.ret.as_ref().map(|ty| self.ast_ty(ty));
                        self.check_function_fallthrough(f, b, &body_sym);
                        let saved_fn = self.current_fn.replace(self.resolved.declared(f.name.span));
                        self.check_block_with(
                            b,
                            &body_dirs,
                            &body_ranged,
                            &body_sym,
                            &body_index_bounds,
                            expected.as_ref(),
                        );
                        self.current_fn.set(saved_fn);
                        *self.type_params.borrow_mut() = saved;
                    }
                }
                ImplItem::ModeField { .. } | ImplItem::AttrBinding(_) | ImplItem::Attr(_) => {}
                ImplItem::Process(process) => {
                    self.check_process_block(&process.body, &dirs, &ranged, &sym, &index_bounds)
                }
                ImplItem::Stmt(s) => self.check_stmt(s, &dirs, &sym, &ranged, None),
            }
        }
    }

    /// Check a `process` body, with the index bounds and port directions in
    /// scope for its statements.
    pub(super) fn check_process_block(
        &mut self,
        block: &Block,
        view_dirs: &PortDirs,
        bounds: &HashMap<String, (i64, i64)>,
        names: &HashMap<String, Ty>,
        index_bounds: &HashMap<String, (i64, i64)>,
    ) {
        let saved_index_bounds = self.array_bounds.replace(index_bounds.clone());
        let saved_process = self.in_process.replace(true);
        self.check_stmt_sequence(&block.stmts, view_dirs, names, bounds, None);
        self.in_process.set(saved_process);
        self.array_bounds.replace(saved_index_bounds);
    }

    /// Build the value environment for an impl body: the `in` ports (for the
    /// write check) and a name -> type table (ports + impl-level lets/consts).
    pub(super) fn impl_env(&self, im: &ImplDecl) -> ImplEnvironment {
        let mut illegal = HashSet::new();
        let mut consts: HashSet<String> = HashSet::new();
        let mut plain_in_roots = HashSet::new();
        let mut sym = HashMap::new();
        let mut ranged: HashMap<String, (i64, i64)> = HashMap::new();
        // Names are impl-local, so this starts empty for each one.
        self.array_bounds.borrow_mut().clear();
        if im.trait_.is_none() {
            if let Some(ports) = self
                .declarations
                .entity_type_id(&im.target)
                .and_then(|id| self.entities.get(&id))
            {
                for p in ports {
                    sym.insert(p.name.clone(), p.ty.clone());
                    if let Some(r) = p.range {
                        ranged.insert(p.name.clone(), r);
                    }
                    if let Some(bounds) = p.index_bounds {
                        self.array_bounds
                            .borrow_mut()
                            .insert(p.name.clone(), bounds);
                    }
                    if p.dir == Some(Direction::In) {
                        illegal.insert(p.name.clone());
                        // A *plain* (non-bus-mode) `in` port has no writable
                        // parts: driving a field/index of it is illegal too.
                        if p.view.is_none() {
                            plain_in_roots.insert(p.name.clone());
                        }
                    }
                    // A bus-mode port contributes each `in` leaf (`bus.ready`),
                    // so driving it inside the entity is rejected (spec 3.19).
                    if let Some(dirs) = p.view.clone().and_then(|k| self.view_dirs.get(&k)) {
                        for (field, dir) in dirs {
                            if *dir == Direction::In {
                                illegal.insert(format!("{}.{field}", p.name));
                            }
                        }
                    }
                }
            }
        }
        for it in &im.items {
            match it {
                ImplItem::Let(l) => {
                    let mut ty = l.ty.as_ref().map(|t| self.ast_ty(t)).unwrap_or(Ty::Error);
                    // An unconstrained local still acquires fixed storage in a
                    // native test executable. Retain the initializer's known
                    // shape in the statement environment so later writes are
                    // checked against that storage instead of treating `len=0`
                    // as a wildcard forever.
                    if let Ty::Array { len, .. } = &mut ty {
                        if *len == 0 {
                            *len = match l.value.as_ref() {
                                Some(Expr::StrLit { text, .. }) => {
                                    u32::try_from(text.chars().count()).unwrap_or(u32::MAX)
                                }
                                Some(Expr::Array { elems, .. }) => {
                                    u32::try_from(elems.len()).unwrap_or(u32::MAX)
                                }
                                Some(value) => match self.type_of(value, &sym) {
                                    Ty::Array { len, .. } => len,
                                    _ => 0,
                                },
                                None => 0,
                            };
                        }
                    }
                    sym.insert(l.name.text.clone(), ty);
                    self.type_cache.borrow_mut().clear();
                    if let Some(r) = l.ty.as_ref().and_then(|t| self.declared_range(t)) {
                        ranged.insert(l.name.text.clone(), r);
                    }
                    if let Some(b) = l.ty.as_ref().and_then(|t| self.declared_index_bounds(t)) {
                        self.array_bounds
                            .borrow_mut()
                            .insert(l.name.text.clone(), b);
                    }
                }
                ImplItem::Const(c) => {
                    sym.insert(c.name.text.clone(), self.ast_ty(&c.ty));
                    self.type_cache.borrow_mut().clear();
                    consts.insert(c.name.text.clone());
                }
                _ => {}
            }
        }
        (
            PortDirs {
                illegal,
                plain_in_roots,
                consts,
                loop_vars: HashSet::new(),
            },
            sym,
            ranged,
        )
    }
}
