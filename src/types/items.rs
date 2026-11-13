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
                // A constant is checked against its declared type, as a `let`
                // initializer is; `const X: integer = "hello";` passed (#487).
                self.check_init(Some(&c.ty), &c.value, sym);
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
                // Implicit numbering counts on from the previous variant, as
                // lowering numbers them, so it can collide with an explicit
                // value too (`Zero, One, Two = 1`, #184).
                // In `i128`, so a discriminant from `2^63` to `u64::MAX` is
                // read and counted on from like any other (#421).
                let mut seen: HashMap<i128, &str> = HashMap::new();
                let mut next = Some(0i128);
                for v in &e.variants {
                    let n = match &v.value {
                        Some(val) => match Self::fit_literal(val).or_else(|| self.const_int(val)) {
                            Some(n) => n,
                            None => {
                                // An integer literal no 64-bit value holds
                                // (`0x1_0000_0000_0000_0000`) is past a
                                // discriminant's range, not unknown (#475).
                                let wide = matches!(val, Expr::Int { text, .. }
                                    if !text.contains('.') && unsigned_lit_text(text).is_none());
                                if wide {
                                    self.error(
                                        codes::TYPE_MISMATCH,
                                        expr_span(val),
                                        format!(
                                            "`{}::{}` has the discriminant {}, past the 64 \
                                             bits a discriminant holds",
                                            e.name.text,
                                            v.name.text,
                                            crate::syntax::pretty::expr_string(val)
                                        ),
                                    );
                                } else {
                                    // A string, a real, an array or a runtime
                                    // call was skipped and numbering counted on
                                    // past it, colliding with later ones (#480).
                                    self.error_with_help(
                                        codes::TYPE_MISMATCH,
                                        expr_span(val),
                                        format!(
                                            "`{}::{}` has the discriminant `{}`, which is not \
                                             a constant integer",
                                            e.name.text,
                                            v.name.text,
                                            crate::syntax::pretty::expr_string(val)
                                        ),
                                        "a discriminant is an integer literal or a constant \
                                         integer expression"
                                            .to_string(),
                                    );
                                }
                                // Not a literal: the count is unknown from here.
                                next = None;
                                continue;
                            }
                        },
                        None => match next {
                            Some(n) => n,
                            None => continue,
                        },
                    };
                    next = n.checked_add(1);
                    let val = v.value.as_ref();
                    if n > i128::from(u64::MAX) {
                        self.error(
                            codes::TYPE_MISMATCH,
                            val.map(expr_span).unwrap_or(v.name.span),
                            format!(
                                "`{}::{}` has the discriminant {n}, past the 64 bits a \
                                 discriminant holds",
                                e.name.text, v.name.text
                            ),
                        );
                        continue;
                    }
                    // Discriminants are unsigned: the enum's width is what
                    // its largest one needs. A negative one was dropped in
                    // lowering and numbered from zero instead (#166).
                    if n < 0 {
                        self.error(
                            codes::TYPE_MISMATCH,
                            val.map(expr_span).unwrap_or(v.name.span),
                            format!(
                                "`{}::{}` has the negative discriminant {n}; an enum's \
                                 discriminants start at 0",
                                e.name.text, v.name.text
                            ),
                        );
                        continue;
                    }
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
                // A test is a root nothing instantiates: a port has nothing
                // to connect to, and lowering failed on its writes (#188).
                if let Some(port) = e.ports.first() {
                    if e.attrs.iter().any(|attribute| {
                        crate::resolve::is_enabled_std_test_attribute(self.resolved, attribute)
                    }) {
                        self.error_with_help(
                            codes::TYPE_MISMATCH,
                            port.name.span,
                            format!("test entity `{}` cannot have ports", e.name.text),
                            "a test drives its design through signals of its own: declare \
                             `let` signals in its impl and connect an instance to them"
                                .to_string(),
                        );
                    }
                }
                for port in &e.ports {
                    self.check_applied_view(&port.ty);
                    for a in &port.attrs {
                        self.check_attr_target(a, "port", type_head_name(&port.ty));
                        self.check_attr_value(a);
                    }
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
                self.check_inherited_defaults(im);
                self.check_display_fmt(im);
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
                // `fn puts(s: string) -> integer;` outside an `extern "C"`
                // block escaped the foreign ABI's checks and lowered as a
                // call nothing could run, or passed a string as a word
                // (#193, #194).
                if f.body.is_none() {
                    self.error_with_help(
                        codes::TYPE_MISMATCH,
                        f.name.span,
                        format!("function `{}` has no body", f.name.text),
                        format!(
                            "give it one, or declare a C function: `extern \"C\" {{ fn {}(..) -> ..; }}`",
                            f.name.text
                        ),
                    );
                }
                // A generic fn's body is verified at each call (it inlines),
                // where the concrete types are known; checking it abstractly
                // (operators on the opaque `T`) would wrongly reject it. One
                // generic only over values (`<W: integer>`) has no opaque
                // type, and its calls bind no type to check it with, so it
                // is checked here (#386).
                let value_only = f.generics.params.iter().all(|parameter| {
                    parameter
                        .bound
                        .as_ref()
                        .and_then(type_head_name)
                        .is_some_and(|head| head == "integer")
                });
                if let Some(body) = f.body.as_ref().filter(|_| !f.generics.params.is_empty()) {
                    for statement in &body.stmts {
                        if let Stmt::Let(declaration) = statement {
                            if declaration.is_static {
                                self.reject_generic_static(declaration);
                            }
                        }
                    }
                }
                if value_only {
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
            Item::ExternBlock { abi, fns, span } => {
                // The compiler's built-ins are `core`'s alone: anywhere else
                // the ABI would silently become a C call.
                if abi == "intrinsic" && !self.module_of(*span).starts_with("core::") {
                    self.error_with_help(
                        codes::LANG_ITEM,
                        *span,
                        "`extern \"intrinsic\"` is reserved for `core::intrinsics`".to_string(),
                        "call the built-ins through `std::math`, or declare a C function with \
                         `extern \"C\"`"
                            .to_string(),
                    );
                }
                for function in fns {
                    self.check_extern_c_signature(function);
                }
            }
            // A struct newtype needs no check of its own: the form carries no
            // body, so there is nothing to validate past its base type.
            Item::Struct(_) => {}
            Item::View(v) => self.check_view(v),
            Item::AttrDecl(attribute) => self.check_attr_default(attribute),
            Item::Using(_) | Item::AttrBinding(_) | Item::Macro(_) | Item::MacroCall { .. } => {}
        }
    }

    /// Validate the scalar ABI the LLVM backend and fixed runtime implement.
    /// Accepting a broader source type is dangerous here: the C ABI contract
    /// does not yet define aggregate or multiword ownership. Void calls in
    /// statement position are likewise not represented in hardware IR yet, so
    /// reject them instead of dropping their effects.
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
        // No return type is a C `void` function, called for its effect from
        // a testbench or a function body (checked at the call).
        if let Some(ty) = &function.ret {
            self.check_extern_c_type(function, ty, "return type");
        }
    }

    /// Record each `extern "C"` function taking a struct or array, before
    /// any call to one is checked.
    pub(super) fn collect_aggregate_extern_fns(&mut self, modules: &[Module]) {
        for module in modules {
            for item in &module.items {
                let Item::ExternBlock { fns, .. } = item else {
                    continue;
                };
                for function in fns {
                    let aggregate = function
                        .params
                        .iter()
                        .filter_map(|p| p.ty.as_ref())
                        .chain(function.ret.as_ref())
                        .any(|ty| {
                            let checked = self.ast_ty(ty);
                            checked.is_c_aggregate()
                                && !self.is_c_handle(&checked)
                                && self.c_aggregate_problem(ty, 0).is_none()
                        });
                    if let Some(id) = self
                        .resolved
                        .declared(function.name.span)
                        .filter(|_| aggregate)
                    {
                        self.aggregate_extern_fns.insert(id);
                    }
                }
            }
        }
    }

    /// Whether `ty` is `std::ffi::Handle`, an opaque C pointer passed by
    /// value.
    pub(super) fn is_c_handle(&self, ty: &Ty) -> bool {
        matches!(ty, Ty::Named(id) if self.resolved.lang_of(*id) == Some("c_handle"))
    }

    /// Why `ty` cannot cross to C as an aggregate, or `None`: a struct or
    /// fixed array whose leaves are `integer`, `natural`, `real`, ranged
    /// integers or packed values of up to 64 bits.
    fn c_aggregate_problem(&self, ty: &Type, depth: usize) -> Option<String> {
        if depth > 16 {
            return Some("it nests too deeply".to_string());
        }
        match self.ast_ty(ty) {
            Ty::Integer | Ty::Natural | Ty::Real | Ty::Error => None,
            // A packed value: one word, or little-endian words by pointer.
            Ty::Array {
                len: 1..,
                family: Some(_),
                ..
            } => None,
            Ty::Array {
                len, family: None, ..
            } if len > 0 => match ty {
                Type::Indexed { base, .. } => self.c_aggregate_problem(base, depth + 1),
                _ => Some("an array needs a declared length".to_string()),
            },
            Ty::Named(id) => {
                let key = self.definition_key(id)?;
                if self
                    .structs
                    .get(&key)
                    .is_some_and(|(base, _)| base.is_some())
                {
                    return Some(format!("`{key}` derives from another type"));
                }
                let Some(fields) = self.struct_field_types.get(&key) else {
                    return Some(format!("`{key}` is not a struct of C scalars"));
                };
                if fields.is_empty() {
                    return Some(format!("`{key}` has no fields"));
                }
                let types = self.field_decl_types.get(&key)?;
                fields.iter().find_map(|(name, _, _)| {
                    let field = types.get(name)?;
                    self.c_aggregate_problem(field, depth + 1)
                        .map(|problem| format!("field `{name}`: {problem}"))
                })
            }
            Ty::Char => Some("Char has no declared C character ABI".to_string()),
            other => Some(format!("`{}` has no C layout", self.ty_display(&other))),
        }
    }

    /// Check that an `extern "C"` parameter or result has a representable ABI
    /// type: `real` maps to `double`, integer-shaped types to 64-bit words.
    pub(super) fn check_extern_c_type(&mut self, function: &FnDecl, ty: &Type, position: &str) {
        let checked = self.ast_ty(ty);
        // `std::ffi::Handle` is the C pointer itself.
        if self.is_c_handle(&checked) {
            return;
        }
        // A struct or fixed array of C scalars crosses by pointer, laid out
        // as C lays it out; the call is then testbench-only, since hardware
        // holds an aggregate as separate leaves.
        if checked.is_c_aggregate() {
            match self.c_aggregate_problem(ty, 0) {
                // A parameter crosses by pointer; a result is returned by the
                // platform's struct-return rules.
                None => {
                    if let Some(id) = self.resolved.declared(function.name.span) {
                        self.aggregate_extern_fns.insert(id);
                    }
                    return;
                }
                Some(problem) => {
                    self.error(
                        codes::TYPE_MISMATCH,
                        type_head_span(ty).unwrap_or(function.name.span),
                        format!(
                            "unsupported extern C {position} `{}` in `{}`: {problem}",
                            crate::syntax::pretty::type_str(ty),
                            function.name.text
                        ),
                    );
                    return;
                }
            }
        }
        let supported = matches!(checked, Ty::Integer | Ty::Natural | Ty::Real)
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
            Ty::Array { .. } | Ty::Named(_) | Ty::Tuple(_) => {
                "aggregate and nominal values have no C layout mapping".to_string()
            }
            Ty::Char => "Char has no declared C character ABI".to_string(),
            Ty::Void => "void is not a value ABI type".to_string(),
            Ty::Integer | Ty::Natural | Ty::Real | Ty::Error => unreachable!(),
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
    /// A trait is implemented for a named type, a vector family
    /// (`unsigned`, even written `unsigned[8]`), or every array (`impl<T> ..
    /// for T[]`). A tuple or one concrete array (`(unsigned[8], unsigned[8])`,
    /// `integer[2]`) has no key to hold the impl: the tuple's registered
    /// nowhere and the array's as one for all of `integer`, so its operator
    /// was rejected at the use (#150, #162). Say so at the impl.
    fn check_trait_impl_target(&mut self, implementation: &ImplDecl) {
        let target = &implementation.target;
        let supported = match target {
            Type::Tuple { .. } => false,
            // `impl Into<unsigned> for Logic[]`: an implicit conversion is
            // filed under `Logic[]`, apart from `Logic` (conversions proposal).
            Type::Indexed { index: None, .. }
                if implementation
                    .trait_
                    .as_ref()
                    .and_then(|path| self.trait_key(path))
                    .as_deref()
                    == Some("Into") =>
            {
                true
            }
            Type::Indexed { base, .. } => {
                let generic = type_head_name(base).is_some_and(|name| {
                    implementation
                        .params
                        .params
                        .iter()
                        .any(|param| param.name.text == name)
                });
                generic
                    || self
                        .type_key(base)
                        .is_some_and(|key| self.array_families.contains(&key))
            }
            _ => true,
        };
        if supported {
            return;
        }
        let written = crate::syntax::pretty::type_str(target);
        self.error_with_help(
            codes::TYPE_MISMATCH,
            type_span(target),
            format!("a trait cannot be implemented for `{written}`"),
            format!(
                "implement it for a named type: wrap the value in a struct \
                 (`struct Pair {{ pub v: {written} }}`) and implement the trait for that"
            ),
        );
    }

    /// A struct with fields is stored field by field, and parallel drivers
    /// resolve each field by its own type: `impl Resolve for Packet` was
    /// never called, and an `integer` field still conflicted (#167).
    // ponytail: rejected rather than folded across the leaves; that needs
    // the struct's `resolve` called on the reassembled drivers.
    fn check_struct_resolve(&mut self, implementation: &ImplDecl) {
        let is_resolve = implementation
            .trait_
            .as_ref()
            .and_then(|path| path.segments.last())
            .is_some_and(|segment| segment.text == "Resolve");
        let Some(key) = self.type_key(&implementation.target) else {
            return;
        };
        let record = self
            .structs
            .get(&key)
            .is_some_and(|(base, fields)| base.is_none() && !fields.is_empty());
        if !is_resolve || !record {
            return;
        }
        self.error_with_help(
            codes::TYPE_MISMATCH,
            type_span(&implementation.target),
            format!(
                "`Resolve` cannot be implemented for `{}`, a struct with fields",
                crate::syntax::pretty::type_str(&implementation.target)
            ),
            "a struct is stored field by field and parallel drivers resolve each field by \
             its own type: give the field types `impl Resolve` (as `Logic` has)"
                .to_string(),
        );
    }

    /// A signal, port or local holds bits: a struct with no fields has none,
    /// and storing one reached lowering with no width (#210). Such a struct
    /// is still a type (`Unicode::code(c)`, a `Formatter` parameter).
    pub(super) fn check_storable(&mut self, ty: &Type) {
        match ty {
            Type::Indexed { base, .. } => self.check_storable(base),
            Type::Path(_) | Type::Generic { .. } => {
                let Some(key) = self.type_key(ty) else { return };
                if self
                    .structs
                    .get(&key)
                    .is_some_and(|(base, fields)| base.is_none() && fields.is_empty())
                {
                    let name = key.rsplit("::").next().unwrap_or(&key).to_string();
                    self.error_with_help(
                        codes::TYPE_MISMATCH,
                        crate::syntax::ast::type_span(ty),
                        format!("`{name}` has no fields, so it holds no bits to store"),
                        format!("give `{name}` a field, or use it only as a type (`{name}::f()`)"),
                    );
                }
            }
            _ => {}
        }
    }

    /// Whether a field is an array with no length (`unsigned[8][]`). A
    /// string (`Char[]`, an array of them) is a testbench runtime value.
    fn unsized_field(ty: &Type) -> bool {
        matches!(ty, Type::Indexed { index: None, base, .. }
            if !matches!(base.as_ref(), Type::Path(path)
                if path.segments.last().is_some_and(|s| s.text == "Char")))
    }

    pub(super) fn check_type_layout(&mut self, ty: &Type) {
        match ty {
            Type::Path(_) => {}
            Type::Indexed { base, index, .. } => {
                self.check_type_layout(base);
                let Some(index) = index.as_deref() else {
                    return;
                };
                match index {
                    // A named constant or constant expression is checked as
                    // its value, as a literal is (#453).
                    Expr::Range { lo, hi, .. } => {
                        let (Some(left), Some(right)) = (self.const_int(lo), self.const_int(hi))
                        else {
                            return;
                        };
                        let length = (left - right).unsigned_abs() + 1;
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
                        let Some(width) = self.const_int(index) else {
                            self.check_type_index_expr(index);
                            // A literal past every machine word is past the
                            // layout maximum too; it used to pass silently
                            // (#149).
                            if let Expr::Int { text, .. } = index {
                                if !text.contains('.') {
                                    self.error(
                                        codes::TYPE_MISMATCH,
                                        expr_span(index),
                                        format!(
                                            "type width {text} exceeds the compiler layout \
                                             maximum of {}",
                                            u32::MAX
                                        ),
                                    );
                                }
                            }
                            return;
                        };
                        if width < 0 {
                            self.error(
                                codes::TYPE_MISMATCH,
                                expr_span(index),
                                "a type width cannot be negative".to_string(),
                            );
                        } else if width == 0 {
                            // An empty vector has no storage to simulate, and
                            // a zero length elsewhere means "unsized" (#146).
                            self.error(
                                codes::TYPE_MISMATCH,
                                expr_span(index),
                                "a type width must be at least 1".to_string(),
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
            Type::Generic { base, args, span } => {
                self.check_type_layout(base);
                for argument in args {
                    match argument {
                        GenericArg::PositionalType(ty) | GenericArg::NamedType { ty, .. } => {
                            self.check_type_layout(ty);
                        }
                        GenericArg::Positional(value) | GenericArg::Named { value, .. } => {
                            if self.const_int(value).is_none() {
                                self.check_type_index_expr(value);
                            }
                        }
                    }
                }
                self.check_instantiated_layout(base, args, *span);
            }
            Type::View { target, .. } => self.check_type_layout(target),
            Type::Tuple { elems, .. } => {
                for elem in elems {
                    self.check_type_layout(elem);
                }
            }
        }
    }

    /// A width or argument that is not a constant still has to name values
    /// that exist and not divide by zero: `unsigned[nonexistent]` and
    /// `unsigned[10 / 0]` passed and broke lowering (#526).
    fn check_type_index_expr(&mut self, index: &Expr) {
        let mut problems: Vec<(Span, String, &'static str)> = Vec::new();
        let mut visit = |e: &Expr, problems: &mut Vec<(Span, String, &'static str)>| match e {
            Expr::Path(path) if path.segments.len() == 1 => {
                if self.resolved.resolved(path.span).is_none() {
                    problems.push((
                        path.span,
                        format!(
                            "cannot find value `{}` in this scope",
                            path.segments[0].text
                        ),
                        codes::UNKNOWN_NAME,
                    ));
                }
            }
            Expr::Binary {
                op: BinOp::Div | BinOp::Rem,
                rhs,
                span,
                ..
            } if self.const_int(rhs) == Some(0) => {
                problems.push((
                    *span,
                    "division by a constant zero".to_string(),
                    codes::TYPE_MISMATCH,
                ));
            }
            _ => {}
        };
        fn walk(
            e: &Expr,
            visit: &mut impl FnMut(&Expr, &mut Vec<(Span, String, &'static str)>),
            problems: &mut Vec<(Span, String, &'static str)>,
        ) {
            visit(e, problems);
            match e {
                Expr::Binary { lhs, rhs, .. } => {
                    walk(lhs, visit, problems);
                    walk(rhs, visit, problems);
                }
                Expr::Unary { rhs, .. } => walk(rhs, visit, problems),
                Expr::Range { lo, hi, .. } => {
                    walk(lo, visit, problems);
                    walk(hi, visit, problems);
                }
                Expr::Call { args, .. } => {
                    for argument in args {
                        walk(argument, visit, problems);
                    }
                }
                _ => {}
            }
        }
        walk(index, &mut visit, &mut problems);
        for (span, message, code) in problems {
            self.error(code, span, message);
        }
    }

    /// `MyVec<0>` for `struct MyVec<N: integer>(unsigned[N])`: the struct's
    /// layout with the value arguments substituted is checked as written
    /// types are, reported at the instantiation; a zero or negative width
    /// passed and crashed lowering (#533).
    fn check_instantiated_layout(&mut self, base: &Type, args: &[GenericArg], span: Span) {
        let Some(key) = self.type_key(base) else {
            return;
        };
        let Some(params) = self.struct_params.get(&key).cloned() else {
            return;
        };
        let mut bindings = HashMap::new();
        for (position, argument) in args.iter().enumerate() {
            if let GenericArg::Positional(value) = argument {
                if self.const_int(value).is_none() {
                    continue;
                }
                if let Some(name) = params.get(position) {
                    bindings.insert(name.clone(), argument.clone());
                }
            }
        }
        if bindings.is_empty() {
            return;
        }
        let mut layouts: Vec<Type> = self
            .structs
            .get(&key)
            .and_then(|(base, _)| base.clone())
            .into_iter()
            .collect();
        if let Some(fields) = self.field_decl_types.get(&key) {
            layouts.extend(fields.values().cloned());
        }
        let start = self.sink.diagnostics().len();
        for mut layout in layouts {
            crate::syntax::imports::substitute_type(&mut layout, &bindings);
            self.check_type_layout(&layout);
        }
        self.sink.relocate_from(start, span);
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
            self.reject_entity_return(function, ret);
        }
    }

    /// A function cannot return an entity: an instance is hardware, built
    /// once where it is declared, and a call is evaluated wherever it is
    /// made, so it would have to bring an instance into being (owner rule).
    fn reject_entity_return(&mut self, function: &FnDecl, ret: &Type) {
        let Some(head) = self.type_key(ret) else {
            return;
        };
        if !self.entities.contains_key(&head)
            || self.type_params.borrow().contains(self.key_leaf(&head))
        {
            return;
        }
        self.error_with_help(
            codes::INSTANCE_PLACEMENT,
            crate::syntax::ast::type_span(ret),
            format!("`{}` cannot return an entity", function.name.text),
            "an instance is hardware, built once where it is declared: instantiate it at the \
             top of an entity body and wire it through ports, or return the values it \
             would compute"
                .to_string(),
        );
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
                    // `struct Box<T>(T)` derives from a type it does not know:
                    // no representation to share, `?` widths and a test that
                    // did not lower (#220). A value parameter sizing a known
                    // base (`float<W, M>(Logic[..])`) is fine.
                    let type_params: Vec<&str> = structure
                        .params
                        .params
                        .iter()
                        .filter(|p| {
                            p.bound
                                .as_ref()
                                .is_none_or(|b| type_head_name(b).is_none_or(|h| h != "integer"))
                        })
                        .map(|p| p.name.text.as_str())
                        .collect();
                    if crate::syntax::pretty::type_str(base)
                        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                        .any(|word| type_params.contains(&word))
                    {
                        self.error_with_help(
                            codes::TYPE_MISMATCH,
                            structure.name.span,
                            format!(
                                "newtype `{}` derives from its own type parameter",
                                structure.name.text
                            ),
                            format!(
                                "a newtype shares a known type's representation; hold the \
                                 value in a field instead: `struct {}<..> {{ pub value: {} }}`",
                                structure.name.text,
                                crate::syntax::pretty::type_str(base)
                            ),
                        );
                    }
                    // A newtype over an array is a packed vector family: one
                    // index over single-bit (enum) elements, or over another
                    // family (`struct Byte(unsigned[8])`). Over `Point[]` or
                    // `unsigned[8][4]` its elements read as single bits: a
                    // 4-bit `PointList[4]` (#206, #176).
                    if let (true, Type::Indexed { base: inner, .. }) =
                        (structure.fields.is_empty(), base)
                    {
                        let generic = |name: &str| {
                            structure.params.params.iter().any(|p| p.name.text == name)
                        };
                        let packed = match inner.as_ref() {
                            Type::Path(path) => match self.path_key(path) {
                                Some(key) => {
                                    self.enum_variants.contains_key(&key)
                                        || self.array_families.contains(&key)
                                        || path.segments.len() == 1
                                            && generic(&path.segments[0].text)
                                }
                                None => true,
                            },
                            _ => false,
                        };
                        if !packed {
                            self.error_with_help(
                                codes::TYPE_MISMATCH,
                                structure.name.span,
                                format!(
                                    "`{}` wraps `{}`, but a newtype over an array is a packed \
                                     vector, so its elements must be an enum such as `Logic`",
                                    structure.name.text,
                                    crate::syntax::pretty::type_str(base)
                                ),
                                format!(
                                    "use an element such as `Logic` or `Bit`, or hold the \
                                     array in a field: `struct {} {{ pub items: {} }}`",
                                    structure.name.text,
                                    crate::syntax::pretty::type_str(base)
                                ),
                            );
                        }
                    }
                }
                for field in &structure.fields {
                    self.check_type_layout(&field.ty);
                    // `data: unsigned[8][]` sizes no field: the struct had
                    // no layout, and its signals were dropped (#212).
                    if Self::unsized_field(&field.ty) {
                        self.error_with_help(
                            codes::TYPE_MISMATCH,
                            crate::syntax::ast::type_span(&field.ty),
                            format!(
                                "field `{}` has no fixed size: `{}`",
                                field.name.text,
                                crate::syntax::pretty::type_str(&field.ty)
                            ),
                            "give the array a length, or make the struct generic over it \
                             (`struct S<N: integer> { pub data: T[N] }`)"
                                .to_string(),
                        );
                    }
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
                    self.check_storable(&port.ty);
                    self.report_bare_generic(&port.ty);
                }
            }
            Item::Impl(implementation) => {
                self.check_param_type_layouts(&implementation.params);
                self.check_type_layout(&implementation.target);
                if implementation.trait_.is_some() {
                    self.check_trait_impl_target(implementation);
                    self.check_struct_resolve(implementation);
                }
                for item in &implementation.items {
                    match item {
                        ImplItem::Const(constant) => self.check_type_layout(&constant.ty),
                        ImplItem::Let(declaration) => {
                            if let Some(ty) = &declaration.ty {
                                self.check_type_layout(ty);
                                self.check_storable(ty);
                            }
                        }
                        ImplItem::Fn(function) => self.check_fn_type_layouts(function),
                        ImplItem::ModeField { .. }
                        | ImplItem::Process(_)
                        | ImplItem::Stmt(_)
                        | ImplItem::AttrBinding(_)
                        | ImplItem::Attr(_)
                        | ImplItem::Block(_) => {}
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
            Item::AttrBinding(_) | Item::Macro(_) | Item::MacroCall { .. } => {}
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

impl Checker<'_> {
    /// A trait's default body checked against each type that inherits it:
    /// generic, `self` types as nothing, so `self.id` on a type without an
    /// `id` field passed and crashed lowering (#574). A generic trait or
    /// impl is left to its generic check.
    /// A `Display::fmt` body is expanded into the caller's format at compile
    /// time, so it may only bind `let`s and `write!`: an `if`, a loop or an
    /// assignment in it built a test that aborted when it printed ("direct
    /// Process IR lowering is incomplete", #653). Report it here instead.
    fn check_display_fmt(&mut self, im: &ImplDecl) {
        let display = im
            .trait_
            .as_ref()
            .and_then(|tr| self.trait_key(tr))
            .is_some_and(|key| key == "Display" || key.ends_with("::Display"));
        if !display {
            return;
        }
        for item in &im.items {
            let ImplItem::Fn(function) = item else {
                continue;
            };
            let Some(body) = &function.body else {
                continue;
            };
            if function.name.text != "fmt" {
                continue;
            }
            let allowed = |statement: &&Stmt| match statement {
                Stmt::Let(_) => true,
                Stmt::Expr(Expr::Call {
                    bang: true, callee, ..
                }) => matches!(callee.as_ref(), Expr::Path(path)
                    if path.segments.last().is_some_and(|s| s.text == "write")),
                _ => false,
            };
            let unsupported = body.stmts.iter().find(|statement| !allowed(statement));
            if let Some(statement) = unsupported {
                self.error_with_help(
                    codes::UNSUPPORTED_EXPR,
                    crate::syntax::ast::stmt_span(statement),
                    "a `Display::fmt` body is formatted at compile time and may only \
                     hold `let`s and `write!` calls"
                        .to_string(),
                    "branches, loops and assignments in `fmt` are not supported yet; \
                     print the varying part from the caller"
                        .to_string(),
                );
            }
        }
    }

    fn check_inherited_defaults(&mut self, im: &ImplDecl) {
        if !im.params.params.is_empty() {
            return;
        }
        let Some(key) = im.trait_.as_ref().and_then(|tr| self.trait_key(tr)) else {
            return;
        };
        let Some(defaults) = self.trait_default_bodies.get(&key).cloned() else {
            return;
        };
        let own: HashSet<&str> = im
            .items
            .iter()
            .filter_map(|item| match item {
                ImplItem::Fn(f) => Some(f.name.text.as_str()),
                _ => None,
            })
            .collect();
        for f in &defaults {
            let Some(body) = &f.body else { continue };
            if body.stmts.is_empty() || own.contains(f.name.text.as_str()) {
                continue;
            }
            let saved = self.push_type_params(f.generics.params.iter().map(|p| &p.name.text));
            self.check_function_block(f, body, Some(&im.target), true);
            *self.type_params.borrow_mut() = saved;
        }
    }
}

impl Checker<'_> {
    /// `pub attr a: integer = self'a + 1;`, or `a` and `b` naming each
    /// other: a type attribute defined through itself has no value. Its
    /// expansion stopped at a depth limit and lowering met the bare
    /// attribute (#578).
    pub(super) fn check_type_attr_cycles(&mut self) {
        let reads = |declaration: &AttrDecl| -> Vec<String> {
            let Some(value) = &declaration.default else {
                return Vec::new();
            };
            let mut value = value.clone();
            let mut out = Vec::new();
            crate::syntax::walk::walk_expr(&mut value, &mut |e| {
                if let Expr::SysAttr { base, attr, .. } = e {
                    if matches!(base.as_ref(), Expr::Path(p) if p.segments.len() == 1 && p.segments[0].text == "self")
                    {
                        out.push(attr.text.clone());
                    }
                }
                true
            });
            out
        };
        let mut cyclic: Vec<(Span, String)> = Vec::new();
        for ((owner, name), declaration) in &self.type_attrs {
            let mut stack = reads(declaration);
            let mut seen = HashSet::new();
            while let Some(next) = stack.pop() {
                if next == *name {
                    cyclic.push((declaration.name.span, name.clone()));
                    break;
                }
                if !seen.insert(next.clone()) {
                    continue;
                }
                if let Some(read) = self.type_attrs.get(&(owner.clone(), next)) {
                    stack.extend(reads(read));
                }
            }
        }
        cyclic.sort_by_key(|(span, _)| span.start);
        for (span, name) in cyclic {
            self.error_with_help(
                codes::TYPE_MISMATCH,
                span,
                format!("type attribute `{name}` is defined in terms of itself"),
                "break the cycle: a type attribute cannot depend on itself".to_string(),
            );
        }
    }
}
