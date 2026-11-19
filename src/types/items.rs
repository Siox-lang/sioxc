//! Item-level checks: extern signatures, struct cycles and layouts, views,
//! and attribute targets.

use super::*;

impl<'a> Checker<'a> {
    /// Type-check one top-level item.
    pub(super) fn check_item(&mut self, item: &Item) {
        self.check_item_shapes(item);
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
                self.enum_values
                    .check_declaration(e, self.resolved, self.sink, None);
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
                        self.check_attr_target(
                            a,
                            "port",
                            crate::resolve::attributes::type_identity(&port.ty, self.resolved),
                        );
                        self.check_attr_value(a);
                    }
                }
                for a in &e.attrs {
                    self.check_attr_target(a, "entity", self.resolved.declared(e.name.span));
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
                        let saved = self.push_generic_scope(&f.generics);
                        // The std operator/literal hook traits use an empty
                        // body as an intrinsic placeholder. A real default
                        // body, however, is ordinary inlined code and must
                        // return on every path like any other function.
                        self.check_function_block(f, b, None, !b.stmts.is_empty());
                        self.in_generic_scope.set(saved);
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
                // (operators on the opaque `T`) would wrongly reject it.
                if f.generics.params.is_empty() {
                    if let Some(b) = &f.body {
                        self.check_function_block(f, b, None, true);
                    }
                } else if let Some(body) = &f.body {
                    for statement in &body.stmts {
                        if let Stmt::Let(declaration) = statement {
                            if declaration.is_static {
                                self.reject_generic_static(declaration);
                            }
                        }
                    }
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
        let ty = self.declarations.normalize_type(ty)?;
        let ty = ty.as_ref();
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
                let Some(declaration) = self.declarations.structure(id) else {
                    return Some(format!("`{key}` is not a struct of C scalars"));
                };
                if declaration.base.is_some() {
                    return Some(format!("`{key}` derives from another type"));
                }
                if declaration.fields.is_empty() {
                    return Some(format!("`{key}` has no fields"));
                }
                declaration.fields.iter().find_map(|field| {
                    self.c_aggregate_problem(&field.ty, depth + 1)
                        .map(|problem| format!("field `{}`: {problem}", field.name.text))
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
        let record = self
            .struct_type_id(&implementation.target)
            .and_then(|owner| self.declarations.structure(owner))
            .is_some_and(|structure| structure.base.is_none() && !structure.fields.is_empty());
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
                let Some(structure) = self
                    .struct_type_id(ty)
                    .and_then(|owner| self.declarations.structure(owner))
                else {
                    return;
                };
                if structure.base.is_none() && structure.fields.is_empty() {
                    let name = &structure.name.text;
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

    /// A function returns values, not structural hardware instances.
    fn reject_entity_return(&mut self, function: &FnDecl) {
        let Some(ret) = &function.ret else { return };
        if self.declarations.entity_type_id(ret).is_none() {
            return;
        }
        self.error_with_help(
            codes::INSTANCE_PLACEMENT,
            type_span(ret),
            format!("`{}` cannot return an entity", function.name.text),
            "an instance is hardware, built once where it is declared: instantiate it at the \
             top of an entity body and wire it through ports, or return the values it \
             would compute"
                .to_string(),
        );
    }

    /// Representation checks beyond the shared source-layout pass.
    pub(super) fn check_item_shapes(&mut self, item: &Item) {
        match item {
            Item::Fn(function) => self.reject_entity_return(function),
            Item::Trait(declaration) => {
                for function in &declaration.items {
                    self.reject_entity_return(function);
                }
            }
            Item::ExternBlock { fns, .. } => {
                for function in fns {
                    self.reject_entity_return(function);
                }
            }
            Item::Struct(structure) => {
                if let Some(base) = &structure.base {
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
                                    self.is_enum_ty(&self.path_ty(path))
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
            Item::Entity(entity) => {
                for port in &entity.ports {
                    self.check_storable(&port.ty);
                }
            }
            Item::Impl(implementation) => {
                if implementation.trait_.is_some() {
                    self.check_trait_impl_target(implementation);
                    self.check_struct_resolve(implementation);
                }
                for item in &implementation.items {
                    if let ImplItem::Fn(function) = item {
                        self.reject_entity_return(function);
                    }
                    if let ImplItem::Let(declaration) = item {
                        if let Some(ty) = &declaration.ty {
                            self.check_storable(ty);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Check a `view`: every field it names must exist on the target struct.
    pub(super) fn check_view(&mut self, view: &ViewDecl) {
        let target_ty = &view.target;
        let Some(target) = self.type_key(target_ty) else {
            return;
        };
        let target_name = self.key_leaf(&target).to_string();
        let Some(owner) = self.struct_type_id(target_ty) else {
            self.error(
                codes::TYPE_MISMATCH,
                type_head_span(target_ty).unwrap_or(view.span),
                format!(
                    "view `{}` must target a struct, found `{target_name}`",
                    view.name.text,
                ),
            );
            return;
        };
        let fields = self.base_struct_fields_of(owner);
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
            } else if let Some(visibility) = self.field_visibility_for(owner, &f.name.text) {
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
    pub(super) fn check_attr_target(&mut self, a: &Attr, kind: &str, type_id: Option<DefId>) {
        let name = a
            .name
            .segments
            .last()
            .map(|s| s.text.as_str())
            .unwrap_or("");
        let verdict = self
            .attr_identity(a)
            .and_then(|id| self.attr_targets.get(&id))
            .map(|targets| {
                let ok = targets.iter().any(|t| {
                    t.text == kind
                        || (!crate::resolve::attributes::is_target_kind(&t.text)
                            && type_id.is_some()
                            && self
                                .resolved
                                .resolved(t.span)
                                .map(|id| self.resolved.entity_id(id).unwrap_or(id))
                                == type_id)
                });
                (
                    ok,
                    targets
                        .iter()
                        .map(|t| t.text.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                )
            });
        if let Some((false, allowed)) = verdict {
            self.error(
                codes::INVALID_ATTR_TARGET,
                a.name.span,
                format!("attribute `{name}` cannot be applied to this {kind} (allowed: {allowed})"),
            );
        }
        self.warn_unimplemented_attr(a);
    }

    /// Attributes `std::attrs` declares and the compiler resolves, but which
    /// no stage reads. Writing one has no effect, and nothing else says so —
    /// `#[name = "foo"]` looks like it renames the emitted entity and does
    /// nothing at all.
    pub(super) fn warn_unimplemented_attr(&mut self, attr: &Attr) {
        let Some(id) = self.attr_identity(attr) else {
            return;
        };
        let purpose = match self.resolved.lang_of(id) {
            Some("metadata_keep") => "preserving a signal through optimization",
            Some("metadata_library") => "the emitted library name",
            Some("metadata_name") => "the emitted entity name",
            _ => return,
        };
        let name = self.resolved.def(id).map_or("", |d| d.name.as_str());
        let message = format!("attribute `{name}` has no effect yet");
        self.warn(
            codes::UNIMPLEMENTED_ATTR,
            attr.name.span,
            message,
            &format!("it is reserved for {purpose}; nothing reads it today"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::FileId;

    #[test]
    fn c_aggregate_contracts_borrow_declarations_without_legacy_struct_maps() {
        let mut sources = [
            "module allowed;
             pub struct Cell { pub word: integer }
             pub type CellAlias = Cell;
             pub type Samples = real[2];
             pub type SamplesAlias = Samples;
             pub struct Packet { pub first: CellAlias, pub samples: SamplesAlias }",
            "module rejected;
             pub struct Packet { pub byte: Char }
             pub struct Empty {}
             pub struct Derived(integer);
             pub enum Tag { A, B }",
            "module client;
             type Good = allowed::Packet;
             type Bad = rejected::Packet;
             extern \"C\" {
                 fn good(value: Good);
                 fn samples(value: allowed::SamplesAlias);
                 fn bad(value: Bad);
                 fn empty(value: rejected::Empty);
                 fn derived(value: rejected::Derived);
                 fn tag(value: rejected::Tag);
             }",
        ];
        for _ in 0..2 {
            let mut sink = DiagnosticSink::default();
            let modules = sources
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    crate::syntax::parse_module(FileId(index as u32), source, &mut sink)
                })
                .collect::<Vec<_>>();
            let resolved = crate::resolve::resolve(&modules, &mut sink);
            assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
            let mut checker = Checker::new(&mut sink, &resolved, &modules);
            checker.collect(&modules);
            let mut checked = 0;
            for function in modules
                .iter()
                .flat_map(|module| &module.items)
                .filter_map(|item| match item {
                    Item::ExternBlock { fns, .. } => Some(fns),
                    _ => None,
                })
                .flatten()
            {
                let problem =
                    checker.c_aggregate_problem(function.params[0].ty.as_ref().unwrap(), 0);
                let expected = match function.name.text.as_str() {
                    "good" | "samples" => None,
                    "bad" => Some("field `byte`: Char has no declared C character ABI"),
                    "empty" => Some("`rejected::Empty` has no fields"),
                    "derived" => Some("`rejected::Derived` derives from another type"),
                    "tag" => Some("`rejected::Tag` is not a struct of C scalars"),
                    name => panic!("unexpected extern {name}"),
                };
                assert_eq!(problem.as_deref(), expected, "{}", function.name.text);
                checked += 1;
            }
            assert_eq!(checked, 6);
            sources.reverse();
        }
    }
}
