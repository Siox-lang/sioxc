//! Constructor, conversion, function, and method call lowering.

use super::*;

impl<'a> Lowering<'a> {
    /// `T(x)` on a named type: dispatch to `impl From<Source> for T`,
    /// selected by the argument's type (sole impl accepted for an unknown
    /// source). Struct-valued results come back as per-field values.
    pub(super) fn lower_from(
        &self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let target = match callee {
            ast::Expr::Path(p) => self
                .free_fns
                .enum_path_key(p)
                .or_else(|| (p.segments.len() == 1).then(|| p.segments[0].text.clone()))?,
            _ => return None,
        };
        let arg = args.first()?;
        let src = self.operand_type_name(arg);
        let found = self.lower_from_inner(&target, arg, src.as_deref(), env);
        // This is the last conversion strategy tried, so a `None` here is an
        // `Unknown` in the driver. Record it while the target, the source and
        // a span are all still in hand.
        if found.is_none()
            && (self.structs.contains_key(&target) || self.enum_variants.contains_key(&target))
        {
            self.bad_conversions
                .borrow_mut()
                .push((target, src.clone(), ast::expr_span(callee)));
        }
        found
    }

    /// `float<32, 23>(1.5)`: a family's `From<Source>` impl, inlined with
    /// `Self'left`/`'right`/`'high`/`'low`/`'length` describing the format being
    /// built. `None` when the family has no impl for the argument's type, and
    /// the conversion is the kernel's raw resize.
    pub(super) fn lower_family_from(
        &self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        env: &HashMap<String, Val>,
    ) -> Option<Expr> {
        let ast::Expr::Index { base, index, .. } = callee else {
            return None;
        };
        let ast::Expr::Path(path) = base.as_ref() else {
            return None;
        };
        let [arg] = args else {
            return None;
        };
        let target = self
            .free_fns
            .struct_path_key(path)
            .or_else(|| expr_path(base))?;
        let fns = self.op_impls.get("From", &target)?;
        // The operand's own family first (`sfixed<16, 8>` resizing): the
        // checker records this conversion form's argument as a kernel integer.
        let source = match self
            .operand_type_name(arg)
            .filter(|family| family != "integer")
        {
            Some(family) => family,
            None => match self.expr_types.get(&ast::expr_span(arg)) {
                Some(crate::types::Ty::Real) => "real".to_string(),
                _ if self.is_real_expr(&self.lower_scalar_env(arg, env)) => "real".to_string(),
                _ => "integer".to_string(),
            },
        };
        let (f, _) = fns.iter().find(|(f, declared)| {
            declared.clone().or_else(|| {
                f.params
                    .iter()
                    .find(|p| !p.is_self)
                    .and_then(|p| p.ty.as_ref())
                    .and_then(|ty| self.free_fns.type_head_key(ty))
            }) == Some(source.clone())
        })?;
        let body = f.body.as_ref()?;
        let (left, right) = match index.as_ref() {
            ast::Expr::Range { lo, hi, .. } => (
                self.eval_const(lo, &self.cur_env)?,
                self.eval_const(hi, &self.cur_env)?,
            ),
            width => (self.eval_const(width, &self.cur_env)? - 1, 0),
        };
        let mut fenv: HashMap<String, Val> = HashMap::new();
        // Each bound as the source `0 - 4` would lower: a negative one is a
        // signed subtraction, which every width rule downstream reads as a
        // small kernel integer. Its 64-bit pattern instead read as a huge
        // unsigned number, and `1 << (0 - Self'low)` lost its bits.
        let bound = |value: i64| {
            if value < 0 {
                self.source_binary(
                    BinOp::SSub,
                    &Expr::Const(0),
                    &Expr::Const(value.unsigned_abs()),
                    ast::expr_span(callee),
                )
            } else {
                Expr::Const(value as u64)
            }
        };
        for (attr, value) in [
            ("left", left),
            ("right", right),
            ("high", left.max(right)),
            ("low", left.min(right)),
        ] {
            fenv.insert(format!("Self::{attr}"), Val::Scalar(bound(value)));
        }
        fenv.insert(
            "Self::length".to_string(),
            Val::Scalar(Expr::Const(left.abs_diff(right) + 1)),
        );
        let param = f.params.iter().find(|p| !p.is_self)?.name.as_ref()?;
        fenv.insert(
            param.text.clone(),
            self.bind_source_value(self.lower_val_env(arg, env), ast::expr_span(arg), None),
        );
        // A resize reads the source format too: `value'low`, and its width
        // travels to the calls in the body (`word(value)`).
        self.bind_range_attrs(&mut fenv, &param.text, arg, env);
        let width = self.ast_width(arg);
        fenv.insert(
            format!("{}::length", param.text),
            Val::Scalar(Expr::Const(width as u64)),
        );
        let saved_width = self
            .param_widths
            .borrow_mut()
            .insert(param.text.clone(), width);
        let shapes = self.source_shape_scope(
            HashMap::new(),
            f.ret
                .as_ref()
                .map(|ty| self.source_layout(ty, &self.cur_env)),
        );
        let out = self.inline_block(&body.stmts, &fenv);
        drop(shapes);
        match saved_width {
            Some(previous) => self
                .param_widths
                .borrow_mut()
                .insert(param.text.clone(), previous),
            None => self.param_widths.borrow_mut().remove(&param.text),
        };
        // The value is a word of the format, as a stored one is: operator
        // bodies read its sign bit at `'length - 1`.
        let length = u32::try_from(left.abs_diff(right) + 1).ok()?;
        match out? {
            Val::Scalar(value) if length < 64 => {
                Some(self.source_slice(&value, length - 1, 0, ast::expr_span(arg)))
            }
            Val::Scalar(value) => Some(value),
            _ => None,
        }
    }

    /// Inline a `From` conversion's body for the target type.
    pub(super) fn lower_from_inner(
        &self,
        target: &str,
        arg: &ast::Expr,
        src: Option<&str>,
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let src = src.map(str::to_string);
        // No explicit `impl From<src> for target`: try a derivation-total
        // conversion (spec: T(x) is auto for total derivations).
        let Some(fns) = self.op_impls.get("From", target) else {
            return self.derived_conversion(target, src.as_deref(), arg, env);
        };
        let declared = |f: &ast::FnDecl, a: &Option<String>| -> Option<String> {
            a.clone().or_else(|| {
                f.params
                    .iter()
                    .find(|p| !p.is_self)
                    .and_then(|p| p.ty.as_ref())
                    .and_then(|ty| self.free_fns.type_head_key(ty))
            })
        };
        let chosen = match &src {
            Some(sty) => fns
                .iter()
                .find(|(f, a)| declared(f, a).as_deref() == Some(sty)),
            None => (fns.len() == 1).then(|| &fns[0]),
        };
        let (f, _) = match chosen {
            Some(c) => c,
            None => return self.derived_conversion(target, src.as_deref(), arg, env),
        };
        let body = f.body.as_ref()?;
        let mut fenv: HashMap<String, Val> = HashMap::new();
        if let Some(p) = f.params.iter().find(|p| !p.is_self) {
            if let Some(n) = &p.name {
                self.bind_range_attrs(&mut fenv, &n.text, arg, env);
                fenv.insert(
                    n.text.clone(),
                    self.bind_source_value(self.lower_val_env(arg, env), ast::expr_span(arg), None),
                );
                fenv.insert(
                    format!("{}::length", n.text),
                    Val::Scalar(Expr::Const(self.ast_width(arg) as u64)),
                );
            }
        }
        let _shapes = self.source_shape_scope(
            HashMap::new(),
            f.ret
                .as_ref()
                .map(|ty| self.source_layout(ty, &self.cur_env)),
        );
        self.inline_block(&body.stmts, &fenv)
    }

    /// `T()` / `T[N]()` — the nullary constructor: the structural `new()`
    /// default of a named type, the explicit spelling of the value an
    /// uninitialized signal already powers on to (§3.29), and the zero-argument
    /// member of the same `T(...)` family whose one-argument form `T(x)` is the
    /// conversion of §3.28. An enum yields its first variant (`T'LEFT`); a
    /// numeric / vector / `Char` / `real` / kernel `integer` yields `0`; a
    /// struct yields its fields defaulted the same way. The `impl New for T`
    /// *override* waits on trait resolution; this is the derived default only.
    /// (An array `T[N]()` of a composite defaults through its per-element signal
    /// inits, not an expression value.)
    pub(super) fn lower_new(&self, callee: &ast::Expr, args: &[ast::Expr]) -> Option<Val> {
        if !args.is_empty() {
            return None;
        }
        // The head type name — a bare `T` or the family of a sized `T[N]` (the
        // width is irrelevant to a zero default).
        let name = match callee {
            ast::Expr::Path(p) => self
                .free_fns
                .type_owner_key(p)
                .or_else(|| (p.segments.len() == 1).then(|| p.segments[0].text.clone()))?,
            ast::Expr::Index { base, .. } => match base.as_ref() {
                ast::Expr::Path(p) if p.segments.len() == 1 => p.segments[0].text.clone(),
                _ => return None,
            },
            _ => return None,
        };
        if let Some(&d) = self.enum_first_disc.get(&name) {
            return Some(Val::Scalar(Expr::Const(d)));
        }
        if let Some(fields) = self.struct_default_leaves(&name, "") {
            return Some(Val::Fields(fields));
        }
        (self.array_families.contains(&name)
            || matches!(name.as_str(), "integer" | "Char" | "real"))
        .then_some(Val::Scalar(Expr::Const(0)))
    }

    /// A struct's derived default as flattened `(leaf-dotted-name, expr)` pairs
    /// (the shape `Val::Fields` assignment consumes), each field defaulted
    /// structurally and nested structs recursed. `None` for a non-aggregate (a
    /// scalar newtype like `struct unsigned : Logic[]`, which has no fields).
    pub(super) fn struct_default_leaves(
        &self,
        sname: &str,
        prefix: &str,
    ) -> Option<Vec<(String, Expr)>> {
        let fields = self.raw_struct_fields(sname).filter(|f| !f.is_empty())?;
        let mut out = Vec::new();
        for (fname, fty) in fields {
            let path = if prefix.is_empty() {
                fname.clone()
            } else {
                format!("{prefix}.{fname}")
            };
            if let ast::Type::Path(enum_path) = &fty {
                if let Some(enum_key) = self.free_fns.enum_path_key(enum_path) {
                    if let Some(&d) = self.enum_first_disc.get(&enum_key) {
                        out.push((path, Expr::Const(d)));
                        continue;
                    }
                }
            }
            if let Some(h) = self.free_fns.type_head_key(&fty) {
                if let Some(nested) = self.struct_default_leaves(&h, &path) {
                    out.extend(nested);
                    continue;
                }
                if let Some(&d) = self.enum_first_disc.get(&h) {
                    out.push((path, Expr::Const(d)));
                    continue;
                }
            }
            out.push((path, Expr::Const(0)));
        }
        Some(out)
    }

    /// Lower a call to a module-level `fn`: const-fold when every argument
    /// const-evaluates (so `clog2(DEPTH)` is a constant), else inline the
    /// body like an operator impl (params bound positionally, with
    /// `param::length` available). Depth-guarded against runaway recursion.
    /// Inline a module-level function call.
    ///
    /// Returns a [`Val`], not an `Expr`: a function may return a struct, and
    /// discarding the `Val::Fields` the body produced left the call with no
    /// value at all. The assignment it fed was then dropped for want of
    /// fields, so `s = twice(a)` read as zero with only a "never driven"
    /// warning — while a *method* with the identical body worked, because the
    /// method path always kept the `Val`.
    pub(super) fn lower_free_call(
        &self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let display_name = call_fn_key(callee)?;
        let f = self.free_fns.get(callee)?;
        // A bodyless declaration is a foreign C function (`extern "C"`).
        if f.body.is_none() {
            let is_type = |t: &Option<ast::Type>, expected: &str| {
                t.as_ref()
                    .is_some_and(|ty| self.type_resolves_to(ty, expected))
            };
            let f64_args = f
                .params
                .iter()
                .filter(|p| !p.is_self)
                .map(|p| is_type(&p.ty, "real"))
                .collect();
            let integer_args = f
                .params
                .iter()
                .filter(|p| !p.is_self)
                .map(|p| is_type(&p.ty, "integer"))
                .collect();
            let f64_ret = is_type(&f.ret, "real");
            let integer_ret = is_type(&f.ret, "integer");
            let arguments = args
                .iter()
                .map(|a| {
                    let value = self.lower_scalar_env(a, env);
                    self.source_values
                        .borrow_mut()
                        .append(&value, ast::expr_span(a), None)
                })
                .collect();
            let mut arena = self.source_values.borrow_mut();
            let value = arena.push_node(
                ProcessValue {
                    span: ast::expr_span(callee),
                    ty: None,
                    bit_width: None,
                    kind: ProcessValueKind::ForeignCall {
                        name: f.name.text.clone(),
                        arguments,
                        float_arguments: f64_args,
                        integer_arguments: integer_args,
                        float_result: f64_ret,
                        integer_result: integer_ret,
                    },
                },
                None,
            );
            return Some(Val::Scalar(arena.reference(value)));
        }
        // Constant arguments: run the body statically.
        let consts: Option<Vec<i64>> = args
            .iter()
            .map(|a| eval_const_fns(a, &self.cur_env, &self.free_fns, 0))
            .collect();
        if let Some(cs) = consts {
            let mut fenv = self.cur_env.clone();
            for (p, v) in f.params.iter().filter(|p| !p.is_self).zip(cs) {
                if let Some(n) = &p.name {
                    fenv.insert(n.text.clone(), v);
                }
            }
            if let Some(v) = eval_const_stmts(&f.body.as_ref()?.stmts, &fenv, &self.free_fns, 0) {
                return Some(Val::Scalar(Expr::Const(v as u64)));
            }
        }
        // Dynamic arguments: inline against shared source-value handles.
        if self.inline_depth.get() > 16 {
            // Bailing here leaves an `Unknown` in the middle of a driver, so
            // record it — otherwise lowering "succeeds" and the design only
            // fails much later with a generic engine message.
            self.depth_exceeded
                .borrow_mut()
                .push((display_name, ast::expr_span(callee)));
            return None;
        }
        self.inline_depth.set(self.inline_depth.get() + 1);
        let mut fenv: HashMap<String, Val> = HashMap::new();
        let mut shapes = HashMap::new();
        // Saved param-family bindings to restore after this inline (nesting).
        let mut saved: Vec<(String, Option<String>)> = Vec::new();
        let mut saved_widths: Vec<(String, Option<u32>)> = Vec::new();
        // Names this inline added to `param_integers`, removed on the way out
        // so a nested or later inline does not inherit them.
        let mut added_integers: Vec<String> = Vec::new();
        for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(n) = &p.name {
                // An argument is read against the *parameter's* declared type,
                // so a positional literal for a struct parameter is a struct
                // literal. Left as the concatenation it lexes as, the
                // parameter bound no fields and the body's `p.a` reported
                // having no hardware form.
                let a = &self.as_struct_literal(p.ty.as_ref(), a);
                // ...and a character literal for a `Char` parameter is its
                // code point, not the logic-literal placeholder, which would
                // otherwise resolve to `Logic` and read 0 for `'A'`.
                let (argument, layout) = self.lower_source_argument(a, p.ty.as_ref(), env);
                if let Some(layout) = layout {
                    shapes.insert(n.text.clone(), layout);
                }
                let value = match argument {
                    Val::Scalar(v) => Val::Scalar(
                        self.resolve_char_literal(p.ty.as_ref().and_then(type_head_name), v),
                    ),
                    other => other,
                };
                fenv.insert(
                    n.text.clone(),
                    self.bind_source_value(value, ast::expr_span(a), None),
                );
                fenv.insert(
                    format!("{}::length", n.text),
                    Val::Scalar(Expr::Const(self.ast_width(a) as u64)),
                );
                self.bind_range_attrs(&mut fenv, &n.text, a, env);
                // Propagate the argument's family so the body dispatches
                // operators on the caller's concrete type.
                if let Some(fam) = self.operand_type_name(a) {
                    let prev = self.param_types.borrow_mut().insert(n.text.clone(), fam);
                    saved.push((n.text.clone(), prev));
                }
                // A parameter declared `integer` makes the body's operations
                // signed, which its recorded types cannot say — and so does a
                // generic one bound to a kernel integer (`abs(n)` for
                // `fn abs<T>(v: T)`).
                let generic = p.ty.as_ref().and_then(type_head_name).is_some_and(|name| {
                    f.generics
                        .params
                        .iter()
                        .any(|param| param.name.text == name)
                });
                let integer_argument = self.declares_kernel_integer(a)
                    || matches!(
                        self.expr_types.get(&ast::expr_span(a)),
                        Some(crate::types::Ty::Integer)
                    );
                if (p.ty.as_ref().and_then(type_head_name) == Some("integer")
                    || (generic && integer_argument))
                    && self.param_integers.borrow_mut().insert(n.text.clone())
                {
                    added_integers.push(n.text.clone());
                }
                // The width travels with the family: a nested inline (e.g.
                // `signed`'s Ord inside this body) reads `self'length` off the
                // parameter, and without this it saw none.
                let w = self.ast_width(a);
                if w > 0 {
                    saved_widths.push((
                        n.text.clone(),
                        self.param_widths.borrow_mut().insert(n.text.clone(), w),
                    ));
                }
            }
        }
        let return_layout = self.source_function_return_layout(f, &shapes);
        let _shapes = self.source_shape_scope(shapes, return_layout);
        let out = f
            .body
            .as_ref()
            .and_then(|b| self.inline_block(&b.stmts, &fenv));
        // The result is a value of the declared return type, so it wraps to
        // that width. Assigning it to a signal masked it anyway, which hid
        // this — but used in place (`neg(x) < 0`) the extra bits survived and
        // signed's Ord tested the wrong one.
        // Only a scalar result has a declared width to wrap to; a struct's
        // leaves were already masked field by field as the body built them.
        let out = match (out, f.ret.as_ref()) {
            (Some(Val::Scalar(v)), Some(ret)) => Some(Val::Scalar(self.bind_source_expression(
                self.mask_to_type_width(v, ret, ast::expr_span(callee)),
                ast::expr_span(callee),
            ))),
            (v, _) => v,
        };
        for (name, prev) in saved.into_iter().rev() {
            match prev {
                Some(v) => self.param_types.borrow_mut().insert(name, v),
                None => self.param_types.borrow_mut().remove(&name),
            };
        }
        for (name, prev) in saved_widths.into_iter().rev() {
            match prev {
                Some(w) => self.param_widths.borrow_mut().insert(name, w),
                None => self.param_widths.borrow_mut().remove(&name),
            };
        }
        for name in added_integers {
            self.param_integers.borrow_mut().remove(&name);
        }
        self.inline_depth.set(self.inline_depth.get() - 1);
        out
    }

    /// Wrap `v` to the width of declared type `ret`, when that is a bounded
    /// vector. A `real`, a kernel `integer` or an unknown width is left alone.
    pub(super) fn mask_to_type_width(
        &self,
        v: Expr,
        ret: &ast::Type,
        span: crate::diag::Span,
    ) -> Expr {
        if type_head_name(ret).is_some_and(|h| matches!(h, "real" | "integer")) {
            return v;
        }
        let w = type_width(
            ret,
            &self.cur_env,
            &self.free_fns,
            &self.structs,
            &self.const_ranges,
        );
        if w == 0 || w >= 64 {
            return v;
        }
        self.source_binary(BinOp::And, &v, &Expr::Const((1u64 << w) - 1), span)
    }

    /// Find a method `name` on type `ty`: an inherent-impl method
    /// (`impl T { fn name(self, ..) }`) or a trait-impl method
    /// (`impl Tr for T { fn name(self, ..) }`, held in `op_impls` keyed by
    /// trait+type). Inherent impls win; first match otherwise.
    pub(super) fn find_method(
        &self,
        ty: &str,
        name: &str,
        input: Option<&str>,
    ) -> Option<&'a ast::FnDecl> {
        if let Some(impls) = self.inherent_impls.get(ty) {
            for im in impls {
                for it in &im.items {
                    if let ast::ImplItem::Fn(f) = it {
                        if f.name.text == name {
                            return Some(f);
                        }
                    }
                }
            }
        }
        if let Some(f) = self
            .op_impls
            .iter()
            .filter(|(_, owner, _)| *owner == ty)
            .flat_map(|(_, _, fns)| fns.iter())
            .find(|(f, rhs)| {
                // An integer literal argument adopts the owner type, as an
                // operator's right operand does (`x < 0` on `signed`).
                f.name.text == name
                    && input.is_none_or(|input| {
                        rhs.as_deref()
                            .is_none_or(|rhs| rhs == input || (input == "integer" && rhs == ty))
                    })
            })
            .map(|(f, _)| *f)
        {
            return Some(f);
        }
        // Last: a defaulted method the type inherits from a trait it
        // implements. The impl's own methods are found above, so an override
        // always wins; this only supplies what the impl omitted.
        self.implemented_traits
            .get(ty)?
            .iter()
            .filter_map(|tr| self.trait_decls.get(tr.as_str()))
            .flat_map(|t| t.items.iter())
            .find(|f| f.name.text == name && f.body.is_some())
    }

    /// The argument whose type a generic function's result takes: for
    /// `fn rem<T>(a: T, m: T) -> T`, the first argument bound to a `T`.
    pub(super) fn generic_return_argument<'e>(
        &self,
        callee: &ast::Expr,
        args: &'e [ast::Expr],
    ) -> Option<&'e ast::Expr> {
        let function = self.free_fns.get(callee)?;
        let generic = |ty: &ast::Type| match ty {
            ast::Type::Path(path) if path.segments.len() == 1 => {
                let name = &path.segments[0].text;
                (function.generics.params.iter())
                    .any(|param| &param.name.text == name)
                    .then_some(name.clone())
            }
            _ => None,
        };
        let name = generic(function.ret.as_ref()?)?;
        function
            .params
            .iter()
            .filter(|param| !param.is_self)
            .zip(args)
            .find(|(param, _)| {
                param.ty.as_ref().and_then(generic).as_deref() == Some(name.as_str())
            })
            .map(|(_, argument)| argument)
    }

    /// Lower a method call `recv.method(args)` (spec 3.20) by inlining the
    /// impl method's body: `self` binds to the receiver, each named parameter
    /// to its argument (mirroring [`Self::lower_free_call`]), and the receiver
    /// type is stashed under `param_types["self"]` so operators inside the body
    /// dispatch on the concrete type. Value-returning methods (`a.cmp(b)`,
    /// `s.can_send()`) inline to a [`Val`]; a body the inliner cannot express
    /// as a value (a statement method that drives signals) yields `None`.
    pub(super) fn lower_method_call(
        &self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let ast::Expr::Field { base, field, .. } = callee else {
            return None;
        };
        let ty = self.operand_type_name(base)?;
        let input = args.first().and_then(|arg| match arg {
            ast::Expr::Construct { ty: Some(ty), .. } if type_head_name(ty) == Some("Range") => {
                Some("Range".to_string())
            }
            _ => self.operand_type_name(arg),
        });
        let f = self.find_method(&ty, &field.text, input.as_deref())?;
        let body = f.body.as_ref()?;
        if self.inline_depth.get() > 16 {
            self.depth_exceeded
                .borrow_mut()
                .push((format!("{ty}.{}", field.text), ast::expr_span(callee)));
            return None;
        }
        self.inline_depth.set(self.inline_depth.get() + 1);
        self.inlining_methods
            .borrow_mut()
            .push((ty.clone(), field.text.clone()));
        // Bind `self` to the receiver's signal so a `self'event`/`self'old`
        // sysattr in the body (the std `ClockLike` edge methods) resolves to it.
        let saved_self = self.self_signal.replace(self.base_signal(base));
        let mut fenv: HashMap<String, Val> = HashMap::new();
        let mut shapes = HashMap::new();
        if let Some(layout) = self.source_operand_layout(base, env) {
            shapes.insert("self".to_owned(), layout);
        }
        fenv.insert(
            "self".to_string(),
            self.bind_source_value(self.lower_val_env(base, env), ast::expr_span(base), None),
        );
        fenv.insert(
            "self::length".to_string(),
            Val::Scalar(Expr::Const(self.ast_width(base) as u64)),
        );
        self.bind_range_attrs(&mut fenv, "self", base, env);
        // Family bindings to restore after the inline (nesting-safe).
        let mut saved: Vec<(String, Option<String>)> = Vec::new();
        let mut saved_widths: Vec<(String, Option<u32>)> = Vec::new();
        // Names this inline added to `param_integers`, removed on the way out
        // so a nested or later inline does not inherit them.
        let mut added_integers: Vec<String> = Vec::new();
        let self_prev = self
            .param_types
            .borrow_mut()
            .insert("self".to_string(), ty.clone());
        saved.push(("self".to_string(), self_prev));
        let receiver_width = self.ast_width(base);
        for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(n) = &p.name {
                // An argument is read against the *parameter's* declared type,
                // so a positional literal for a struct parameter is a struct
                // literal. Left as the concatenation it lexes as, the
                // parameter bound no fields and the body's `p.a` reported
                // having no hardware form.
                let a = &self.as_struct_literal(p.ty.as_ref(), a);
                // ...and a character literal for a `Char` parameter is its
                // code point, not the logic-literal placeholder, which would
                // otherwise resolve to `Logic` and read 0 for `'A'`.
                let (argument, layout) = self.lower_source_argument(a, p.ty.as_ref(), env);
                if let Some(layout) = layout {
                    shapes.insert(n.text.clone(), layout);
                }
                let value = match argument {
                    Val::Scalar(v) => Val::Scalar(
                        self.resolve_char_literal(p.ty.as_ref().and_then(type_head_name), v),
                    ),
                    other => other,
                };
                fenv.insert(
                    n.text.clone(),
                    self.bind_source_value(value, ast::expr_span(a), None),
                );
                fenv.insert(
                    format!("{}::length", n.text),
                    Val::Scalar(Expr::Const(
                        self.literal_aware_width(a, receiver_width) as u64
                    )),
                );
                self.bind_range_attrs(&mut fenv, &n.text, a, env);
                // An integer literal for a parameter not declared `integer`
                // adopts the receiver's type, as an operator's right operand
                // does: `x >= -4` on `signed` reaches `Ord::ge`'s default
                // `rhs.le(self)` with `rhs` a `signed`.
                if let Some(mut fam) = self.operand_type_name(a) {
                    if fam == "integer" && p.ty.as_ref().and_then(type_head_name) != Some("integer")
                    {
                        fam = ty.clone();
                    }
                    let prev = self.param_types.borrow_mut().insert(n.text.clone(), fam);
                    saved.push((n.text.clone(), prev));
                }
                // A parameter declared `integer` makes the body's operations
                // signed, which its recorded types cannot say.
                if p.ty.as_ref().and_then(type_head_name) == Some("integer")
                    && self.param_integers.borrow_mut().insert(n.text.clone())
                {
                    added_integers.push(n.text.clone());
                }
                // The width travels with the family: a nested inline (e.g.
                // `signed`'s Ord inside this body) reads `self'length` off the
                // parameter, and without this it saw none.
                let w = self.ast_width(a);
                if w > 0 {
                    saved_widths.push((
                        n.text.clone(),
                        self.param_widths.borrow_mut().insert(n.text.clone(), w),
                    ));
                }
            }
        }
        // The receiver's width too, so a body passing `self` on (`rhs.le(self)`
        // in a comparison default) gives the callee a width. Bound after the
        // arguments, which read the caller's `self`.
        if receiver_width > 0 {
            saved_widths.push((
                "self".to_string(),
                self.param_widths
                    .borrow_mut()
                    .insert("self".to_string(), receiver_width),
            ));
        }
        let return_layout = self.source_function_return_layout(f, &shapes);
        let _shapes = self.source_shape_scope(shapes, return_layout);
        let out = self.inline_block(&body.stmts, &fenv);
        for (name, prev) in saved.into_iter().rev() {
            match prev {
                Some(v) => self.param_types.borrow_mut().insert(name, v),
                None => self.param_types.borrow_mut().remove(&name),
            };
        }
        for (name, prev) in saved_widths.into_iter().rev() {
            match prev {
                Some(w) => self.param_widths.borrow_mut().insert(name, w),
                None => self.param_widths.borrow_mut().remove(&name),
            };
        }
        self.self_signal.set(saved_self);
        for name in added_integers {
            self.param_integers.borrow_mut().remove(&name);
        }
        self.inlining_methods.borrow_mut().pop();
        self.inline_depth.set(self.inline_depth.get() - 1);
        out
    }

    /// Borrow a procedure body and bind its receiver/arguments explicitly.
    pub(super) fn lower_method_stmt(
        &mut self,
        recv: &ast::Expr,
        method: &str,
        args: &[ast::Expr],
        cond: Option<Expr>,
    ) -> bool {
        self.lower_source_procedure(Some((recv, method)), recv, args, cond, None)
    }

    /// Borrow a free procedure body without caller-expression substitution.
    pub(super) fn lower_free_stmt(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        cond: Option<Expr>,
    ) -> bool {
        self.lower_source_procedure(None, callee, args, cond, None)
    }

    /// Lower a conversion expression (spec 3.17): `unsigned[16](x)` resizes,
    /// `signed[8](x)` truncates, `integer(x)` crosses to the kernel word, and
    /// `resize(x, n)` is the family-preserving spelling (n const-evaluable —
    /// the language is static, so a value argument in width position is a
    /// generic argument). Semantics on the word IR: an `signed`-family source
    /// sign-extends into the full word first (`v - 2^w` when the sign bit is
    /// set); the target width truncates via a slice; widening to `unsigned`
    /// zero-extends implicitly. `None` when `callee` is not a conversion.
    pub(super) fn lower_conversion(
        &self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        env: &HashMap<String, Val>,
    ) -> Option<Expr> {
        if let Some(value) = self.lower_family_from(callee, args, env) {
            return Some(value);
        }
        // A field-less struct derived from a scalar kernel type is a nominal
        // newtype with the same representation. Its constructor is therefore
        // value-transparent (`time(v)`, `frequency(v)`), just as derivation is;
        // the target signal supplies any required real coercion.
        if let ast::Expr::Path(p) = callee {
            if let Some(target) = self.free_fns.struct_path_key(p) {
                let scalar_newtype = self.structs.get(&target).is_some_and(|s| {
                    s.fields.is_empty()
                        && ["integer", "real", "Char"].iter().any(|kernel| {
                            struct_derives_kernel(&target, kernel, &self.structs, &self.free_fns)
                        })
                });
                if scalar_newtype {
                    return Some(self.lower_scalar_env(args.first()?, env));
                }
                // A field-less struct over a *vector* (`struct Byte(unsigned[8])`)
                // is the same idea at a width: the constructor keeps the value
                // and the type fixes how many bits of it there are. Without
                // this `Byte(200)` matched no conversion shape at all and
                // lowered to `Unknown`.
                if let Some(base) = self
                    .structs
                    .get(&target)
                    .filter(|s| s.fields.is_empty())
                    .and_then(|s| s.base.as_ref())
                {
                    if matches!(base, ast::Type::Indexed { .. }) {
                        let w = type_width(
                            base,
                            &self.cur_env,
                            &self.free_fns,
                            &self.structs,
                            &self.const_ranges,
                        );
                        let v = self.lower_scalar_env(args.first()?, env);
                        return Some(if w > 0 && w < 64 {
                            self.source_slice(&v, w - 1, 0, ast::expr_span(callee))
                        } else {
                            v
                        });
                    }
                }
            }
        }
        // Target: (is_resize, family, width). Width None = kernel integer.
        let head = |e: &ast::Expr| match e {
            ast::Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
            _ => None,
        };
        let (target_w, resize) = match callee {
            // `real(n)` is converted by value below; without this arm it
            // never got there and lowered to `Unknown`.
            ast::Expr::Path(p)
                if p.segments.len() == 1
                    && matches!(p.segments[0].text.as_str(), "integer" | "real") =>
            {
                (None, false)
            }
            // `Char(n)`: a code point becomes a symbol (32-bit storage).
            ast::Expr::Path(p) if p.segments.len() == 1 && p.segments[0].text == "Char" => {
                (Some(32), false)
            }
            ast::Expr::Path(p) if p.segments.len() == 1 && p.segments[0].text == "resize" => {
                let n = args.get(1)?;
                let w = match self.lower_scalar_env(n, env) {
                    Expr::Const(c) => c as u32,
                    _ => self.eval_const(n, &self.cur_env)? as u32,
                };
                (Some(w), true)
            }
            ast::Expr::Index { base, index, .. }
                if head(base)
                    .as_deref()
                    .is_some_and(|h| self.array_families.contains(h)) =>
            {
                let w = match self.lower_scalar_env(index, env) {
                    Expr::Const(c) => c as u32,
                    _ => self.eval_const(index, &self.cur_env)? as u32,
                };
                (Some(w), false)
            }
            _ => return None,
        };
        let _ = resize;
        let arg = args.first()?;
        // Conversions are a raw resize (zero-extend / truncate). Signed
        // widening is the library `std::bits::sext`, not the compiler's job.
        let mut v = self.lower_scalar_env(arg, env);
        // A bare character literal is a `Char` (the checker types it so), so
        // the kernel conversions read its code point: `integer('A')` is 65.
        // Left as the logic-literal placeholder it resolved against `Logic`,
        // and every character outside `Logic` read 0.
        if matches!(callee, ast::Expr::Path(p) if p.segments.len() == 1
            && matches!(p.segments[0].text.as_str(), "integer" | "Char"))
        {
            v = self.resolve_char_literal(Some("Char"), v);
        }
        // ...except crossing out of `real`, which is a value conversion: the
        // operand carries f64 bits, and resizing them keeps a mantissa slice
        // rather than the number.
        let to_real = matches!(callee, ast::Expr::Path(p) if p.segments.len() == 1
            && p.segments[0].text == "real");
        if to_real {
            // ...and so is crossing into it: `real(n)` is the number n.
            if !self.is_real_expr(&v) {
                v = self.source_values.borrow_mut().unary(
                    ProcessUnaryOp::IntegerToReal,
                    &v,
                    ast::expr_span(callee),
                );
            }
            return Some(v);
        }
        if self.is_real_expr(&v) {
            v = self.source_values.borrow_mut().unary(
                ProcessUnaryOp::RealToInteger,
                &v,
                ast::expr_span(callee),
            );
        }
        // `integer(x)` of a packed value is its raw word as a 64-bit kernel
        // integer: the slice makes the backend evaluate it at that width, so
        // the kernel arithmetic built on it does too. Left at the vector's
        // width (16 bits for `sfixed<16, 8>`), a later signed step misread
        // its top bit.
        let packed_operand = target_w.is_none()
            && self
                .operand_type_name(arg)
                .is_some_and(|family| self.array_families.contains(&family));
        Some(match target_w {
            Some(w) if w > 0 && w < 64 => self.source_slice(&v, w - 1, 0, ast::expr_span(callee)),
            None if packed_operand => self.source_slice(&v, 63, 0, ast::expr_span(callee)),
            _ => v,
        })
    }

    /// The type name an operand contributes to operator-impl lookup: a local's
    /// declared enum/struct, a suffix literal's target type, an enum variant's
    /// enum, or `integer` for a bare numeric literal.
    pub(super) fn operand_type_name(&self, e: &ast::Expr) -> Option<String> {
        if let Some(family) = self.source_call_family(e) {
            return Some(family);
        }
        if matches!(e, ast::Expr::Field { .. } | ast::Expr::Index { .. }) {
            if let Some(family) = self
                .source_operand_layout(e, &HashMap::new())
                .as_ref()
                .and_then(Self::source_layout_family)
            {
                return Some(family);
            }
        }
        // Resolver-owned source types outrank same-spelled caller signals.
        // A library's `let a: integer` must not inherit the float family of
        // the caller's port `a` and recursively dispatch float arithmetic.
        match self.expr_types.get(&ast::expr_span(e)) {
            Some(crate::types::Ty::Integer) => return Some("integer".into()),
            Some(crate::types::Ty::Real) => return Some("real".into()),
            _ => {}
        }
        if let Some(ty) = self.block_local_type(e) {
            return self.free_fns.type_head_key(&ty);
        }
        match e {
            // A branch-valued expression is whatever its branches are; the
            // checker has already made them agree. Without this an `if`/`match`
            // over `signed` values had no family and compared unsigned, while
            // a struct field or a conversion in the same position did not.
            ast::Expr::IfExpr { then, els, .. } => self
                .operand_type_name(then)
                .or_else(|| self.operand_type_name(els)),
            ast::Expr::Match { arms, .. } => arms
                .iter()
                .filter_map(|a| a.value_expr())
                .find_map(|v| self.operand_type_name(v)),
            // An arithmetic or shift expression is whatever its operands are;
            // the checker has already required them to agree. Comparisons and
            // the logical operators yield `Bool` and so carry no numeric
            // family, and a custom operator's result comes from its impl.
            //
            // Without this a binary expression had no family at all, so
            // `print!("{}", a / b)` rendered a `signed` result as unsigned
            // (-3 came out as 253) while `let q: signed[8] = a / b;` — the
            // same value, merely bound first — printed correctly.
            // `not x` is whatever `x` is, in hardware as in the testbench.
            ast::Expr::Unary { rhs, .. } => self.operand_type_name(rhs),
            ast::Expr::Binary { op, lhs, rhs, .. } if op.keeps_operand_family() => {
                let l = self.operand_type_name(lhs);
                let r = self.operand_type_name(rhs);
                // An integer literal takes the family of the other side, so
                // `0 - q` reads as `q`'s family rather than plain `integer`.
                match (l, r) {
                    (Some(l), _) if l != "integer" => Some(l),
                    (_, Some(r)) if r != "integer" => Some(r),
                    (l, r) => l.or(r),
                }
            }
            ast::Expr::Int { .. } => Some("integer".to_string()),
            ast::Expr::SuffixLit { suffix, .. } => self
                .suffix_impls
                .get(&suffix.text)
                .map(|(ty, _)| ty.clone()),
            // A conversion expression `F[N](x)` / `F(x)` reads as its target
            // family, so operators on it dispatch correctly (`signed[32](a) < ..`
            // uses signed's signed Ord).
            ast::Expr::Call { callee, args, .. } => {
                // A generic call is its argument's type: `rem(a, m)` on
                // `signed` values is a `signed`.
                if let Some(argument) = self.generic_return_argument(callee, args) {
                    return self.operand_type_name(argument);
                }
                // A method call is its declared return type, with `Self` (or
                // the receiver's own family) read as the receiver's type:
                // `x.rem(m) < 0` dispatches signed's Ord.
                if let ast::Expr::Field { base, field, .. } = callee.as_ref() {
                    let ty = self.operand_type_name(base)?;
                    let ret = self
                        .find_method(&ty, &field.text, None)?
                        .ret
                        .as_ref()
                        .and_then(|ret| self.free_fns.type_head_key(ret))?;
                    if ret == "Self" {
                        return Some(ty);
                    }
                    return (self.array_families.contains(&ret)
                        || self.enum_variants.contains_key(&ret)
                        || self.structs.contains_key(&ret))
                    .then_some(ret);
                }
                let head = match callee.as_ref() {
                    // `std::fixed::sfixed[7..-8](x)`: the family the path
                    // resolves to, however it is spelled.
                    ast::Expr::Index { base, .. } => match base.as_ref() {
                        ast::Expr::Path(p) => {
                            self.free_fns.type_owner_key(p).or_else(|| expr_path(base))
                        }
                        _ => expr_path(base),
                    },
                    ast::Expr::Path(p) => self
                        .free_fns
                        .type_owner_key(p)
                        .or_else(|| (p.segments.len() == 1).then(|| p.segments[0].text.clone())),
                    _ => None,
                }?;
                // A conversion reads as its target: a nominal array family
                // (`signed[32](a)`) or an enum (`ULogic(b)` inside
                // `Logic(ULogic(b))`).
                if self.array_families.contains(&head) || self.enum_variants.contains_key(&head) {
                    return Some(head);
                }
                // Otherwise it is an ordinary call, and its declared return
                // type is the family. Without this a call had none, so
                // `neg(x) < 0` never dispatched signed's Ord and compared
                // unsigned.
                let ret = self
                    .free_fns
                    .get(callee)
                    .and_then(|f| f.ret.as_ref())
                    .and_then(|ty| self.free_fns.type_head_key(ty))?;
                // A struct return counts too: `twice(v) + v` needs a type for
                // its left operand before any operator impl can be found,
                // and without one the whole expression produced nothing.
                (self.array_families.contains(&ret)
                    || self.enum_variants.contains_key(&ret)
                    || self.structs.contains_key(&ret))
                .then_some(ret)
            }
            ast::Expr::Path(p) if p.segments.len() >= 2 => self
                .free_fns
                .enum_variant_key(p)
                .map(|(enumeration, _)| enumeration),
            // An *array* element is a signal in its own right and resolves by
            // its flattened name. A *bit* of a packed vector is not, so it
            // reads as the vector's element type — otherwise it had no type
            // at all and no operator impl could be found for it: `v[7] xor
            // v[5]` did not lower, while `v[7] and v[5]` did, because `and`
            // is a built-in with its own lowering and needs no impl.
            ast::Expr::Index { base, .. } => {
                if let Some(name) = expr_path(e) {
                    if let Some(found) = self
                        .local_enum
                        .get(&name)
                        .or_else(|| self.local_struct.get(&name))
                        .or_else(|| self.local_numeric.get(&name))
                    {
                        return Some(found.clone());
                    }
                }
                let family = self.operand_type_name(base)?;
                self.array_element_enum(&family)
            }
            _ => {
                let p = expr_path(e)?;
                // A generic-fn parameter reads as its caller's concrete family.
                if let Some(fam) = self.param_types.borrow().get(&p) {
                    return Some(fam.clone());
                }
                if self.local_char.contains(&p) {
                    return Some("Char".to_string());
                }
                self.local_enum
                    .get(&p)
                    .or_else(|| self.local_struct.get(&p))
                    .or_else(|| self.local_numeric.get(&p))
                    .cloned()
            }
        }
    }

    /// Statements that only bind and assign, applied to `env` in order: a
    /// loop body, a return-free branch, or one such statement of a body.
    fn inline_effects(&self, stmts: &[ast::Stmt], env: &mut HashMap<String, Val>) -> Option<()> {
        // ponytail: a fixed unroll cap; a longer loop leaves the call unlowered.
        const MAX_ITERATIONS: u64 = 4096;
        for stmt in stmts {
            match stmt {
                ast::Stmt::Use(_) => {}
                ast::Stmt::Let(l) => {
                    let value = l.value.as_ref()?;
                    let lowered = self.bind_source_value(
                        self.lower_val_env(value, env),
                        l.span,
                        self.expr_types.get(&ast::expr_span(value)).cloned(),
                    );
                    env.insert(l.name.text.clone(), lowered);
                }
                ast::Stmt::Assign {
                    target: ast::Expr::Path(path),
                    value,
                    after: None,
                    span,
                    ..
                } if path.segments.len() == 1 => {
                    let name = &path.segments[0].text;
                    // Only the function's own bindings: anything else is a
                    // signal, which a function cannot drive.
                    if !env.contains_key(name) {
                        return None;
                    }
                    let lowered = self.bind_source_value(
                        self.lower_val_env(value, env),
                        *span,
                        self.expr_types.get(&ast::expr_span(value)).cloned(),
                    );
                    env.insert(name.clone(), lowered);
                }
                ast::Stmt::For {
                    var,
                    range: ast::Expr::Range { lo, hi, .. },
                    body,
                    ..
                } => {
                    let first = self.constant_expr(&self.lower_scalar_env(lo, env))?;
                    let last = self.constant_expr(&self.lower_scalar_env(hi, env))?;
                    if first.abs_diff(last) >= MAX_ITERATIONS {
                        return None;
                    }
                    let step = if first <= last { 1 } else { -1 };
                    let shadowed = env.get(&var.text).cloned();
                    let mut index = first;
                    loop {
                        env.insert(var.text.clone(), Val::Scalar(Expr::Const(index as u64)));
                        self.inline_scoped(&body.stmts, env)?;
                        if index == last {
                            break;
                        }
                        index += step;
                    }
                    match shadowed {
                        Some(value) => env.insert(var.text.clone(), value),
                        None => env.remove(&var.text),
                    };
                }
                ast::Stmt::If(iff) if !if_returns(iff) => {
                    self.inline_merged_if(iff, env)?;
                }
                _ => return None,
            }
        }
        Some(())
    }

    /// A block in its own scope: its `let`s end with it, its assignments to
    /// outer bindings stay.
    fn inline_scoped(&self, stmts: &[ast::Stmt], env: &mut HashMap<String, Val>) -> Option<()> {
        let mut inner = env.clone();
        self.inline_effects(stmts, &mut inner)?;
        let declared = stmts
            .iter()
            .filter_map(|stmt| match stmt {
                ast::Stmt::Let(l) => Some(l.name.text.as_str()),
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>();
        for (name, value) in env.iter_mut() {
            if !declared.contains(name.as_str()) {
                if let Some(updated) = inner.remove(name) {
                    *value = updated;
                }
            }
        }
        Some(())
    }

    /// An `if` without a `return`: each binding a branch changed becomes one
    /// select on the condition.
    fn inline_merged_if(&self, iff: &ast::IfStmt, env: &mut HashMap<String, Val>) -> Option<()> {
        let cond = self.bind_source_expression(
            self.lower_scalar_env(&iff.cond, env),
            ast::expr_span(&iff.cond),
        );
        let mut then_env = env.clone();
        self.inline_scoped(&iff.then.stmts, &mut then_env)?;
        let mut else_env = env.clone();
        match iff.else_.as_deref() {
            Some(ast::ElseBranch::Block(block)) => {
                self.inline_scoped(&block.stmts, &mut else_env)?
            }
            Some(ast::ElseBranch::If(inner)) => self.inline_merged_if(inner, &mut else_env)?,
            None => {}
        }
        let same = |a: &Val, b: &Val| match (a, b) {
            (
                Val::Scalar(Expr::Canonical { value: a, .. }),
                Val::Scalar(Expr::Canonical { value: b, .. }),
            ) => a == b,
            (Val::Scalar(Expr::Const(a)), Val::Scalar(Expr::Const(b))) => a == b,
            _ => false,
        };
        for (name, value) in env.iter_mut() {
            let (then_value, else_value) = (then_env.remove(name)?, else_env.remove(name)?);
            *value = if same(&then_value, &else_value) {
                then_value
            } else {
                self.bind_source_value(
                    self.source_select_value(cond.clone(), then_value, else_value, iff.span),
                    iff.span,
                    None,
                )
            };
        }
        Some(())
    }

    /// The integer a lowered expression always has: constants and arithmetic
    /// over them, through the value arena.
    pub(super) fn constant_expr(&self, expression: &Expr) -> Option<i64> {
        match expression {
            Expr::Const(value) => Some(*value as i64),
            Expr::Canonical { value, .. } => self.constant_expr(&self.source_node(*value)),
            Expr::Binary { op, lhs, rhs } => {
                let (left, right) = (self.constant_expr(lhs)?, self.constant_expr(rhs)?);
                match op {
                    BinOp::Add | BinOp::SAdd => left.checked_add(right),
                    BinOp::Sub | BinOp::SSub => left.checked_sub(right),
                    BinOp::Mul | BinOp::SMul => left.checked_mul(right),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The value a straight-line `return`/`if-else` block produces, or `None`
    /// if the block has statements the inliner cannot express as a value.
    pub(super) fn inline_block(
        &self,
        mut stmts: &[ast::Stmt],
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let _scope = self.source_lexical_scope();
        // A long straight-line let chain is not recursion. Clone the lexical
        // environment at most once, then update its compact value bindings.
        let mut scoped = std::borrow::Cow::Borrowed(env);
        loop {
            while let [ast::Stmt::Let(l), rest @ ..] = stmts {
                let value = l.value.as_ref()?;
                let layout =
                    l.ty.as_ref()
                        .map(|ty| self.source_layout(ty, &self.cur_env))
                        .or_else(|| self.source_operand_layout(value, scoped.as_ref()));
                let value_ir = match &layout {
                    Some(layout)
                        if matches!(
                            layout.kind,
                            LayoutKind::Array { .. } | LayoutKind::Struct { .. }
                        ) =>
                    {
                        self.lower_shaped_source(value, scoped.as_ref(), layout)
                    }
                    _ => self.lower_val_env(value, scoped.as_ref()),
                };
                let lowered = self.bind_source_value(
                    value_ir,
                    l.span,
                    self.expr_types.get(&ast::expr_span(value)).cloned(),
                );
                let mut attrs = HashMap::new();
                self.bind_range_attrs(&mut attrs, &l.name.text, value, scoped.as_ref());
                if let Some(layout) = layout {
                    self.bind_source_shape(l.name.text.clone(), layout);
                }
                let scoped = scoped.to_mut();
                scoped.insert(l.name.text.clone(), lowered);
                scoped.insert(
                    format!("{}::length", l.name.text),
                    Val::Scalar(Expr::Const(self.ast_width(value) as u64)),
                );
                scoped.extend(attrs);
                stmts = rest;
            }
            // Assignments, constant-bound loops and return-free `if`s update the
            // bindings, as a VHDL function's variables do.
            match stmts {
                [statement @ (ast::Stmt::Assign { after: None, .. } | ast::Stmt::For { .. }), rest @ ..] =>
                {
                    self.inline_effects(std::slice::from_ref(statement), scoped.to_mut())?;
                    stmts = rest;
                }
                [ast::Stmt::If(iff), rest @ ..] if !if_returns(iff) => {
                    self.inline_merged_if(iff, scoped.to_mut())?;
                    stmts = rest;
                }
                _ => break,
            }
        }
        let env = scoped.as_ref();
        match stmts {
            [ast::Stmt::Return { value: Some(v), .. }, ..] => {
                let value = match self.source_return_layout() {
                    Some(layout)
                        if matches!(
                            layout.kind,
                            LayoutKind::Array { .. } | LayoutKind::Struct { .. }
                        ) =>
                    {
                        self.lower_shaped_source(v, env, &layout)
                    }
                    _ => self.lower_val_env(v, env),
                };
                Some(self.bind_source_value(value, ast::expr_span(v), None))
            }
            [ast::Stmt::If(iff), rest @ ..] => {
                let cond = self.bind_source_expression(
                    self.lower_scalar_env(&iff.cond, env),
                    ast::expr_span(&iff.cond),
                );
                let then = self.inline_block(&iff.then.stmts, env)?;
                // The else value: an explicit else branch, or the statements
                // after the if.
                let els = match &iff.else_ {
                    Some(e) => match e.as_ref() {
                        ast::ElseBranch::Block(b) => self.inline_block(&b.stmts, env)?,
                        ast::ElseBranch::If(i) => {
                            self.inline_block(std::slice::from_ref(&ast::Stmt::If(i.clone())), env)?
                        }
                    },
                    None => self.inline_block(rest, env)?,
                };
                Some(self.bind_source_value(
                    self.source_select_value(cond, then, els, iff.span),
                    iff.span,
                    None,
                ))
            }
            // A `match` whose arms return is the same shape as an `if`
            // chain, and only the `if` form was handled — the two share
            // `MatchArm` and have drifted apart repeatedly. First-match
            // priority comes from folding the arms in reverse.
            [ast::Stmt::Match(m), rest @ ..] => {
                let scrut = self.bind_source_expression(
                    self.lower_scalar_env(&m.scrutinee, env),
                    ast::expr_span(&m.scrutinee),
                );
                // What the body yields when no arm returns.
                let after = self.inline_block(rest, env);
                let mut acc: Option<Val> = after.clone();
                for arm in m.arms.iter().rev() {
                    let value = match self.inline_block(&arm.body.stmts, env) {
                        Some(value) => value,
                        // An arm that returns nothing (`_ => {}`) falls
                        // through to the statements after the match.
                        None => after.clone()?,
                    };
                    acc = Some(self.bind_source_value(
                        match (
                            self.arm_match_cond(&arm.pattern, &m.scrutinee, &scrut, env),
                            acc,
                        ) {
                            // A wildcard covers everything that follows it.
                            (None, _) => value,
                            // Nothing follows: an exhaustive match ends here, so
                            // this arm is the fallback.
                            (Some(_), None) => value,
                            (Some(cond), Some(otherwise)) => {
                                self.source_select_value(cond, value, otherwise, arm.span)
                            }
                        },
                        arm.span,
                        None,
                    ));
                }
                acc
            }
            _ => None,
        }
    }
}

/// Whether a `return` sits anywhere inside this `if`.
fn if_returns(statement: &ast::IfStmt) -> bool {
    fn any(statements: &[ast::Stmt]) -> bool {
        statements.iter().any(|statement| match statement {
            ast::Stmt::Return { .. } => true,
            ast::Stmt::If(statement) => if_returns(statement),
            ast::Stmt::Match(statement) => statement.arms.iter().any(|arm| any(&arm.body.stmts)),
            ast::Stmt::For { body, .. } => any(&body.stmts),
            _ => false,
        })
    }
    any(&statement.then.stmts)
        || match statement.else_.as_deref() {
            Some(ast::ElseBranch::Block(block)) => any(&block.stmts),
            Some(ast::ElseBranch::If(inner)) => if_returns(inner),
            None => false,
        }
}
