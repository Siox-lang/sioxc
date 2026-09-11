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
        let Some(fns) = self.op_impls.get(&("From".to_string(), target.to_string())) else {
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
                fenv.insert(n.text.clone(), self.lower_val_env(arg, env));
                fenv.insert(
                    format!("{}::length", n.text),
                    Val::Scalar(Expr::Const(self.ast_width(arg) as u64)),
                );
            }
        }
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
            let args = args.iter().map(|a| self.lower_scalar_env(a, env)).collect();
            return Some(Val::Scalar(Expr::CCall {
                name: f.name.text.clone(),
                args,
                f64_args,
                integer_args,
                f64_ret,
                integer_ret,
            }));
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
        // Dynamic arguments: inline the body as an expression tree.
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
                fenv.insert(n.text.clone(), self.lower_val_env(a, env));
                fenv.insert(
                    format!("{}::length", n.text),
                    Val::Scalar(Expr::Const(self.ast_width(a) as u64)),
                );
                // Propagate the argument's family so the body dispatches
                // operators on the caller's concrete type.
                if let Some(fam) = self.operand_type_name(a) {
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
        // An array-typed parameter has no `Val` to bind to: a `Val` is a scalar
        // or a set of named fields, and an array is neither — its elements are
        // separate signals. So `fenv` held nothing useful for it and the body's
        // `v[0]` resolved to nothing, reporting "has no hardware form" with
        // help about runtime indices, pointing inside the callee at a line the
        // caller never wrote. Substituting the parameter's *name* with the
        // argument turns `v[0]` into `d[0]`, an ordinary element read.
        //
        // When one parameter is an array, *every* parameter is substituted:
        // the body's `v[i]` has to become `q[idx]`, and an index left bound in
        // the value environment instead reports `i` as an unknown name — that
        // environment is consulted for a value, not for the index of an
        // element read. A function with no array parameter keeps its value
        // bindings, which carry the width and family that a substituted
        // expression does not.
        let has_array_param = f.params.iter().filter(|p| !p.is_self).any(|p| {
            p.ty.as_ref().is_some_and(|ty| {
                array_of(
                    ty,
                    &self.cur_env,
                    &self.const_ranges,
                    &self.array_families,
                    &self.free_fns,
                )
                .is_some()
            })
        });
        let array_args: HashMap<String, ast::Expr> = if has_array_param {
            f.params
                .iter()
                .filter(|p| !p.is_self)
                .zip(args)
                .filter_map(|(p, a)| Some((p.name.as_ref()?.text.clone(), a.clone())))
                .collect()
        } else {
            HashMap::new()
        };
        let out = f.body.as_ref().and_then(|b| {
            let stmts = self.normalize_struct_returns(&b.stmts, f.ret.as_ref());
            let stmts: Vec<ast::Stmt> = if array_args.is_empty() {
                stmts
            } else {
                stmts
                    .iter()
                    .map(|s| subst_stmt_paths(s, &array_args))
                    .collect()
            };
            self.inline_block(&stmts, &fenv)
        });
        // The result is a value of the declared return type, so it wraps to
        // that width. Assigning it to a signal masked it anyway, which hid
        // this — but used in place (`neg(x) < 0`) the extra bits survived and
        // signed's Ord tested the wrong one.
        // Only a scalar result has a declared width to wrap to; a struct's
        // leaves were already masked field by field as the body built them.
        let out = match (out, f.ret.as_ref()) {
            (Some(Val::Scalar(v)), Some(ret)) => Some(Val::Scalar(self.mask_to_type_width(v, ret))),
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
    pub(super) fn mask_to_type_width(&self, v: Expr, ret: &ast::Type) -> Expr {
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
        Expr::Binary {
            op: BinOp::And,
            lhs: Box::new(v),
            rhs: Box::new(Expr::Const((1u64 << w) - 1)),
        }
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
            .filter(|((_, t), _)| t == ty)
            .flat_map(|(_, fns)| fns.iter())
            .find(|(f, rhs)| {
                f.name.text == name
                    && input.is_none_or(|input| rhs.as_deref().is_none_or(|rhs| rhs == input))
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
        // Bind `self` to the receiver's signal so a `self'event`/`self'old`
        // sysattr in the body (the std `ClockLike` edge methods) resolves to it.
        let saved_self = self.self_signal.replace(self.base_signal(base));
        let mut fenv: HashMap<String, Val> = HashMap::new();
        fenv.insert("self".to_string(), self.lower_val_env(base, env));
        fenv.insert(
            "self::length".to_string(),
            Val::Scalar(Expr::Const(self.ast_width(base) as u64)),
        );
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
                fenv.insert(n.text.clone(), self.lower_val_env(a, env));
                fenv.insert(
                    format!("{}::length", n.text),
                    Val::Scalar(Expr::Const(
                        self.literal_aware_width(a, receiver_width) as u64
                    )),
                );
                if let Some(fam) = self.operand_type_name(a) {
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
        // A method's array parameter needs the same substitution a free
        // function's does — the value environment has no array case, so the
        // body's `v[0]` resolved to nothing.
        let array_args: HashMap<String, ast::Expr> =
            if f.params.iter().filter(|p| !p.is_self).any(|p| {
                p.ty.as_ref().is_some_and(|ty| {
                    array_of(
                        ty,
                        &self.cur_env,
                        &self.const_ranges,
                        &self.array_families,
                        &self.free_fns,
                    )
                    .is_some()
                })
            }) {
                f.params
                    .iter()
                    .filter(|p| !p.is_self)
                    .zip(args)
                    .filter_map(|(p, a)| Some((p.name.as_ref()?.text.clone(), a.clone())))
                    .collect()
            } else {
                HashMap::new()
            };
        let stmts: Vec<ast::Stmt> = if array_args.is_empty() {
            body.stmts.clone()
        } else {
            body.stmts
                .iter()
                .map(|s| subst_stmt_paths(s, &array_args))
                .collect()
        };
        let out = self.inline_block(&stmts, &fenv);
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
        self.inline_depth.set(self.inline_depth.get() - 1);
        out
    }

    /// Lower a method call used as a *statement* (`s.send(v)`): inline the
    /// method's body as drivers, substituting `self` -> receiver and each
    /// parameter -> its argument, so a body of `self.valid = '1'; self.data =
    /// value;` drives the receiver's flattened field signals. Returns `false`
    /// when the receiver's type or the method can't be resolved (the caller
    /// then leaves the statement to the existing fall-through).
    /// The body of a method call in statement position, with `self` and the
    /// parameters substituted — shared by the combinational and sequential
    /// walkers so a call means the same thing in both. `None` when the call is
    /// not a known method with a body.
    pub(super) fn method_stmt_body(
        &mut self,
        recv: &ast::Expr,
        method: &str,
        args: &[ast::Expr],
    ) -> Option<Vec<ast::Stmt>> {
        let ty = self.operand_type_name(recv)?;
        // `f` borrows the AST (`'a`), not `self`, so it survives the `&mut self`
        // lowering calls below.
        let input = args.first().and_then(|arg| match arg {
            ast::Expr::Construct { ty: Some(ty), .. } if type_head_name(ty) == Some("Range") => {
                Some("Range".to_string())
            }
            _ => self.operand_type_name(arg),
        });
        let f = self.find_method(&ty, method, input.as_deref())?;
        let body = f.body.as_ref()?;
        let mut map: HashMap<String, ast::Expr> = HashMap::new();
        map.insert("self".to_string(), recv.clone());
        for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(n) = &p.name {
                map.insert(n.text.clone(), a.clone());
            }
        }
        Some(
            body.stmts
                .iter()
                .map(|s| subst_stmt_paths(s, &map))
                .collect(),
        )
    }

    /// Inline a method call in statement position as combinational drivers.
    pub(super) fn lower_method_stmt(
        &mut self,
        recv: &ast::Expr,
        method: &str,
        args: &[ast::Expr],
        cond: Option<Expr>,
    ) -> bool {
        let Some(stmts) = self.method_stmt_body(recv, method, args) else {
            return false;
        };
        let span = ast::expr_span(recv);
        self.lower_combinational_block(&ast::Block { stmts, span }, cond);
        true
    }

    /// Inline a free function called in statement position. This is the
    /// procedure-shaped counterpart of `lower_free_call`: parameters are
    /// substituted with their concrete expressions, then assignments and
    /// nested method calls are lowered as ordinary drivers.
    pub(super) fn free_stmt_body(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
    ) -> Option<Vec<ast::Stmt>> {
        let f = self.free_fns.get(callee)?;
        let body = f.body.as_ref()?;
        let mut map: HashMap<String, ast::Expr> = HashMap::new();
        for (param, arg) in f.params.iter().filter(|param| !param.is_self).zip(args) {
            if let Some(name) = &param.name {
                map.insert(name.text.clone(), arg.clone());
            }
        }
        Some(
            body.stmts
                .iter()
                .map(|stmt| subst_stmt_paths(stmt, &map))
                .collect(),
        )
    }

    /// Inline a free call in statement position as combinational drivers.
    pub(super) fn lower_free_stmt(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        cond: Option<Expr>,
    ) -> bool {
        let Some(stmts) = self.free_stmt_body(callee, args) else {
            return false;
        };
        let span = ast::expr_span(callee);
        self.lower_combinational_block(&ast::Block { stmts, span }, cond);
        true
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
                            Expr::Slice {
                                base: Box::new(v),
                                hi: w - 1,
                                lo: 0,
                            }
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
            ast::Expr::Path(p) if p.segments.len() == 1 && p.segments[0].text == "integer" => {
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
        // ...except crossing out of `real`, which is a value conversion: the
        // operand carries f64 bits, and resizing them keeps a mantissa slice
        // rather than the number.
        if self.is_real_expr(&v) {
            v = Expr::Unary {
                op: UnOp::RealToInt,
                rhs: Box::new(v),
            };
        }
        Some(match target_w {
            Some(w) if w > 0 && w < 64 => Expr::Slice {
                base: Box::new(v),
                hi: w - 1,
                lo: 0,
            },
            _ => v,
        })
    }

    /// The type name an operand contributes to operator-impl lookup: a local's
    /// declared enum/struct, a suffix literal's target type, an enum variant's
    /// enum, or `integer` for a bare numeric literal.
    pub(super) fn operand_type_name(&self, e: &ast::Expr) -> Option<String> {
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
            ast::Expr::Call { callee, .. } => {
                let head = match callee.as_ref() {
                    ast::Expr::Index { base, .. } => expr_path(base),
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
                // its left operand before any `Operator` impl can be found,
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

    /// Read every `return` in `stmts` against the function's declared return
    /// type, so a positional literal returned from a struct-returning function
    /// (`return { 3, 4 }`) is a struct literal rather than the concatenation it
    /// lexes as. Returned as the concat it produced no fields, and the caller's
    /// destination was left undriven.
    ///
    /// Only the shapes the inliner itself understands are walked; anything else
    /// is carried through unchanged.
    pub(super) fn normalize_struct_returns(
        &self,
        stmts: &[ast::Stmt],
        ret: Option<&ast::Type>,
    ) -> Vec<ast::Stmt> {
        stmts
            .iter()
            .map(|stmt| match stmt {
                ast::Stmt::Return {
                    value: Some(value),
                    span,
                } => ast::Stmt::Return {
                    value: Some(self.as_struct_literal(ret, value)),
                    span: *span,
                },
                ast::Stmt::If(iff) => {
                    let mut iff = iff.clone();
                    iff.then.stmts = self.normalize_struct_returns(&iff.then.stmts, ret);
                    iff.else_ = iff.else_.map(|branch| {
                        Box::new(match *branch {
                            ast::ElseBranch::Block(mut b) => {
                                b.stmts = self.normalize_struct_returns(&b.stmts, ret);
                                ast::ElseBranch::Block(b)
                            }
                            ast::ElseBranch::If(inner) => {
                                let rewritten = self.normalize_struct_returns(
                                    std::slice::from_ref(&ast::Stmt::If(inner.clone())),
                                    ret,
                                );
                                match rewritten.into_iter().next() {
                                    Some(ast::Stmt::If(inner)) => ast::ElseBranch::If(inner),
                                    _ => ast::ElseBranch::If(inner),
                                }
                            }
                        })
                    });
                    ast::Stmt::If(iff)
                }
                ast::Stmt::Match(m) => {
                    let mut m = m.clone();
                    for arm in &mut m.arms {
                        arm.body.stmts = self.normalize_struct_returns(&arm.body.stmts, ret);
                    }
                    ast::Stmt::Match(m)
                }
                other => other.clone(),
            })
            .collect()
    }

    /// The value a straight-line `return`/`if-else` block produces, or `None`
    /// if the block has statements the inliner cannot express as a value.
    pub(super) fn inline_block(
        &self,
        stmts: &[ast::Stmt],
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        match stmts {
            [ast::Stmt::Return { value: Some(v), .. }, ..] => Some(self.lower_val_env(v, env)),
            [ast::Stmt::If(iff), rest @ ..] => {
                let cond = self.lower_scalar_env(&iff.cond, env);
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
                Some(select_val(cond, then, els))
            }
            // A `match` whose arms return is the same shape as an `if`
            // chain, and only the `if` form was handled — the two share
            // `MatchArm` and have drifted apart repeatedly. First-match
            // priority comes from folding the arms in reverse.
            [ast::Stmt::Match(m), rest @ ..] => {
                let scrut = self.lower_scalar_env(&m.scrutinee, env);
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
                    acc = Some(
                        match (
                            self.arm_match_cond(&arm.pattern, &m.scrutinee, &scrut, env),
                            acc,
                        ) {
                            // A wildcard covers everything that follows it.
                            (None, _) => value,
                            // Nothing follows: an exhaustive match ends here, so
                            // this arm is the fallback.
                            (Some(_), None) => value,
                            (Some(cond), Some(otherwise)) => select_val(cond, value, otherwise),
                        },
                    );
                }
                acc
            }
            // `let t: T = expr;` names a value for the statements that
            // follow. Without this arm the body matched neither shape and the
            // whole call lowered to an `Unknown` — and silently, because
            // `check` and `--emit ir` both pass on it and only code
            // generation reports the unlowered driver.
            [ast::Stmt::Let(l), rest @ ..] => {
                let value = l.value.as_ref()?;
                let mut scoped = env.clone();
                scoped.insert(l.name.text.clone(), self.lower_val_env(value, env));
                scoped.insert(
                    format!("{}::length", l.name.text),
                    Val::Scalar(Expr::Const(self.ast_width(value) as u64)),
                );
                self.inline_block(rest, &scoped)
            }
            _ => None,
        }
    }
}
