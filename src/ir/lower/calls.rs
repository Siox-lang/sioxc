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
        if let Some(value) = self.lower_from_elements(&target, callee, args, env) {
            return Some(value);
        }
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

    /// `T(v)` from an anonymous array's elements (`unsigned(v)` with `v:
    /// Logic[4]`): `impl From<Logic[]> for T`, as an ordinary call.
    pub(super) fn lower_from_elements(
        &self,
        target: &str,
        callee: &ast::Expr,
        args: &[ast::Expr],
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let [arg] = args else {
            return None;
        };
        // The checker types an unsized conversion's argument as the target;
        // its layout still says it is an array of enum elements.
        let key = self.anonymous_array_key(arg).or_else(|| {
            match self.source_operand_layout(arg, env)?.kind {
                LayoutKind::Array { element, .. } => match element.kind {
                    LayoutKind::Scalar {
                        domain: crate::ir::ScalarDomain::Enum(name),
                        ..
                    } => Some(format!("{}[]", self.free_fns.canonical_type_key(&name))),
                    _ => None,
                },
                _ => None,
            }
        })?;
        let (f, _) = self
            .op_impls
            .get("From", target)?
            .iter()
            .find(|(f, declared)| {
                declared.clone().or_else(|| {
                    f.params
                        .iter()
                        .find(|p| !p.is_self)
                        .and_then(|p| p.ty.as_ref())
                        .and_then(|ty| self.free_fns.trait_arg_key(ty))
                }) == Some(key.clone())
            })?;
        self.inline_function(f, "from".to_string(), callee, args, env)
    }

    /// An anonymous array argument's conversion key, its element's with `[]`,
    /// as `From<Logic[]>` registers it.
    pub(super) fn anonymous_array_key(&self, arg: &ast::Expr) -> Option<String> {
        match self.expr_types.get(&ast::expr_span(arg)) {
            Some(crate::types::Ty::Array {
                family: None, elem, ..
            }) => match elem.as_ref() {
                crate::types::Ty::Named(definition) => self
                    .free_fns
                    .nominal_type_key(*definition)
                    .map(|key| format!("{key}[]")),
                _ => None,
            },
            _ => None,
        }
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
        let source = match self.anonymous_array_key(arg).or_else(|| {
            self.operand_type_name(arg)
                .filter(|family| family != "integer")
        }) {
            Some(family) => family,
            None => match self.expr_types.get(&ast::expr_span(arg)) {
                Some(crate::types::Ty::Real) => "real".to_string(),
                _ if self.is_real_expr(&self.lower_scalar_env(arg, env)) => "real".to_string(),
                _ => "integer".to_string(),
            },
        };
        if let (Some(_target_std), Some(source_std)) = (
            super::source_processes::std_integer_vector_family(&target),
            super::source_processes::std_integer_vector_family(&source),
        ) {
            let (left, right) = match index.as_ref() {
                ast::Expr::Range { lo, hi, .. } => (
                    self.eval_const(lo, &self.cur_env)?,
                    self.eval_const(hi, &self.cur_env)?,
                ),
                width => (self.eval_const(width, &self.cur_env)? - 1, 0),
            };
            let target_width = u32::try_from(left.abs_diff(right) + 1).ok()?;
            let source_width = self
                .source_operand_layout(arg, env)
                .and_then(|l| match l.kind {
                    LayoutKind::Packed { width, .. } => Some(width),
                    _ => None,
                })
                .unwrap_or_else(|| self.ast_width(arg));
            if target_width > 64 && source_width > 0 {
                return self.lower_wide_packed_resize(
                    arg,
                    env,
                    source_std == "signed",
                    source_width,
                    target_width,
                    &target,
                    left,
                    right,
                    ast::expr_span(callee),
                );
            }
        }
        let (f, _) = fns.iter().find(|(f, declared)| {
            declared.clone().or_else(|| {
                f.params
                    .iter()
                    .find(|p| !p.is_self)
                    .and_then(|p| p.ty.as_ref())
                    .and_then(|ty| self.free_fns.type_head_key(ty))
            }) == Some(source.clone())
        })?;
        // A vector built from an anonymous array's elements is an ordinary
        // call: the format inliner binds only packed arguments.
        if source.ends_with("[]") {
            return match self.inline_function(f, "from".to_string(), callee, args, env)? {
                Val::Scalar(value) => Some(value),
                _ => None,
            };
        }
        self.inline_in_format(f, index, callee, args, env)
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_wide_packed_resize(
        &self,
        arg: &ast::Expr,
        env: &HashMap<String, Val>,
        source_signed: bool,
        source_width: u32,
        target_width: u32,
        target: &str,
        left: i64,
        right: i64,
        span: crate::diag::Span,
    ) -> Option<Expr> {
        let v = self.lower_scalar_env(arg, env);
        let elem = self
            .expr_types
            .get(&span)
            .and_then(|ty| match ty {
                crate::types::Ty::Array { elem, .. } => Some((**elem).clone()),
                _ => None,
            })
            .unwrap_or(crate::types::Ty::Error);
        let target_type = Some(crate::types::Ty::Array {
            elem: Box::new(elem),
            family: Some(target.to_string()),
            len: target_width,
            low: left.min(right),
        });
        let target_layout = Some(SourceLayout {
            span,
            kind: LayoutKind::Packed {
                width: target_width,
                family: target.to_string(),
                range: Some(LayoutRange { left, right }),
                element_enum: self.array_element_enum(target),
            },
        });
        let mut source_values = self.source_values.borrow_mut();
        let operand = source_values.append(&v, span, None);
        let final_kind = if source_signed && target_width > source_width {
            let resized = source_values.push_node(
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(target_width),
                    kind: ProcessValueKind::RawResize { operand },
                },
                None,
            );
            let sign = source_values.push_node(
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(target_width),
                    kind: ProcessValueKind::Number(ProcessNumber::Integer(
                        super::source_processes::single_bit_words(source_width - 1),
                    )),
                },
                None,
            );
            let flipped = source_values.push_node(
                ProcessValue {
                    span,
                    ty: None,
                    bit_width: Some(target_width),
                    kind: ProcessValueKind::Binary {
                        operation: ProcessBinaryOp::Xor,
                        left: resized,
                        right: sign,
                    },
                },
                None,
            );
            ProcessValueKind::Binary {
                operation: ProcessBinaryOp::Sub,
                left: flipped,
                right: sign,
            }
        } else {
            ProcessValueKind::RawResize { operand }
        };
        let final_id = source_values.push_node(
            ProcessValue {
                span,
                ty: target_type,
                bit_width: Some(target_width),
                kind: final_kind,
            },
            target_layout,
        );
        Some(source_values.reference(final_id))
    }

    /// Inline `f` with `Self'left`/`'right`/`'high`/`'low`/`'length`
    /// describing the format `index` gives, each parameter bound to its
    /// argument with that argument's own format (`value'low`): a `From`
    /// conversion to the format, or an associated call on it
    /// (`ufixed<6, 2>::resize(x, ..)`).
    pub(super) fn inline_in_format(
        &self,
        f: &ast::FnDecl,
        index: &ast::Expr,
        callee: &ast::Expr,
        args: &[ast::Expr],
        env: &HashMap<String, Val>,
    ) -> Option<Expr> {
        f.body.as_ref()?;
        let (left, right) = match index {
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
        let params: Vec<&ast::Ident> = f
            .params
            .iter()
            .filter(|p| !p.is_self)
            .map(|p| p.name.as_ref())
            .collect::<Option<_>>()?;
        if params.len() != args.len() {
            return None;
        }
        let family = match callee {
            ast::Expr::Index { base, .. } => match base.as_ref() {
                ast::Expr::Path(path) => self
                    .free_fns
                    .struct_path_key(path)
                    .or_else(|| expr_path(base)),
                _ => expr_path(base),
            },
            ast::Expr::Path(path) => path
                .segments
                .len()
                .checked_sub(2)
                .map(|owner| path.segments[owner].text.clone()),
            _ => None,
        };
        let in_format = f.ret.as_ref().and_then(type_head_name).is_some_and(|head| {
            head == "Self"
                || family
                    .as_deref()
                    .is_some_and(|family| family.rsplit("::").next() == Some(head))
        });
        let inspection = super::source_bindings::SourceCallInspection {
            parameters: params
                .iter()
                .zip(args)
                .filter_map(|(param, arg)| {
                    self.source_operand_layout(arg, env)
                        .map(|layout| (param.text.clone(), layout))
                })
                .collect(),
            result: family.as_ref().filter(|_| in_format).and_then(|target| {
                Some(SourceLayout {
                    span: ast::expr_span(callee),
                    kind: LayoutKind::Packed {
                        width: u32::try_from(left.abs_diff(right) + 1).ok()?,
                        family: target.clone(),
                        range: Some(LayoutRange { left, right }),
                        element_enum: self.array_element_enum(target),
                    },
                })
            }),
        };
        let mut saved_widths = Vec::new();
        for (param, arg) in params.iter().zip(args) {
            fenv.insert(
                param.text.clone(),
                self.bind_source_value(self.lower_val_env(arg, env), ast::expr_span(arg), None),
            );
            // A resize reads the source format too: `value'low`, and its
            // width travels to the calls in the body (`word(value)`).
            self.bind_range_attrs(&mut fenv, &param.text, arg, env);
            let width = self.ast_width(arg);
            fenv.insert(
                format!("{}::length", param.text),
                Val::Scalar(Expr::Const(width as u64)),
            );
            saved_widths.push((
                param.text.clone(),
                self.param_widths
                    .borrow_mut()
                    .insert(param.text.clone(), width),
            ));
        }
        let shapes = self.source_shape_scope(
            HashMap::new(),
            f.ret
                .as_ref()
                .map(|ty| self.source_layout(ty, &self.cur_env)),
        );
        let out =
            self.inline_source_function(f, ast::expr_span(callee), &mut fenv, Some(&inspection));
        drop(shapes);
        for (name, saved) in saved_widths {
            match saved {
                Some(previous) => self.param_widths.borrow_mut().insert(name, previous),
                None => self.param_widths.borrow_mut().remove(&name),
            };
        }
        // A result of the type itself (`-> ufixed`, `-> Self`) is a word of
        // the format, as a stored one is: operator bodies read its sign bit
        // at `'length - 1`. Any other result (`-> integer`) keeps its width.
        if !in_format {
            return match out? {
                Val::Scalar(value) => Some(value),
                _ => None,
            };
        }
        let length = u32::try_from(left.abs_diff(right) + 1).ok()?;
        match out? {
            Val::Scalar(value) if length < 64 => {
                Some(self.source_slice(&value, length - 1, 0, ast::expr_span(callee)))
            }
            Val::Scalar(value) => Some(value),
            _ => None,
        }
    }

    /// `ufixed<6, 2>::resize(x, ..)`: an associated function called on an
    /// applied type, inlined with `Self` describing that type's format.
    pub(super) fn lower_qualified_call(
        &self,
        qualifier: &ast::Type,
        callee: &ast::Expr,
        args: &[ast::Expr],
        env: &HashMap<String, Val>,
    ) -> Option<Expr> {
        let ast::Type::Indexed {
            index: Some(index), ..
        } = qualifier
        else {
            return None;
        };
        let f = self.free_fns.get(callee)?;
        self.inline_in_format(f, index, callee, args, env)
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
        f.body.as_ref()?;
        let mut inspection = super::source_bindings::SourceCallInspection {
            result: self.named_source_layout(target, ast::expr_span(arg)),
            ..Default::default()
        };
        let mut fenv: HashMap<String, Val> = HashMap::new();
        if let Some(p) = f.params.iter().find(|p| !p.is_self) {
            if let Some(n) = &p.name {
                if let Some(layout) = self.source_operand_layout(arg, env) {
                    inspection.parameters.insert(n.text.clone(), layout);
                }
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
        self.inline_source_function(f, ast::expr_span(arg), &mut fenv, Some(&inspection))
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
        self.inline_function(f, display_name, callee, args, env)
    }

    /// `arg` as a `real` through its type's `impl From<S> for real`
    /// (`real(ufixed)`), inlined at `site`. `None` when it has no such impl.
    pub(super) fn real_from(
        &self,
        site: &ast::Expr,
        arg: &ast::Expr,
        env: &HashMap<String, Val>,
    ) -> Option<Expr> {
        let family = self.operand_type_name(arg)?;
        let function = self.op_impls.get("From", "real").and_then(|impls| {
            impls.iter().find_map(|(f, source)| {
                (source.clone().or_else(|| {
                    f.params
                        .iter()
                        .find(|p| !p.is_self)
                        .and_then(|p| p.ty.as_ref())
                        .and_then(|ty| self.free_fns.type_head_key(ty))
                }) == Some(family.clone()))
                .then_some(*f)
            })
        })?;
        match self.inline_function(
            function,
            format!("real({family})"),
            site,
            std::slice::from_ref(arg),
            env,
        )? {
            Val::Scalar(value) => Some(value),
            _ => None,
        }
    }

    /// `real op x` (or `x op real`) for an `x` whose type has `impl From<S>
    /// for real`: both operands as `real`, `x` converted by that impl. `None`
    /// when the operands do not mix `real` with such a type; an `integer`
    /// side keeps its own coercion.
    pub(super) fn promoted_real_operands(
        &self,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        env: &HashMap<String, Val>,
    ) -> Option<(Expr, Expr)> {
        let is_real = |e: &ast::Expr| {
            matches!(
                self.expr_types.get(&ast::expr_span(e)),
                Some(crate::types::Ty::Real)
            )
        };
        let (left_real, right_real) = (is_real(lhs), is_real(rhs));
        if left_real == right_real {
            return None;
        }
        let other = if left_real { rhs } else { lhs };
        if matches!(
            self.expr_types.get(&ast::expr_span(other)),
            Some(crate::types::Ty::Integer) | None
        ) {
            return None;
        }
        let converted = self.real_from(other, other, env)?;
        let real = self.lower_scalar_env(if left_real { lhs } else { rhs }, env);
        Some(if left_real {
            (real, converted)
        } else {
            (converted, real)
        })
    }

    /// Inline `f` for a call at `callee` with `args`, as a free function's
    /// call is; `display_name` names it in depth diagnostics.
    pub(super) fn inline_function(
        &self,
        f: &ast::FnDecl,
        display_name: String,
        callee: &ast::Expr,
        args: &[ast::Expr],
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        // A `core::intrinsics` function is a native operation, not a call.
        if let Some(intrinsic) = self.free_fns.intrinsic(f) {
            let span = ast::expr_span(callee);
            let operands: Vec<Expr> = args
                .iter()
                .map(|a| self.coerce_real(self.lower_scalar_env(a, env), ast::expr_span(a)))
                .collect();
            let mut arena = self.source_values.borrow_mut();
            return Some(Val::Scalar(match (intrinsic, operands.as_slice()) {
                (crate::ir::Intrinsic::Real(function), [x]) => {
                    arena.unary(ProcessUnaryOp::Real(function), x, span)
                }
                (crate::ir::Intrinsic::Pow, [x, y]) => arena.binary(BinOp::FPow, x, y, span),
                _ => return None,
            }));
        }
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
            let c_scalar = |t: &Option<ast::Type>| {
                crate::ir::CScalar::of(t.as_ref()?, self.resolved, |path| {
                    self.aliases.get(&self.free_fns.type_alias_path_key(path)?)
                })
            };
            let c_arguments = f
                .params
                .iter()
                .filter(|p| !p.is_self)
                .map(|p| c_scalar(&p.ty))
                .collect();
            let c_result = c_scalar(&f.ret);
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
                        void_result: false,
                        c_arguments,
                        c_result,
                        pointer_arguments: Vec::new(),
                        aggregate_result: false,
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
            if self.depth_exceeded.borrow().is_empty() {
                self.depth_exceeded
                    .borrow_mut()
                    .push((display_name, ast::expr_span(callee)));
            }
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
                // A literal argument (`f(165)`, `f(0 - 3)`) is as wide as the
                // parameter it fills, not its minimal width (`7` alone is
                // three bits).
                let literal = self.operand_type_name(a).as_deref() == Some("integer")
                    && self.eval_const(a, &self.cur_env).is_some();
                let literal_width = literal
                    .then(|| {
                        p.ty.as_ref()
                            .and_then(|ty| self.source_layout(ty, &self.cur_env).bit_width())
                            .and_then(|width| u32::try_from(width).ok())
                    })
                    .flatten()
                    .filter(|width| *width > 0);
                // An array's length is its element count; `ast_width` is
                // one element's width (`v'length` sized a local `1`).
                let length = match self.source_operand_layout(a, env).map(|layout| layout.kind) {
                    Some(LayoutKind::Array {
                        range: Some(range), ..
                    }) => range.len().unwrap_or(0),
                    _ => u64::from(literal_width.unwrap_or_else(|| self.ast_width(a))),
                };
                fenv.insert(
                    format!("{}::length", n.text),
                    Val::Scalar(Expr::Const(length)),
                );
                self.bind_range_attrs(&mut fenv, &n.text, a, env);
                // Propagate the argument's family so the body dispatches
                // operators on the caller's concrete type.
                // A literal argument (`'1'`, `"01--"`) has no family of its
                // own: the parameter's declared type names it.
                let declared =
                    p.ty.as_ref()
                        .and_then(type_head_name)
                        .filter(|head| !f.generics.params.iter().any(|g| g.name.text == *head))
                        .map(str::to_owned);
                // ...and a bare integer for a vector parameter is that vector:
                // read as `integer`, `b nand 7` in `f(165)` found no impl and
                // the call lowered to nothing.
                let family = match (self.operand_type_name(a), declared) {
                    (Some(_), Some(declared)) if literal => Some(declared),
                    (argument, declared) => argument.or(declared),
                };
                if let Some(fam) = family {
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
                let w = literal_width.unwrap_or_else(|| self.ast_width(a));
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
        let out = self.inline_source_function(f, ast::expr_span(callee), &mut fenv, None);
        // The result is a value of the declared return type, so it wraps to
        // that width. Assigning it to a signal masked it anyway, which hid
        // this — but used in place (`neg(x) < 0`) the extra bits survived and
        // signed's Ord tested the wrong one.
        // Only a scalar result has a declared width to wrap to; a struct's
        // leaves were already masked field by field as the body built them.
        let out = match (out, f.ret.as_ref()) {
            (Some(Val::Scalar(v)), Some(ret)) => Some(Val::Scalar(self.mask_to_type_width(
                v,
                ret,
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
        if w == 0 || w == 64 {
            return v;
        }
        if w < 64 {
            self.source_binary(BinOp::And, &v, &Expr::Const((1u64 << w) - 1), span)
        } else {
            self.source_slice(&v, w - 1, 0, span)
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
            // A literal takes its type from the parameter, as on assignment,
            // so it names no impl: `v.matches("01--")`, `l.matches('1')`.
            ast::Expr::StrLit { .. } | ast::Expr::BitStrLit { .. } | ast::Expr::CharLit { .. } => {
                None
            }
            _ => self.operand_type_name(arg),
        });
        let f = self.find_method(&ty, &field.text, input.as_deref())?;
        f.body.as_ref()?;
        let is_op_method = self
            .op_impls
            .iter()
            .filter(|(_, owner, _)| *owner == ty)
            .flat_map(|(_, _, fns)| fns.iter())
            .any(|(op_fn, _)| std::ptr::eq(*op_fn, f));
        let max_depth = if is_op_method { 20 } else { 16 };
        if self.inline_depth.get() > max_depth {
            if self.depth_exceeded.borrow().is_empty() {
                self.depth_exceeded
                    .borrow_mut()
                    .push((format!("{ty}.{}", field.text), ast::expr_span(callee)));
            }
            return None;
        }
        self.inline_depth.set(self.inline_depth.get() + 1);
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
                // A vector literal given to an unsized parameter (`pattern:
                // unsigned` given `"01--"`) takes the receiver's shape, as it
                // would take its target's on assignment.
                let width_generic = p
                    .ty
                    .as_ref()
                    .is_none_or(|ty| self.source_layout(ty, &self.cur_env).bit_width().is_none());
                let receiver = shapes.get("self").cloned().filter(|_| {
                    width_generic
                        && matches!(a, ast::Expr::StrLit { .. } | ast::Expr::BitStrLit { .. })
                });
                let (argument, layout) = match receiver {
                    Some(receiver) => (self.lower_shaped_source(a, env, &receiver), Some(receiver)),
                    None => self.lower_source_argument(a, p.ty.as_ref(), env),
                };
                if let Some(layout) = layout {
                    shapes.insert(n.text.clone(), layout);
                }
                let value = match argument {
                    // A constant for a vector parameter is cut to the width it
                    // takes (the receiver's), as a signal of that width holds
                    // it: `(a + 0) == (0 - 5)` compared 0xFB with all of the
                    // kernel word's ones.
                    Val::Scalar(_)
                        if p.ty.as_ref().and_then(type_head_name) != Some("integer")
                            // Only where the integer is the word: a fixed
                            // format's argument is already built as its value
                            // (`a < 3` on `ufixed` is 3.0).
                            && self.promotion_of(a).is_none_or(|ty| {
                                matches!(ty, crate::types::Ty::Array { family: Some(family), .. }
                                    if matches!(family.rsplit("::").next(), Some("signed" | "unsigned")))
                            })
                            && self.eval_const(a, &self.cur_env).is_some()
                            && self.literal_aware_width(a, receiver_width) > 0 =>
                    {
                        let w = self.literal_aware_width(a, receiver_width);
                        let v = self.eval_const(a, &self.cur_env).unwrap();
                        Val::Scalar(if w < 64 {
                            Expr::Const(v as u64 & ((1u64 << w) - 1))
                        } else if w > 64 && v < 0 {
                            // Sign-extended through every word, the top one
                            // cut to the width.
                            let mut words = vec![u64::MAX; w.div_ceil(64) as usize];
                            words[0] = v as u64;
                            if !w.is_multiple_of(64) {
                                *words.last_mut().unwrap() &= (1u64 << (w % 64)) - 1;
                            }
                            Expr::WideConst(words)
                        } else {
                            Expr::Const(v as u64)
                        })
                    }
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
                // A literal argument (`'1'`) has no family of its own: the
                // parameter's declared type names it, or `Self` the receiver's.
                let declared = p.ty.as_ref().and_then(type_head_name).map(|head| {
                    if head == "Self" {
                        ty.clone()
                    } else {
                        head.to_owned()
                    }
                });
                if let Some(mut fam) = self.operand_type_name(a).or(declared) {
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
                // parameter, and without this it saw none. A literal's is the
                // receiver's, as its `'length` above: `c > 100` reaches
                // `rhs.lt(self)` with `rhs` = 100, and at its own seven bits
                // 100 tested negative.
                let w = self.literal_aware_width(a, receiver_width);
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
        // Marked only now: the receiver and arguments above are the caller's
        // expressions, and a comparison inside them (`(if d > c { d } else
        // { c }) < c`) is not this body recursing. Marked before them, it
        // fell back to the built-in unsigned order.
        self.inlining_methods
            .borrow_mut()
            .push((ty.clone(), field.text.clone()));
        let out = self.inline_source_function(f, ast::expr_span(callee), &mut fenv, None);
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
        // A widening `resize(x, n)` is `F[n](x)` for `x`'s family `F`:
        // `From<signed> for signed` sign-extends, where the raw resize
        // zero-extended (-3 into 16 bits read 253). Narrowing keeps the low
        // bits either way, and std's bodies use `resize(self, self'length)`.
        if let (ast::Expr::Path(p), [operand, width]) = (callee, args) {
            let widening = self
                .eval_const(width, &self.cur_env)
                .is_some_and(|width| width > i64::from(self.ast_width(operand)));
            if widening && p.segments.len() == 1 && p.segments[0].text == "resize" {
                let family = self.operand_type_name(operand);
                let function = family.as_ref().and_then(|family| {
                    let fns = self.op_impls.get("From", family)?;
                    fns.iter().find_map(|(f, declared)| {
                        let source = declared.clone().or_else(|| {
                            f.params
                                .iter()
                                .find(|p| !p.is_self)
                                .and_then(|p| p.ty.as_ref())
                                .and_then(|ty| self.free_fns.type_head_key(ty))
                        });
                        (source.as_ref() == Some(family)).then_some(f)
                    })
                });
                if let Some(value) = function.and_then(|function| {
                    self.inline_in_format(
                        function,
                        width,
                        callee,
                        std::slice::from_ref(operand),
                        env,
                    )
                }) {
                    return Some(value);
                }
            }
        }
        if let ast::Expr::Path(p) = callee {
            if let Some(Val::Scalar(value)) = self
                .free_fns
                .struct_path_key(p)
                .and_then(|target| self.lower_from_elements(&target, callee, args, env))
            {
                return Some(value);
            }
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
                        return Some(if w > 0 && w != 64 {
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
                    && matches!(p.segments[0].text.as_str(), "integer" | "natural" | "real") =>
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
                let w = match index.as_ref() {
                    ast::Expr::Range { lo, hi, .. } => {
                        let lo = self.eval_const(lo, &self.cur_env)?;
                        let hi = self.eval_const(hi, &self.cur_env)?;
                        u32::try_from(lo.abs_diff(hi) + 1).ok()?
                    }
                    _ => match self.lower_scalar_env(index, env) {
                        Expr::Const(c) => c as u32,
                        _ => self.eval_const(index, &self.cur_env)? as u32,
                    },
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
        // `natural(x)` and `integer(n)` keep the word and check its range;
        // `real(n)` of a natural reads it unsigned.
        let target = match callee {
            ast::Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.as_str()),
            _ => None,
        };
        let from_natural = self.operand_type_name(arg).as_deref() == Some("natural");
        let natural_conversion = match target {
            Some("natural") if self.is_real_expr(&v) => Some(ProcessUnaryOp::RealToNatural),
            Some("natural") if !from_natural => Some(ProcessUnaryOp::IntegerToNatural),
            Some("integer") if from_natural => Some(ProcessUnaryOp::NaturalToInteger),
            Some("real") if from_natural => Some(ProcessUnaryOp::NaturalToReal),
            _ => None,
        };
        if let Some(operation) = natural_conversion {
            return Some(self.source_values.borrow_mut().unary(
                operation,
                &v,
                ast::expr_span(callee),
            ));
        }
        if target == Some("natural") {
            return Some(v);
        }
        // `real(x)` for a type with `impl From<S> for real` (`real(ufixed)`)
        // is that conversion, not the kernel reading of its word.
        if to_real {
            if let Some(value) = args
                .first()
                .and_then(|arg| self.real_from(callee, arg, env))
            {
                return Some(value);
            }
        }
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
            && self.operand_type_name(arg).is_some_and(|family| {
                self.array_families.contains(&family) || self.enum_variants.contains_key(&family)
            });
        Some(match target_w {
            Some(w) if w > 0 && w != 64 => self.source_slice(&v, w - 1, 0, ast::expr_span(callee)),
            None if packed_operand => self.source_slice(&v, 63, 0, ast::expr_span(callee)),
            _ => v,
        })
    }

    /// The type name an operand contributes to operator-impl lookup: a local's
    /// declared enum/struct, a suffix literal's target type, an enum variant's
    /// enum, or `integer` for a bare numeric literal.
    pub(super) fn operand_type_name(&self, e: &ast::Expr) -> Option<String> {
        if let Some(crate::types::Ty::Array {
            family: Some(family),
            ..
        }) = self.promotion_of(e)
        {
            return Some(self.free_fns.canonical_type_key(family));
        }
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
            // A struct literal names its type: `Rect { .w = w }.width()` and
            // `Complex { .. } + Complex { .. }` found no impl (#253).
            ast::Expr::Construct { ty: Some(ty), .. } => self.free_fns.type_head_key(ty),
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
            // `x'old` is an `x`: a method on it (`a'old.to_x01()`) found no
            // impl, and the call lowered to nothing.
            ast::Expr::SysAttr { base, attr, .. } if attr.text == "old" => {
                self.operand_type_name(base)
            }
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
            // A custom operator's result is its impl's: the checker recorded
            // it. Without it a nested `(a xor b) nand c` had no family.
            ast::Expr::Binary {
                op: ast::BinOp::Custom { .. },
                span,
                ..
            } => match self.expr_types.get(span) {
                Some(crate::types::Ty::Array {
                    family: Some(family),
                    ..
                }) => Some(self.free_fns.canonical_type_key(family)),
                _ => None,
            },
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
            ast::Expr::Index { base, index, .. } => {
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
                if matches!(
                    index.as_ref(),
                    ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
                ) && self.array_families.contains(&family)
                {
                    return Some(family);
                }
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
        for stmt in stmts {
            match stmt {
                ast::Stmt::Use(_) => {}
                ast::Stmt::Let(l) => {
                    let value = l.value.as_ref()?;
                    self.register_local_type(l, value);
                    let layout =
                        l.ty.as_ref()
                            .map(|ty| self.source_layout(ty, &self.attribute_env(env)))
                            .filter(|layout| layout.bit_width().is_some())
                            .or_else(|| self.source_operand_layout(value, env));
                    let literal = matches!(
                        value,
                        ast::Expr::StrLit { .. } | ast::Expr::BitStrLit { .. }
                    );
                    let value_ir = match &layout {
                        Some(layout)
                            if matches!(
                                layout.kind,
                                LayoutKind::Array { .. } | LayoutKind::Struct { .. }
                            ) || literal && matches!(layout.kind, LayoutKind::Packed { .. }) =>
                        {
                            self.lower_shaped_source(value, env, layout)
                        }
                        _ => self.lower_val_env(value, env),
                    };
                    let mut lowered = self.bind_source_value(
                        value_ir,
                        l.span,
                        self.expr_types.get(&ast::expr_span(value)).cloned(),
                    );
                    self.source_inline_local(&l.name.text, l.span, layout.clone(), &mut lowered);
                    if let Some(layout) = layout {
                        self.bind_source_shape(l.name.text.clone(), layout);
                    }
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
                    let layout = self.source_bound_layout(name);
                    let literal = matches!(
                        value,
                        ast::Expr::StrLit { .. } | ast::Expr::BitStrLit { .. }
                    );
                    let value_ir = match &layout {
                        Some(layout)
                            if matches!(
                                layout.kind,
                                LayoutKind::Array { .. } | LayoutKind::Struct { .. }
                            ) || literal && matches!(layout.kind, LayoutKind::Packed { .. }) =>
                        {
                            self.lower_shaped_source(value, env, layout)
                        }
                        _ => self.lower_val_env(value, env),
                    };
                    let mut lowered = self.bind_source_value(
                        value_ir,
                        *span,
                        self.expr_types.get(&ast::expr_span(value)).cloned(),
                    );
                    self.source_inline_local(name, *span, layout, &mut lowered);
                    env.insert(name.clone(), lowered);
                }
                ast::Stmt::Assign {
                    target: target @ ast::Expr::Field { .. },
                    value,
                    after: None,
                    span,
                    ..
                } => {
                    let path = expr_path(target)?;
                    let (root, suffix) = path.split_once('.')?;
                    let Some(Val::Fields(mut fields)) = env.get(root).cloned() else {
                        return None;
                    };
                    let field_layout = self.source_operand_layout(target, env);
                    let literal = matches!(
                        value,
                        ast::Expr::StrLit { .. } | ast::Expr::BitStrLit { .. }
                    );
                    let lowered_val = match &field_layout {
                        Some(layout)
                            if matches!(
                                layout.kind,
                                LayoutKind::Array { .. } | LayoutKind::Struct { .. }
                            ) || literal && matches!(layout.kind, LayoutKind::Packed { .. }) =>
                        {
                            self.lower_shaped_source(value, env, layout)
                        }
                        _ => self.lower_val_env(value, env),
                    };
                    let mut lowered = self.bind_source_value(
                        lowered_val,
                        *span,
                        self.expr_types.get(&ast::expr_span(value)).cloned(),
                    );
                    if let Some(layout) = field_layout {
                        if let Val::Scalar(scalar) = &mut lowered {
                            if let Some(width) = layout
                                .packed_width()
                                .filter(|&w| w > 0 && w != 64 && !literal)
                            {
                                *scalar = self.source_slice(scalar, width - 1, 0, *span);
                            }
                        }
                        lowered = self.bind_value_layout(lowered, layout, *span);
                    }
                    match lowered {
                        Val::Scalar(scalar) => {
                            let (_, slot) = fields.iter_mut().find(|(k, _)| k == suffix)?;
                            *slot = scalar;
                        }
                        Val::Fields(subfields) => {
                            for (subk, subv) in subfields {
                                let key = if subk.starts_with('[') {
                                    format!("{suffix}{subk}")
                                } else {
                                    format!("{suffix}.{subk}")
                                };
                                let (_, slot) = fields.iter_mut().find(|(k, _)| *k == key)?;
                                *slot = subv;
                            }
                        }
                    }
                    env.insert(root.to_owned(), Val::Fields(fields));
                }
                // `r[k] = v` or `r[hi..lo] = v` with constant bounds on a packed
                // local or struct field: the vector with those bits replaced.
                ast::Stmt::Assign {
                    target: ast::Expr::Index { base, index, .. },
                    value,
                    after: None,
                    span,
                    ..
                } => {
                    let base_path = expr_path(base)?;
                    let (name, suffix) = match base_path.split_once('.') {
                        Some((root, suffix)) => (root, Some(suffix)),
                        None => (base_path.as_str(), None),
                    };
                    if suffix.is_none() {
                        if let Some(Val::Fields(fields)) = env.get(name).cloned() {
                            let layout = self.source_bound_layout(name)?;
                            let updated =
                                self.write_elements(fields, &layout, index, value, env)?;
                            env.insert(name.to_owned(), Val::Fields(updated));
                            continue;
                        }
                    }
                    let (current, layout) = match suffix {
                        None => {
                            let Some(Val::Scalar(current)) = env.get(name).cloned() else {
                                return None;
                            };
                            (current, self.source_bound_layout(name)?)
                        }
                        Some(suffix) => {
                            let Some(Val::Fields(fields)) = env.get(name) else {
                                return None;
                            };
                            let (_, current) = fields.iter().find(|(k, _)| k == suffix)?;
                            (current.clone(), self.source_operand_layout(base, env)?)
                        }
                    };
                    if !matches!(layout.kind, LayoutKind::Packed { .. }) {
                        return None;
                    }
                    let store_packed = |env: &mut HashMap<String, Val>,
                                        merged: Expr|
                     -> Option<()> {
                        match suffix {
                            None => {
                                env.insert(name.to_owned(), Val::Scalar(merged));
                            }
                            Some(suffix) => {
                                let Some(Val::Fields(mut fields)) = env.get(name).cloned() else {
                                    return None;
                                };
                                let (_, slot) = fields.iter_mut().find(|(k, _)| k == suffix)?;
                                *slot = merged;
                                env.insert(name.to_owned(), Val::Fields(fields));
                            }
                        }
                        Some(())
                    };
                    if let Some((left, right)) =
                        self.source_packed_slice_bounds(&layout, index, env)
                    {
                        let replacement = self.lower_scalar_env(value, env);
                        let merged = self.merge_slice(
                            current,
                            left.max(right),
                            left.min(right),
                            replacement,
                            layout.packed_width()?,
                            *span,
                        );
                        let merged = self.bind_source_expression(merged, *span);
                        store_packed(env, merged)?;
                        continue;
                    }
                    let bit = self.constant_packed_index(&layout, index, env)?;
                    // The element holds whatever value is written, `'X'` as
                    // much as `'1'`: the value plane takes its bit, and the
                    // companion its discriminant (`merge_meta`).
                    let literal = match value {
                        ast::Expr::CharLit { ch, .. } => {
                            Some(self.char_disc(*ch, DEFAULT_LOGIC_TYPE)?)
                        }
                        _ => None,
                    };
                    let encoding = self.logic_encoding(DEFAULT_LOGIC_TYPE);
                    let element = match (literal, encoding) {
                        (Some(discriminant), Some(encoding)) => Expr::Const(u64::from(
                            encoding
                                .value_bits
                                .get(&discriminant)
                                .copied()
                                .unwrap_or(false),
                        )),
                        _ => self.lower_scalar_env(value, env),
                    };
                    let metavalue = literal.is_some_and(|discriminant| {
                        encoding.is_some_and(|encoding| !encoding.binary.contains(&discriminant))
                    });
                    // A logic enum read is its discriminant; its low bit is the
                    // value plane's for '0'/'1', and the nibble decides the rest.
                    let discriminant = literal.is_none() && self.is_logic_scalar(value);
                    let raw = element.clone();
                    let element = if discriminant {
                        self.logic_value_bit(&element, *span)
                    } else {
                        element
                    };
                    let merged = self.merge_slice(
                        current.clone(),
                        bit,
                        bit,
                        element.clone(),
                        layout.packed_width()?,
                        *span,
                    );
                    let merged = self.bind_source_expression(merged, *span);
                    if let Expr::Canonical { value: id, .. } = &merged {
                        self.source_values.borrow_mut().set_merge_meta(
                            *id,
                            source_values::MetaMerge {
                                base: current,
                                element: if discriminant { raw } else { element },
                                literal,
                                metavalue,
                                discriminant,
                                bit,
                            },
                        );
                    }
                    store_packed(env, merged)?;
                }
                ast::Stmt::Expr(ast::Expr::Call { callee, args, .. }) => {
                    let ast::Expr::Field { base, field, .. } = callee.as_ref() else {
                        return None;
                    };
                    let ast::Expr::Path(path) = base.as_ref() else {
                        return None;
                    };
                    let [segment] = path.segments.as_slice() else {
                        return None;
                    };
                    let recv_name = &segment.text;
                    let recv_val = env.get(recv_name)?.clone();
                    let recv_layout = self.source_operand_layout(base, env);
                    let ty = self
                        .operand_type_name(base)
                        .or_else(|| recv_layout.as_ref().and_then(Self::source_layout_family))?;
                    let input = args.first().and_then(|arg| match arg {
                        ast::Expr::Construct { ty: Some(ty), .. }
                            if type_head_name(ty) == Some("Range") =>
                        {
                            Some("Range".to_string())
                        }
                        ast::Expr::StrLit { .. }
                        | ast::Expr::BitStrLit { .. }
                        | ast::Expr::CharLit { .. } => None,
                        _ => self.operand_type_name(arg),
                    });
                    let f = self.find_method(&ty, &field.text, input.as_deref())?;
                    let body = f.body.as_ref()?;
                    let mut fenv: HashMap<String, Val> = HashMap::new();
                    let mut shapes = HashMap::new();
                    if let Some(layout) = recv_layout {
                        shapes.insert("self".to_owned(), layout);
                    }
                    fenv.insert(
                        "self".to_string(),
                        self.bind_source_value(recv_val, ast::expr_span(base), None),
                    );
                    fenv.insert(
                        "self::length".to_string(),
                        Val::Scalar(Expr::Const(self.ast_width(base) as u64)),
                    );
                    self.bind_range_attrs(&mut fenv, "self", base, env);
                    let mut saved: Vec<(String, Option<String>)> = Vec::new();
                    let mut saved_widths: Vec<(String, Option<u32>)> = Vec::new();
                    let mut added_integers: Vec<String> = Vec::new();
                    let self_prev = self
                        .param_types
                        .borrow_mut()
                        .insert("self".to_string(), ty.clone());
                    saved.push(("self".to_string(), self_prev));
                    let receiver_width = self.ast_width(base);
                    for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
                        if let Some(n) = &p.name {
                            let a = &self.as_struct_literal(p.ty.as_ref(), a);
                            let (argument, layout) =
                                self.lower_source_argument(a, p.ty.as_ref(), env);
                            if let Some(layout) = layout {
                                shapes.insert(n.text.clone(), layout);
                            }
                            fenv.insert(
                                n.text.clone(),
                                self.bind_source_value(argument, ast::expr_span(a), None),
                            );
                            let w = self.literal_aware_width(a, receiver_width);
                            fenv.insert(
                                format!("{}::length", n.text),
                                Val::Scalar(Expr::Const(w as u64)),
                            );
                            self.bind_range_attrs(&mut fenv, &n.text, a, env);
                            let declared = p.ty.as_ref().and_then(type_head_name).map(|head| {
                                if head == "Self" {
                                    ty.clone()
                                } else {
                                    head.to_owned()
                                }
                            });
                            if let Some(mut fam) = self.operand_type_name(a).or(declared) {
                                if fam == "integer"
                                    && p.ty.as_ref().and_then(type_head_name) != Some("integer")
                                {
                                    fam = ty.clone();
                                }
                                let prev =
                                    self.param_types.borrow_mut().insert(n.text.clone(), fam);
                                saved.push((n.text.clone(), prev));
                            }
                            if p.ty.as_ref().and_then(type_head_name) == Some("integer")
                                && self.param_integers.borrow_mut().insert(n.text.clone())
                            {
                                added_integers.push(n.text.clone());
                            }
                            if w > 0 {
                                saved_widths.push((
                                    n.text.clone(),
                                    self.param_widths.borrow_mut().insert(n.text.clone(), w),
                                ));
                            }
                        }
                    }
                    if receiver_width > 0 {
                        saved_widths.push((
                            "self".to_string(),
                            self.param_widths
                                .borrow_mut()
                                .insert("self".to_string(), receiver_width),
                        ));
                    }
                    let _shapes = self.source_shape_scope(shapes, None);
                    let mark = self.local_type_saves.borrow().len();
                    let result = self.inline_effects(&body.stmts, &mut fenv);
                    self.restore_local_types(mark);
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
                    result?;
                    env.insert(recv_name.clone(), fenv.remove("self")?);
                }
                ast::Stmt::For {
                    var,
                    range: ast::Expr::Range { lo, hi, .. },
                    body,
                    ..
                } => {
                    let bound = |e: &ast::Expr| {
                        let value = self.constant_expr(&self.lower_scalar_env(e, env));
                        if value.is_none() {
                            self.procedural_loop.set(Some(ast::expr_span(e)));
                        }
                        value
                    };
                    let first = bound(lo)?;
                    let last = bound(hi)?;
                    if first.abs_diff(last) >= MAX_ITERATIONS {
                        self.procedural_loop.set(Some(ast::expr_span(hi)));
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
                // `for x in xs`: one body per element, with `x` read as
                // `xs[label]`. Only the index form lowered in a hardware
                // function; this one left the call unlowered.
                ast::Stmt::For {
                    var,
                    range: iterable,
                    body,
                    ..
                } => {
                    for body in self.element_loop_bodies(var, iterable, body, env)? {
                        self.inline_scoped(&body, env)?;
                    }
                }
                ast::Stmt::If(iff) if !if_returns(iff) => {
                    self.inline_merged_if(iff, env)?;
                }
                _ => return None,
            }
        }
        Some(())
    }

    /// The bodies of `for var in iterable` over an array or vector's
    /// elements, in its declared order, each reading `var` as
    /// `iterable[label]`.
    fn element_loop_bodies(
        &self,
        var: &ast::Ident,
        iterable: &ast::Expr,
        body: &ast::Block,
        env: &HashMap<String, Val>,
    ) -> Option<Vec<Vec<ast::Stmt>>> {
        let range = self.source_operand_layout(iterable, env)?.index_range()?;
        let labels = crate::ir::loop_range(range.left, range.right);
        if labels.len() as u64 >= MAX_ITERATIONS {
            return None;
        }
        Some(
            labels
                .into_iter()
                .map(|label| {
                    let element = ast::Expr::Index {
                        base: Box::new(iterable.clone()),
                        index: Box::new(ast::Expr::Int {
                            text: label.to_string(),
                            span: var.span,
                        }),
                        span: var.span,
                    };
                    let map = HashMap::from([(var.text.clone(), element)]);
                    body.stmts
                        .iter()
                        .map(|s| subst_stmt_paths(s, &map))
                        .collect()
                })
                .collect(),
        )
    }

    /// `r[k] = v` or `r[hi..lo] = v` on an array local, at constant indices:
    /// its elements with those replaced, the right-hand side's paired in
    /// written order.
    fn write_elements(
        &self,
        mut fields: Vec<(String, Expr)>,
        layout: &SourceLayout,
        index: &ast::Expr,
        value: &ast::Expr,
        env: &HashMap<String, Val>,
    ) -> Option<Vec<(String, Expr)>> {
        let LayoutKind::Array {
            range: Some(range), ..
        } = &layout.kind
        else {
            return None;
        };
        let (low, high) = (range.left.min(range.right), range.left.max(range.right));
        let constant = |bound: &ast::Expr| {
            self.constant_expr(&self.lower_scalar_env(bound, env))
                .filter(|label| (low..=high).contains(label))
        };
        let mut set = |label: i64, element: Expr| {
            let key = format!("[{label}]");
            let slot = fields.iter_mut().find(|(name, _)| *name == key)?;
            slot.1 = element;
            Some(())
        };
        if let ast::Expr::Range { lo, hi, .. } = index {
            let targets = loop_range(constant(lo)?, constant(hi)?);
            let Val::Fields(source) = self.lower_val_env(value, env) else {
                return None;
            };
            let source_layout = self.source_operand_layout(value, env)?;
            let source_range = source_layout.index_range()?;
            let sources = loop_range(source_range.left, source_range.right);
            if sources.len() != targets.len() {
                return None;
            }
            let source = source.into_iter().collect::<HashMap<_, _>>();
            for (target, from) in targets.into_iter().zip(sources) {
                set(target, source.get(&format!("[{from}]"))?.clone())?;
            }
        } else {
            let element = self.lower_scalar_env(value, env);
            set(constant(index)?, element)?;
        }
        Some(fields)
    }

    /// A block in its own scope: its `let`s end with it, its assignments to
    /// outer bindings stay.
    fn inline_scoped(&self, stmts: &[ast::Stmt], env: &mut HashMap<String, Val>) -> Option<()> {
        let mut inner = env.clone();
        let mark = self.local_type_saves.borrow().len();
        let done = self.inline_effects(stmts, &mut inner);
        self.restore_local_types(mark);
        done?;
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
    /// `cur_env` with the constant attributes an inlined body has bound
    /// (`value::length`), so a type can size itself by an argument:
    /// `let r: unsigned[value'length]`.
    pub(super) fn attribute_env(&self, env: &HashMap<String, Val>) -> HashMap<String, i64> {
        let mut out = self.cur_env.clone();
        for (name, value) in env {
            if let (true, Val::Scalar(value)) = (name.contains("::"), value) {
                if let Some(constant) = self.constant_expr(value) {
                    out.insert(name.clone(), constant);
                }
            }
        }
        out
    }

    pub(super) fn constant_expr(&self, expression: &Expr) -> Option<i64> {
        match expression {
            Expr::Const(value) => Some(*value as i64),
            Expr::Canonical { value, .. } => self.constant_expr(&self.source_node(*value)),
            // A negative bound (`format'low` of `float<32, 23>`) is bound as
            // its negation.
            Expr::Unary { op: UnOp::Neg, rhs } => self.constant_expr(rhs)?.checked_neg(),
            Expr::Binary { op, lhs, rhs } => {
                let (left, right) = (self.constant_expr(lhs)?, self.constant_expr(rhs)?);
                match op {
                    BinOp::Add | BinOp::SAdd => left.checked_add(right),
                    BinOp::Sub | BinOp::SSub => left.checked_sub(right),
                    BinOp::Mul | BinOp::SMul => left.checked_mul(right),
                    BinOp::Div | BinOp::SDiv => left.checked_div(right),
                    BinOp::Shl => left.checked_shl(u32::try_from(right).ok()?),
                    BinOp::Shr | BinOp::AShr => left.checked_shr(u32::try_from(right).ok()?),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Type a function's `let` like a parameter: its family and width for
    /// operator dispatch, and whether it is a kernel integer (signed ops).
    fn register_local_type(&self, l: &ast::LetDecl, value: &ast::Expr) {
        let name = l.name.text.clone();
        let head = l.ty.as_ref().and_then(type_head_name);
        let family = self.operand_type_name(value).or_else(|| {
            head.filter(|head| !matches!(*head, "integer" | "real" | "Char"))
                .map(str::to_owned)
        });
        let width = Some(self.ast_width(value)).filter(|width| *width > 0);
        let integer = head == Some("integer")
            || self.declares_kernel_integer(value)
            || matches!(
                self.expr_types.get(&ast::expr_span(value)),
                Some(crate::types::Ty::Integer)
            );
        let family = match family {
            Some(family) => self.param_types.borrow_mut().insert(name.clone(), family),
            None => self.param_types.borrow_mut().remove(&name),
        };
        let width = match width {
            Some(width) => self.param_widths.borrow_mut().insert(name.clone(), width),
            None => self.param_widths.borrow_mut().remove(&name),
        };
        let was_integer = if integer {
            !self.param_integers.borrow_mut().insert(name.clone())
        } else {
            self.param_integers.borrow_mut().remove(&name)
        };
        self.local_type_saves
            .borrow_mut()
            .push((name, family, width, was_integer));
    }

    /// Undo the `let` typings made since `mark`.
    fn restore_local_types(&self, mark: usize) {
        let mut saves = self.local_type_saves.borrow_mut();
        while saves.len() > mark {
            let (name, family, width, integer) = saves.pop().expect("above the mark");
            match family {
                Some(family) => self.param_types.borrow_mut().insert(name.clone(), family),
                None => self.param_types.borrow_mut().remove(&name),
            };
            match width {
                Some(width) => self.param_widths.borrow_mut().insert(name.clone(), width),
                None => self.param_widths.borrow_mut().remove(&name),
            };
            if integer {
                self.param_integers.borrow_mut().insert(name);
            } else {
                self.param_integers.borrow_mut().remove(&name);
            }
        }
    }

    /// The value a straight-line `return`/`if-else` block produces, or `None`
    /// if the block has statements the inliner cannot express as a value.
    pub(super) fn inline_block(
        &self,
        stmts: &[ast::Stmt],
        env: &HashMap<String, Val>,
    ) -> Option<(Val, crate::diag::Span)> {
        let mark = self.local_type_saves.borrow().len();
        let value = self.inline_block_body(stmts, env);
        self.restore_local_types(mark);
        value
    }

    fn inline_block_body(
        &self,
        mut stmts: &[ast::Stmt],
        env: &HashMap<String, Val>,
    ) -> Option<(Val, crate::diag::Span)> {
        let extent = stmts.first().zip(stmts.last()).map(|(first, last)| {
            let first = ast::stmt_span(first);
            let last = ast::stmt_span(last);
            crate::diag::Span::new(first.file, first.start..last.end)
        });
        let _scope = self.source_lexical_scope(extent);
        // A long straight-line let chain is not recursion. Clone the lexical
        // environment at most once, then update its compact value bindings.
        let mut scoped = std::borrow::Cow::Borrowed(env);
        loop {
            while let [ast::Stmt::Let(l), rest @ ..] = stmts {
                let value = l.value.as_ref()?;
                self.register_local_type(l, value);
                // A width-generic declaration (`let r: unsigned = v;`) has the
                // initializer's shape: its own has no width.
                let layout =
                    l.ty.as_ref()
                        .map(|ty| self.source_layout(ty, &self.attribute_env(scoped.as_ref())))
                        .filter(|layout| layout.bit_width().is_some())
                        .or_else(|| self.source_operand_layout(value, scoped.as_ref()));
                // A vector literal is shaped too, which keeps its metavalue
                // companion: `let p: unsigned[4] = "00--";` holds `'-'`s.
                let literal = matches!(
                    value,
                    ast::Expr::StrLit { .. } | ast::Expr::BitStrLit { .. }
                );
                let value_ir = match &layout {
                    Some(layout)
                        if matches!(
                            layout.kind,
                            LayoutKind::Array { .. } | LayoutKind::Struct { .. }
                        ) || literal && matches!(layout.kind, LayoutKind::Packed { .. }) =>
                    {
                        self.lower_shaped_source(value, scoped.as_ref(), layout)
                    }
                    _ => self.lower_val_env(value, scoped.as_ref()),
                };
                let mut lowered = self.bind_source_value(
                    value_ir,
                    l.span,
                    self.expr_types.get(&ast::expr_span(value)).cloned(),
                );
                self.source_inline_local(&l.name.text, l.span, layout.clone(), &mut lowered);
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
                // A loop that returns (`for k in 0..7 { if v[k] == '1' {
                // return k; } }`) unrolls in place, followed by what comes
                // after it, so each `return` is one the walk below handles.
                [ast::Stmt::For {
                    var,
                    range: ast::Expr::Range { lo, hi, .. },
                    body,
                    ..
                }, rest @ ..]
                    if statements_return(&body.stmts) =>
                {
                    let bound = |e: &ast::Expr| {
                        self.constant_expr(&self.lower_scalar_env(e, scoped.as_ref()))
                    };
                    let (first, last) = (bound(lo)?, bound(hi)?);
                    if first.abs_diff(last) >= MAX_ITERATIONS {
                        return None;
                    }
                    let mut unrolled = Vec::new();
                    for index in crate::ir::loop_range(first, last) {
                        if index < 0 {
                            return None;
                        }
                        let literal = ast::Expr::Int {
                            text: index.to_string(),
                            span: var.span,
                        };
                        let map = HashMap::from([(var.text.clone(), literal)]);
                        unrolled.extend(body.stmts.iter().map(|s| subst_stmt_paths(s, &map)));
                    }
                    unrolled.extend(rest.iter().cloned());
                    return self.inline_block_body(&unrolled, scoped.as_ref());
                }
                [ast::Stmt::For {
                    var,
                    range: iterable,
                    body,
                    ..
                }, rest @ ..]
                    if statements_return(&body.stmts)
                        && !matches!(iterable, ast::Expr::Range { .. }) =>
                {
                    let mut unrolled: Vec<ast::Stmt> = self
                        .element_loop_bodies(var, iterable, body, scoped.as_ref())?
                        .into_iter()
                        .flatten()
                        .collect();
                    unrolled.extend(rest.iter().cloned());
                    return self.inline_block_body(&unrolled, scoped.as_ref());
                }
                [statement @ (ast::Stmt::Assign { after: None, .. }
                | ast::Stmt::For { .. }
                | ast::Stmt::Expr(ast::Expr::Call { .. })), rest @ ..] => {
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
                let literal = matches!(v, ast::Expr::StrLit { .. } | ast::Expr::BitStrLit { .. });
                let value = match self.source_return_layout() {
                    Some(layout)
                        if matches!(
                            layout.kind,
                            LayoutKind::Array { .. } | LayoutKind::Struct { .. }
                        ) || literal && matches!(layout.kind, LayoutKind::Packed { .. }) =>
                    {
                        self.lower_shaped_source(v, env, &layout)
                    }
                    _ => self.lower_val_env(v, env),
                };
                Some((
                    self.bind_source_value(value, ast::expr_span(v), None),
                    ast::expr_span(v),
                ))
            }
            [ast::Stmt::If(iff), rest @ ..] => {
                let cond = self.bind_source_expression(
                    self.lower_scalar_env(&iff.cond, env),
                    ast::expr_span(&iff.cond),
                );
                // A branch that can fall through continues with the statements
                // after the if: one that returns only on some paths (`if a {
                // if b { return x; } acc = acc + 1; }`) found no `return` of
                // its own and left the call unlowered. One that always returns
                // stays on its own, which keeps its lexical scope its own.
                // ponytail: a fall-through branch lowers `rest` once more, so
                // a chain of k such ifs lowers the tail 2^k times.
                let continued = |branch: &[ast::Stmt]| {
                    if statements_always_return(branch) {
                        return self.inline_block(branch, env);
                    }
                    let mut all = branch.to_vec();
                    all.extend(rest.iter().cloned());
                    self.inline_block(&all, env)
                };
                let els = || match &iff.else_ {
                    Some(e) => match e.as_ref() {
                        ast::ElseBranch::Block(b) => continued(&b.stmts),
                        ast::ElseBranch::If(i) => continued(&[ast::Stmt::If(i.clone())]),
                    },
                    None => self.inline_block(rest, env),
                };
                // A condition on formats (`Self'low <= value'low`) is
                // constant: only the branch it takes is built.
                let constant = match &cond {
                    Expr::Const(c) => Some(*c != 0),
                    Expr::Canonical { value, .. } => crate::ir::arena_constant_integer(
                        *value,
                        &self.source_values.borrow().ir.values,
                    )
                    .map(|c| c != 0),
                    _ => None,
                };
                match constant {
                    Some(true) => return continued(&iff.then.stmts),
                    Some(false) => return els(),
                    None => {}
                }
                let then = continued(&iff.then.stmts)?;
                let els = els()?;
                Some(self.source_inline_select(cond, then, els, iff.span))
            }
            // A `match` whose arms return is the same shape as an `if`
            // chain, and only the `if` form was handled — the two share
            // `MatchArm` and have drifted apart repeatedly. First-match
            // priority comes from folding the arms in reverse.
            [ast::Stmt::Match(m), rest @ ..] => {
                let scrut = self.bind_source_expression(
                    if self.is_tuple_scrutinee(&m.scrutinee) {
                        Expr::Const(0)
                    } else {
                        self.lower_scalar_env(&m.scrutinee, env)
                    },
                    ast::expr_span(&m.scrutinee),
                );
                // What the body yields when no arm returns.
                let after = self.inline_block(rest, env);
                let mut acc = after.clone();
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
                            (Some(cond), Some(otherwise)) => {
                                self.source_inline_select(cond, value, otherwise, arm.span)
                            }
                        },
                    );
                }
                acc
            }
            _ => None,
        }
    }
}

/// The most iterations a hardware function loop unrolls.
// ponytail: a fixed unroll cap; a longer loop leaves the call unlowered.
const MAX_ITERATIONS: u64 = 4096;

/// Whether a `return` sits anywhere in these statements.
fn statements_return(statements: &[ast::Stmt]) -> bool {
    statements.iter().any(|statement| match statement {
        ast::Stmt::Return { .. } => true,
        ast::Stmt::If(statement) => if_returns(statement),
        ast::Stmt::Match(statement) => statement
            .arms
            .iter()
            .any(|arm| statements_return(&arm.body.stmts)),
        ast::Stmt::For { body, .. } => statements_return(&body.stmts),
        _ => false,
    })
}

/// Whether every path through these statements ends in a `return`.
fn statements_always_return(statements: &[ast::Stmt]) -> bool {
    statements.iter().any(|statement| match statement {
        ast::Stmt::Return { .. } => true,
        ast::Stmt::If(statement) => if_always_returns(statement),
        ast::Stmt::Match(statement) => statement
            .arms
            .iter()
            .all(|arm| statements_always_return(&arm.body.stmts)),
        _ => false,
    })
}

fn if_always_returns(statement: &ast::IfStmt) -> bool {
    statements_always_return(&statement.then.stmts)
        && match statement.else_.as_deref() {
            Some(ast::ElseBranch::Block(block)) => statements_always_return(&block.stmts),
            Some(ast::ElseBranch::If(inner)) => if_always_returns(inner),
            None => false,
        }
}

/// Whether a `return` sits anywhere inside this `if`.
fn if_returns(statement: &ast::IfStmt) -> bool {
    let any = statements_return;
    any(&statement.then.stmts)
        || match statement.else_.as_deref() {
            Some(ast::ElseBranch::Block(block)) => any(&block.stmts),
            Some(ast::ElseBranch::If(inner)) => if_returns(inner),
            None => false,
        }
}
