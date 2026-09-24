//! Item-level checks: extern signatures, struct cycles and layouts, views,
//! and attribute targets.

use super::*;

impl<'a> Checker<'a> {
    /// Type-check one top-level item.
    pub(super) fn check_item(&mut self, item: &Item) {
        self.check_item_type_layouts(item);
        let sym = HashMap::new();
        let sym = &sym;
        match item {
            Item::Const(c) => {
                self.check_const_not_entity(c);
                self.check_expr(&c.value, sym);
            }
            Item::Enum(e) => {
                self.check_enum(e);
                for v in &e.variants {
                    if let Some(val) = &v.value {
                        self.check_expr(val, sym);
                    }
                }
                // Variants are values: two sharing a discriminant compare equal
                // and are indistinguishable at runtime (and in a waveform).
                // Only explicit constants are checked — implicit numbering
                // cannot collide.
                let mut seen: HashMap<i64, &str> = HashMap::new();
                for v in &e.variants {
                    let Some(val) = &v.value else { continue };
                    let Some(n) = Self::const_literal(val) else {
                        continue;
                    };
                    if let Some(prev) = seen.insert(n, v.name.text.as_str()) {
                        self.error(
                            codes::DUPLICATE_ITEM,
                            v.name.span,
                            format!(
                                "`{}::{}` and `{}::{prev}` both have the value {n}",
                                e.name.text, v.name.text, e.name.text
                            ),
                        );
                    }
                }
            }
            Item::Entity(e) => {
                for port in &e.ports {
                    self.check_applied_view(&port.ty);
                }
                for a in &e.attrs {
                    self.check_attr_target(a, "entity", Some(e.name.text.as_str()));
                    self.check_attr_value(a);
                    if let Some(v) = &a.value {
                        self.check_expr(v, sym);
                    }
                }
            }
            Item::Impl(im) => {
                self.check_applied_view(&im.target);
                self.check_impl(im);
            }
            Item::Trait(t) => {
                for f in &t.items {
                    if let Some(b) = &f.body {
                        let saved =
                            self.push_type_params(f.generics.params.iter().map(|p| &p.name.text));
                        // The std operator/literal hook traits use an empty
                        // body as an intrinsic placeholder. A real default
                        // body, however, is ordinary inlined code and must
                        // return on every path like any other function.
                        self.check_function_block(f, b, None, !b.stmts.is_empty());
                        *self.type_params.borrow_mut() = saved;
                    }
                }
            }
            Item::Fn(f) => {
                // A generic fn's body is verified at each call (it inlines),
                // where the concrete types are known; checking it abstractly
                // (operators on the opaque `T`) would wrongly reject it.
                if f.generics.params.is_empty() {
                    if let Some(b) = &f.body {
                        self.check_function_block(f, b, None, true);
                    }
                } else if let Some(body) = &f.body {
                    // Operator/type checks wait for monomorphization, but
                    // reaching the end without a value is independent of the
                    // concrete type arguments and is already invalid here.
                    let names = f
                        .params
                        .iter()
                        .filter_map(|parameter| {
                            Some((
                                parameter.name.as_ref()?.text.clone(),
                                parameter
                                    .ty
                                    .as_ref()
                                    .map(|ty| self.ast_ty(ty))
                                    .unwrap_or(Ty::Error),
                            ))
                        })
                        .collect();
                    self.check_function_fallthrough(f, body, &names);
                }
            }
            Item::ExternBlock { fns, .. } => {
                for function in fns {
                    self.check_extern_c_signature(function);
                }
            }
            // A struct newtype needs no check of its own: the form carries no
            // body, so there is nothing to validate past its base type.
            Item::Struct(_) => {}
            Item::View(v) => self.check_view(v),
            Item::Using(_) | Item::AttrDecl(_) => {}
        }
    }

    /// Validate the scalar ABI the current LLVM and generated-C backends
    /// implement. Accepting a broader source type is dangerous here: both
    /// backends otherwise lower every non-real value as one `uint64_t`, which
    /// silently truncates wide vectors and treats aggregate layouts as scalar
    /// words. Void calls in statement position are likewise not represented
    /// in hardware IR yet, so reject them instead of dropping their effects.
    pub(super) fn check_extern_c_signature(&mut self, function: &FnDecl) {
        if !function.generics.params.is_empty() {
            self.error(
                codes::TYPE_MISMATCH,
                function.name.span,
                format!(
                    "extern C function `{}` cannot have generic parameters",
                    function.name.text
                ),
            );
        }
        for parameter in &function.params {
            let Some(ty) = &parameter.ty else {
                self.error(
                    codes::TYPE_MISMATCH,
                    parameter
                        .name
                        .as_ref()
                        .map_or(function.name.span, |name| name.span),
                    format!(
                        "extern C parameter in `{}` needs an explicit ABI type",
                        function.name.text
                    ),
                );
                continue;
            };
            self.check_extern_c_type(function, ty, "parameter");
        }
        match &function.ret {
            Some(ty) => self.check_extern_c_type(function, ty, "return type"),
            None => self.error_with_help(
                codes::TYPE_MISMATCH,
                function.name.span,
                format!(
                    "void extern C function `{}` is not supported yet",
                    function.name.text
                ),
                "extern calls currently need a scalar return value; statement-only C calls have no hardware IR representation"
                    .to_string(),
            ),
        }
    }

    /// Check that an `extern "C"` parameter or result has a representable ABI
    /// type: `real` maps to `double`, integer-shaped types to 64-bit words.
    pub(super) fn check_extern_c_type(&mut self, function: &FnDecl, ty: &Type, position: &str) {
        let checked = self.ast_ty(ty);
        let supported = matches!(checked, Ty::Integer | Ty::Real)
            || matches!(
                checked,
                Ty::Array {
                    len: 1..=64,
                    family: Some(_),
                    ..
                }
            );
        if supported || checked == Ty::Error {
            return;
        }
        let detail = match checked {
            Ty::Array {
                len,
                family: Some(_),
                ..
            } if len > 64 => {
                format!("packed value is {len} bits, but the current C ABI carries one 64-bit word")
            }
            Ty::Array { .. } | Ty::Named(_) => {
                "aggregate and nominal values have no C layout mapping".to_string()
            }
            Ty::Char => "Char has no declared C character ABI".to_string(),
            Ty::Void => "void is not a value ABI type".to_string(),
            Ty::Integer | Ty::Real | Ty::Error => unreachable!(),
        };
        self.error_with_help(
            codes::TYPE_MISMATCH,
            type_head_span(ty).unwrap_or(function.name.span),
            format!(
                "unsupported extern C {position} `{}` in `{}`: {detail}",
                crate::syntax::pretty::type_str(ty),
                function.name.text
            ),
            "use `real`, `integer`, or a packed numeric type of at most 64 bits; wrap other C signatures in a scalar C adapter"
                .to_string(),
        );
    }

    /// Validate constant layout bounds before elaboration/lowering tries to
    /// flatten them. Symbolic widths are checked after substitution; here we
    /// catch source constants that cannot fit the compiler's `u32` layout
    /// representation and would otherwise truncate or attempt an impossible
    /// allocation during error recovery.
    /// A struct that contains itself, directly or through other structs, has
    /// no finite layout. Elaboration flattens a struct into leaf signals, so
    /// one of these recursed until the process aborted with no diagnostic —
    /// `struct A { f: B } struct B { f: A }` was enough.
    pub(super) fn check_struct_field_cycles(&mut self) {
        let mut names: Vec<String> = self.struct_field_types.keys().cloned().collect();
        names.sort();
        let mut found = Vec::new();
        for name in &names {
            if let Some((field, span, through)) = self.self_containing(name) {
                found.push((name.clone(), field, span, through));
            }
        }
        for (name, field, span, through) in found {
            let path = if through == name {
                format!("`{name}` contains itself")
            } else {
                format!("`{name}` contains itself through `{through}`")
            };
            self.error_with_help(
                codes::TYPE_MISMATCH,
                span,
                format!("{path}, so it has no finite layout"),
                format!(
                    "field `{field}` would have to hold another `{name}`, without end; \
                     hardware has no indirection to break the cycle"
                ),
            );
        }
    }

    /// The field that closes a containment cycle back to `start`, with the
    /// struct it goes through. Breadth-first so the shortest cycle is found.
    pub(super) fn self_containing(&self, start: &str) -> Option<(String, Span, String)> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut queue: Vec<(String, String, Span, String)> = self
            .struct_field_types
            .get(start)?
            .iter()
            .map(|(f, head, span)| (head.clone(), f.clone(), *span, head.clone()))
            .collect();
        while !queue.is_empty() {
            let mut next = Vec::new();
            for (current, field, span, through) in queue {
                if current == start {
                    return Some((field, span, through));
                }
                if !seen.insert(current.clone()) {
                    continue;
                }
                if let Some(fields) = self.struct_field_types.get(&current) {
                    for (_, head, _) in fields {
                        next.push((head.clone(), field.clone(), span, through.clone()));
                    }
                }
            }
            queue = next;
        }
        None
    }

    /// Reject a type whose layout cannot be computed, such as an unconstrained
    /// array in a position that needs a width.
    pub(super) fn check_type_layout(&mut self, ty: &Type) {
        match ty {
            Type::Path(_) => {}
            Type::Indexed { base, index, .. } => {
                self.check_type_layout(base);
                let Some(index) = index.as_deref() else {
                    return;
                };
                match index {
                    Expr::Range { lo, hi, .. } => {
                        let (Some(left), Some(right)) = (signed_lit(lo), signed_lit(hi)) else {
                            return;
                        };
                        let length = (i128::from(left) - i128::from(right)).unsigned_abs() + 1;
                        if length > u128::from(u32::MAX) {
                            self.error(
                                codes::TYPE_MISMATCH,
                                expr_span(index),
                                format!(
                                    "range contains {length} elements, exceeding the compiler \
                                     layout maximum of {}",
                                    u32::MAX
                                ),
                            );
                        }
                    }
                    _ => {
                        let Some(width) = signed_lit(index) else {
                            return;
                        };
                        if width < 0 {
                            self.error(
                                codes::TYPE_MISMATCH,
                                expr_span(index),
                                "a type width cannot be negative".to_string(),
                            );
                        } else if u32::try_from(width).is_err() {
                            self.error(
                                codes::TYPE_MISMATCH,
                                expr_span(index),
                                format!(
                                    "type width {width} exceeds the compiler layout maximum of {}",
                                    u32::MAX
                                ),
                            );
                        }
                    }
                }
            }
            Type::Generic { base, args, .. } => {
                self.check_type_layout(base);
                for argument in args {
                    match argument {
                        GenericArg::PositionalType(ty) | GenericArg::NamedType { ty, .. } => {
                            self.check_type_layout(ty);
                        }
                        GenericArg::Positional(_) | GenericArg::Named { .. } => {}
                    }
                }
            }
            Type::View { target, .. } => self.check_type_layout(target),
        }
    }

    /// Check the type layouts in a function signature.
    pub(super) fn check_fn_type_layouts(&mut self, function: &FnDecl) {
        self.check_param_type_layouts(&function.generics);
        for parameter in &function.params {
            if let Some(ty) = &parameter.ty {
                self.check_type_layout(ty);
            }
        }
        if let Some(ret) = &function.ret {
            self.check_type_layout(ret);
        }
    }

    /// Check the type layouts in a generic parameter list's bounds.
    pub(super) fn check_param_type_layouts(&mut self, parameters: &Params) {
        for parameter in &parameters.params {
            if let Some(bound) = &parameter.bound {
                self.check_type_layout(bound);
            }
        }
    }

    /// Check the type layouts an item declares.
    pub(super) fn check_item_type_layouts(&mut self, item: &Item) {
        match item {
            Item::Using(using) => {
                if let UsingKind::Alias { ty, .. } = &using.kind {
                    self.check_type_layout(ty);
                }
            }
            Item::Const(constant) => self.check_type_layout(&constant.ty),
            Item::Fn(function) => self.check_fn_type_layouts(function),
            Item::ExternBlock { fns, .. } => {
                for function in fns {
                    self.check_fn_type_layouts(function);
                }
            }
            Item::Struct(structure) => {
                self.check_param_type_layouts(&structure.params);
                if let Some(base) = &structure.base {
                    self.check_type_layout(base);
                }
                for field in &structure.fields {
                    self.check_type_layout(&field.ty);
                }
            }
            Item::View(view) => {
                self.check_param_type_layouts(&view.params);
                self.check_type_layout(&view.target);
            }
            Item::Enum(en) => {
                if let Some(repr) = &en.repr {
                    self.check_type_layout(repr);
                }
            }
            Item::Entity(entity) => {
                self.check_param_type_layouts(&entity.params);
                for port in &entity.ports {
                    self.check_type_layout(&port.ty);
                }
            }
            Item::Impl(implementation) => {
                self.check_param_type_layouts(&implementation.params);
                self.check_type_layout(&implementation.target);
                for item in &implementation.items {
                    match item {
                        ImplItem::Const(constant) => self.check_type_layout(&constant.ty),
                        ImplItem::Let(declaration) => {
                            if let Some(ty) = &declaration.ty {
                                self.check_type_layout(ty);
                            }
                        }
                        ImplItem::Fn(function) => self.check_fn_type_layouts(function),
                        ImplItem::ModeField { .. } | ImplItem::Process(_) | ImplItem::Stmt(_) => {}
                    }
                }
            }
            Item::Trait(trait_) => {
                self.check_param_type_layouts(&trait_.params);
                for function in &trait_.items {
                    self.check_fn_type_layouts(function);
                }
            }
            Item::AttrDecl(attribute) => self.check_type_layout(&attribute.ty),
        }
    }

    /// Check a `view`: every field it names must exist on the target struct.
    pub(super) fn check_view(&mut self, view: &ViewDecl) {
        let target_ty = &view.target;
        let Some(target) = self.type_key(target_ty) else {
            return;
        };
        let target_name = self.key_leaf(&target).to_string();
        if !self.structs.contains_key(&target) {
            self.error(
                codes::TYPE_MISMATCH,
                type_head_span(target_ty).unwrap_or(view.span),
                format!(
                    "view `{}` must target a struct, found `{target_name}`",
                    view.name.text,
                ),
            );
            return;
        }
        let fields = self.base_struct_fields(target_ty);
        let mut seen = HashSet::new();
        for f in &view.fields {
            if !fields.iter().any(|n| n == &f.name.text) {
                self.error(
                    codes::TYPE_MISMATCH,
                    f.name.span,
                    format!("struct `{target}` has no field `{}`", f.name.text),
                );
            } else if !seen.insert(f.name.text.clone()) {
                self.error(
                    codes::DUPLICATE_ITEM,
                    f.name.span,
                    format!("view field `{}` is declared more than once", f.name.text),
                );
            } else if let Some(visibility) = self.field_visibility_for(&target, &f.name.text) {
                // A view is allowed to make private storage part of an
                // interface only when it is declared by that storage's own
                // module. Otherwise a foreign module could publish private
                // representation simply by wrapping it in a view.
                if !visibility.is_pub && self.module_of(view.span) != visibility.module {
                    self.sink.emit(
                        Diagnostic::error(format!(
                            "view `{}` cannot expose private field `{target_name}.{}` from another module",
                            view.name.text, f.name.text
                        ))
                        .with_code(codes::PRIVATE_MEMBER)
                        .at(f.name.span)
                        .label(visibility.span, "private field declared here")
                        .help("declare the view in the struct's module, or make the backing field `pub`"),
                    );
                }
            }
        }
        for field in fields {
            if !seen.contains(&field) {
                self.error(
                    codes::TYPE_MISMATCH,
                    view.name.span,
                    format!(
                        "view `{}` does not specify direction for `{field}`",
                        view.name.text
                    ),
                );
            }
        }
    }

    /// Check an applied view at a use site, so `Source Stream<T>` is verified
    /// against the view's declaration.
    pub(super) fn check_applied_view(&mut self, ty: &Type) {
        let Type::View { view, target, span } = ty else {
            return;
        };
        let Some(view_name) = view.segments.last().map(|i| i.text.as_str()) else {
            return;
        };
        let Some(key) = self.type_key(ty) else {
            return;
        };
        if !self.views.contains_key(&key) {
            let target_name = type_head_name(target).unwrap_or("<error>");
            let msg = format!("view `{view_name}` is not declared for struct `{target_name}`");
            // The two names in the reverse order is the pre-migration spelling
            // (`impl Source Stream`). Naming that outright beats a message that
            // reads backwards from what was written.
            if self
                .views
                .contains_key(&format!("{target_name}@{view_name}"))
            {
                self.error_with_help(
                    codes::TYPE_MISMATCH,
                    *span,
                    msg,
                    format!(
                        "write `{view_name} {target_name}` — the backing type leads \
                         and the view follows it, as in a port's `name: Type view`"
                    ),
                );
            } else {
                self.error(codes::TYPE_MISMATCH, *span, msg);
            }
        }
    }

    /// Spec 3.5: an attribute may only be applied to a target its declaration
    /// allows. Targets are item kinds (`entity`, `let`, `port`) or **type
    /// names** — `pub attr external_clock: Bool for Pll;` is valid only on
    /// the `Pll` entity or on declarations/instances of `Pll` (per-instance
    /// vendor metadata, preserved for external tools). Unknown attribute
    /// names on entities are reported by name resolution.
    pub(super) fn check_attr_target(&mut self, a: &Attr, kind: &str, type_name: Option<&str>) {
        let name = a
            .name
            .segments
            .last()
            .map(|s| s.text.as_str())
            .unwrap_or("");
        let verdict = self.attr_targets.get(name).map(|targets| {
            let ok = targets
                .iter()
                .any(|t| t == kind || Some(t.as_str()) == type_name);
            (ok, targets.join(", "))
        });
        if let Some((false, allowed)) = verdict {
            self.error(
                codes::INVALID_ATTR_TARGET,
                a.name.span,
                format!("attribute `{name}` cannot be applied to this {kind} (allowed: {allowed})"),
            );
        }
        self.warn_unimplemented_attr(name, a.name.span);
    }

    /// Attributes `std::attrs` declares and the compiler resolves, but which
    /// no stage reads. Writing one has no effect, and nothing else says so —
    /// `#[name = "foo"]` looks like it renames the emitted entity and does
    /// nothing at all.
    pub(super) fn warn_unimplemented_attr(&mut self, name: &str, span: Span) {
        let purpose = match name {
            "keep" => "preserving a signal through optimization",
            "library" => "the emitted library name",
            "name" => "the emitted entity name",
            _ => return,
        };
        self.warn(
            codes::UNIMPLEMENTED_ATTR,
            span,
            format!("attribute `{name}` has no effect yet"),
            &format!("it is reserved for {purpose}; nothing reads it today"),
        );
    }
}
