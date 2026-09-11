//! Inlined values, aggregates, attributes, and element-wise expressions.

use super::*;

impl<'a> Lowering<'a> {
    /// Lower an expression to a [`Val`], with fn parameters substituted from
    /// `env`. Struct-typed locals and struct literals become per-field values.
    /// `not` over a bit vector: every bit inverted, lowered as `mask - x` so
    /// no engine needs width knowledge. `None` when the operand is not a
    /// vector reference — a 1-bit operand keeps the boolean form (identical
    /// either way), and a compound expression or enum-typed signal has its
    /// own `not`.
    pub(super) fn vector_not(
        &self,
        rhs: &ast::Expr,
        lower: impl Fn(&ast::Expr) -> Expr,
    ) -> Option<Expr> {
        let is_vector_ref = match rhs {
            // A slice is always a bit vector.
            ast::Expr::Index { base, index, .. } if self.slice_bounds(base, index).is_some() => {
                true
            }
            ast::Expr::Path(_) | ast::Expr::Field { .. } | ast::Expr::Index { .. } => {
                self.block_local_type(rhs)
                    .and_then(|ty| self.free_fns.type_head_key(&ty))
                    .is_some_and(|family| self.array_families.contains(&family))
                    || expr_path(rhs)
                        .and_then(|p| self.locals.get(&p))
                        .map(|&id| self.out.signals[id.0 as usize].enum_type.is_none())
                        .unwrap_or(false)
            }
            _ => false,
        };
        if !is_vector_ref {
            return None;
        }
        let w = self.ast_width(rhs);
        (w > 1 && w <= 64).then(|| {
            let mask = if w == 64 { u64::MAX } else { (1u64 << w) - 1 };
            // `x xor all-ones`, not `all-ones - x`. The two agree bit for bit
            // on two-valued data, but a subtraction makes the metavalue
            // companion poison the whole vector, so `not "0000X100"` came back
            // all `'X'` where `std_logic_1164` inverts per element and leaves
            // `1111X011`.
            Expr::Binary {
                op: BinOp::Xor,
                lhs: Box::new(lower(rhs)),
                rhs: Box::new(Expr::Const(mask)),
            }
        })
    }

    /// Lower an expression to a value in `env`, which may be an aggregate of
    /// leaves rather than a single expression.
    pub(super) fn lower_val_env(&self, e: &ast::Expr, env: &HashMap<String, Val>) -> Val {
        match e {
            // `self::length` inside an operator-impl body: the bound operand's
            // width (inline_op stashes it under the "param::attr" key).
            ast::Expr::SysAttr { base, attr, .. } => {
                if let Some(v) =
                    expr_path(base).and_then(|p| env.get(&format!("{p}::{}", attr.text)))
                {
                    return v.clone();
                }
                Val::Scalar(self.lower_expr(e))
            }
            ast::Expr::IfExpr {
                cond, then, els, ..
            } => {
                let c = self.lower_scalar_env(cond, env);
                select_val(
                    c,
                    self.lower_val_env(then, env),
                    self.lower_val_env(els, env),
                )
            }
            // A match *expression* whose arms are struct values, folded per
            // field the way `IfExpr` above is. Without this it fell through to
            // the scalar lowering, which builds one `Select` chain over whole
            // values and has nowhere to put a struct — so a decoder written as
            // `c = match op { .. => Ctrl { .. } }` drove nothing at all, and
            // said so only as "never driven" on each field. The equivalent
            // `if`-expression and the statement form both worked.
            ast::Expr::Match {
                scrutinee, arms, ..
            } => self.lower_match_val(scrutinee, arms, env),
            ast::Expr::Call { callee, args, .. } => {
                // `T()` — the nullary constructor — resolves to the type's
                // default (a struct yields per-field values) before any
                // free-fn/conversion lookup.
                if let Some(v) = self.lower_new(callee, args) {
                    return v;
                }
                match self
                    .lower_conversion(callee, args, env)
                    .map(Val::Scalar)
                    .or_else(|| self.lower_free_call(callee, args, env))
                {
                    Some(v) => v,
                    None => match self
                        .lower_method_call(callee, args, env)
                        .or_else(|| self.lower_from(callee, args, env))
                    {
                        Some(v) => v,
                        None => Val::Scalar(self.lower_expr(e)),
                    },
                }
            }
            ast::Expr::Path(p) if p.segments.len() == 1 => {
                let name = &p.segments[0].text;
                if let Some(v) = env.get(name) {
                    return v.clone();
                }
                if let Some(value) = self.block_local_value(e) {
                    return value;
                }
                if let Some(v) = self.aggregate_signal_val(name) {
                    return v;
                }
                // A struct constant read whole (`p = K`). Its fields live in
                // the constant table under dotted paths, so it has no signal
                // and no scalar form — as a value it is the fields themselves,
                // which is what an assignment expands per leaf.
                if let Some(fields) = self.const_struct_value(name) {
                    return Val::Fields(fields);
                }
                Val::Scalar(self.lower_expr(e))
            }
            // `self.re` where `self` is an env-bound struct value.
            ast::Expr::Field { base, field, .. } => {
                if let Some(value) = self.block_local_value(e) {
                    return value;
                }
                if let ast::Expr::Path(p) = base.as_ref() {
                    if p.segments.len() == 1 {
                        if let Some(Val::Fields(fs)) = env.get(&p.segments[0].text) {
                            let v = fs
                                .iter()
                                .find(|(n, _)| *n == field.text)
                                .map(|(_, e)| e.clone())
                                .unwrap_or(Expr::Unknown);
                            return Val::Scalar(v);
                        }
                    }
                }
                if let Some(value) = self.nested_aggregate_val(e) {
                    return value;
                }
                Val::Scalar(self.lower_expr(e))
            }
            // A struct literal (named or name-less): one value per field.
            // Explicit `.re = v` binds by name; a positional arg binds to the
            // struct's field at that position (needs a named struct type).
            ast::Expr::Construct {
                ty,
                args,
                spread,
                span,
            } => {
                // Field order comes from the construct's type, or (for a
                // name-less `{ ..base, .. }`) from the spread base's struct type.
                let struct_name: Option<String> = ty
                    .as_ref()
                    .and_then(|ty| self.free_fns.type_head_key(ty))
                    .or_else(|| {
                        spread
                            .as_ref()
                            .and_then(|b| expr_path(b))
                            .and_then(|p| self.local_struct_repr.get(&p).cloned())
                    });
                let field_order: Option<Vec<String>> = struct_name
                    .as_deref()
                    .and_then(|n| self.raw_struct_fields(n))
                    .map(|fs| fs.into_iter().map(|(n, _)| n).collect());
                let mut fields: Vec<(String, Expr)> = Vec::new();
                // `{ ..base, .. }`: seed every field from `base` before overrides.
                if let (Some(base), Some(sname)) = (spread, struct_name.as_deref()) {
                    // Copy per *leaf*, not per top-level field: a field that is
                    // itself a struct has no scalar form, so reading it whole
                    // would yield `Unknown` and silently drop everything nested
                    // that the spread was supposed to carry over.
                    for leaf in self.struct_leaf_names(sname) {
                        let mut fe = (**base).clone();
                        for part in leaf.split('.') {
                            fe = ast::Expr::Field {
                                base: Box::new(fe),
                                field: ast::Ident {
                                    text: part.to_string(),
                                    span: *span,
                                },
                                span: *span,
                            };
                        }
                        let v = self.lower_scalar_env(&fe, env);
                        fields.push((leaf, v));
                    }
                }
                for (i, a) in args.iter().enumerate() {
                    let fname = match &a.field {
                        Some(f) => f.text.clone(),
                        None => field_order
                            .as_ref()
                            .and_then(|o| o.get(i).cloned())
                            .unwrap_or_default(),
                    };
                    // Every field carries a value; a value-less arg only reaches
                    // here on parser recovery (already diagnosed).
                    // A field holding a struct of its own (composition:
                    // `Outer { .inner = Inner { .a = v } }`) lowers to its own
                    // `Fields`, which has no scalar form. Splice those in under
                    // a dotted name so the flat map still addresses one leaf per
                    // entry — `inner.a` then resolves against the flattened
                    // signal `…o.inner.a` exactly as a top-level field does.
                    // A struct-typed field reads its value against that
                    // field's type, so a positional literal nested in a named
                    // one is itself a struct literal rather than the concat it
                    // lexes as.
                    let field_ty = struct_name
                        .as_deref()
                        .and_then(|n| self.structs.get(n))
                        .and_then(|sd| sd.fields.iter().find(|f| f.name.text == fname))
                        .map(|f| f.ty.clone());
                    let vals: Vec<(String, Expr)> = match &a
                        .value
                        .as_ref()
                        .map(|v| self.as_struct_literal(field_ty.as_ref(), v))
                    {
                        Some(v) => match self.lower_val_env(v, env) {
                            Val::Scalar(e) => vec![(fname, e)],
                            Val::Fields(inner) => inner
                                .into_iter()
                                .map(|(n, e)| (format!("{fname}.{n}"), e))
                                .collect(),
                        },
                        None => vec![(fname, Expr::Unknown)],
                    };
                    for (fname, v) in vals {
                        match fields.iter_mut().find(|(n, _)| *n == fname) {
                            Some(slot) => slot.1 = v,
                            None => fields.push((fname, v)),
                        }
                    }
                }
                Val::Fields(fields)
            }
            ast::Expr::Binary { op, lhs, rhs, .. } => {
                let op_str = crate::syntax::pretty::bin_op(op);
                if let Some(native) =
                    self.native_vector_logical(op_str, lhs, rhs, &|e| self.lower_scalar_env(e, env))
                {
                    return Val::Scalar(native);
                }
                if !matches!(op_str, "==" | "!=") {
                    if let Some(v) = self.inline_op(op_str, lhs, rhs, env) {
                        return v;
                    }
                }
                if let Some(derived) = self.inline_cmp(op_str, lhs, rhs, env) {
                    return Val::Scalar(derived);
                }
                let (l, r) = (
                    self.lower_scalar_env(lhs, env),
                    self.lower_scalar_env(rhs, env),
                );
                Val::Scalar(self.make_binary(
                    op.clone(),
                    l,
                    r,
                    self.binary_uses_kernel_integer(lhs, rhs),
                    self.declares_kernel_integer(lhs) || self.declares_kernel_integer(rhs),
                ))
            }
            ast::Expr::Unary { op, rhs, .. } => {
                // `not x` on an enum operand inlines its impl, as it does in
                // `lower_expr`. Building a raw unary here negated the
                // discriminant instead of consulting `Logic`'s table, so
                // `(not a) and b` gave '1' where 'X' was meant — while
                // `let t = not a; t and b`, the same thing named, was right.
                if *op == ast::UnOp::Not {
                    if let Some(v) = self.inline_unary("not", rhs) {
                        return v;
                    }
                    // `not` on a vector is `mask - x`, not a bitwise
                    // complement. `lower_expr` knew that and this path did
                    // not, so `unsigned[8](not s)` — the same operand inside
                    // a conversion — lowered to a raw `not` and read 0 where
                    // the bare `not s` gave 55.
                    if let Some(v) = self.vector_not(rhs, |e| self.lower_scalar_env(e, env)) {
                        return Val::Scalar(v);
                    }
                }
                Val::Scalar(self.make_unary(*op, self.lower_scalar_env(rhs, env)))
            }
            ast::Expr::SuffixLit { .. } => self.inline_suffix(e).unwrap_or_else(|| {
                Val::Scalar(self.lower_expr(e)) // fixed fs/Hz table fallback
            }),
            _ => self
                .nested_aggregate_val(e)
                .unwrap_or_else(|| Val::Scalar(self.lower_expr(e))),
        }
    }

    /// Inline the `impl Suffix<sym, _> for T` `suffix` fn for a suffixed
    /// literal (`5i` -> the `"i"` impl's body): its parameter binds to the
    /// literal value.
    pub(super) fn inline_suffix(&self, e: &ast::Expr) -> Option<Val> {
        let ast::Expr::SuffixLit { text, suffix, .. } = e else {
            return None;
        };
        let (_, f) = self.suffix_impls.get(&suffix.text)?;
        let body = f.body.as_ref()?;
        let mut env: HashMap<String, Val> = HashMap::new();
        if let Some(p) = f.params.iter().find(|p| !p.is_self) {
            if let Some(n) = &p.name {
                // A `real` parameter takes the literal's float value.
                let is_real = p.ty.as_ref().and_then(type_head_name) == Some("real");
                let v = if is_real {
                    Expr::Real(text.replace('_', "").parse().unwrap_or(0.0))
                } else {
                    Expr::Const(parse_int(text).unwrap_or(0))
                };
                env.insert(n.text.clone(), Val::Scalar(v));
            }
        }
        self.inline_block(&body.stmts, &env)
    }

    /// Lower an expression to a single scalar in `env`.
    pub(super) fn lower_scalar_env(&self, e: &ast::Expr, env: &HashMap<String, Val>) -> Expr {
        match self.lower_val_env(e, env) {
            Val::Scalar(e) => e,
            Val::Fields(_) => Expr::Unknown, // a struct value has no scalar context
        }
    }

    /// The per-field value of a struct-typed local (`p` -> `p.re`, `p.im`).
    pub(super) fn struct_local_val(&self, name: &str) -> Option<Val> {
        let sname = self.local_struct_repr.get(name)?;
        let s = self.structs.get(sname)?;
        Some(Val::Fields(
            s.fields
                .iter()
                .map(|f| {
                    let sig = self.locals.get(&format!("{name}.{}", f.name.text));
                    (
                        f.name.text.clone(),
                        sig.map(|&id| Expr::Current(id)).unwrap_or(Expr::Unknown),
                    )
                })
                .collect(),
        ))
    }

    /// A flattened struct or array signal as one aggregate value. Scanning
    /// leaf names also handles nested structs/arrays, where no signal exists
    /// for an intermediate field.
    /// A *nested* aggregate read whole: `outer.inner`, `w[0]` where `w` is an
    /// array of structs. Only a bare name reached `aggregate_signal_val`, so
    /// these were lowered as ordinary scalars -- which an aggregate has none of
    /// -- and reported "has no hardware form", a message about runtime indices
    /// on a path whose indices are literal. Writing one already worked, so a
    /// struct array element could be assigned but not read back.
    pub(super) fn nested_aggregate_val(&self, e: &ast::Expr) -> Option<Val> {
        let path = self.folded_elem_path(e)?;
        self.aggregate_signal_val(&path)
    }

    /// The aggregate value of a struct- or array-typed signal, as its leaves.
    pub(super) fn aggregate_signal_val(&self, name: &str) -> Option<Val> {
        if !self.local_struct_repr.contains_key(name) && !self.local_array.contains_key(name) {
            return None;
        }
        let field_prefix = format!("{name}.");
        let element_prefix = format!("{name}[");
        let mut fields: Vec<(String, Expr)> = self
            .locals
            .iter()
            .filter(|(path, _)| {
                path.starts_with(&field_prefix) || path.starts_with(&element_prefix)
            })
            .map(|(path, &signal)| {
                (
                    path.strip_prefix(name)
                        .unwrap_or(path)
                        .trim_start_matches('.')
                        .to_string(),
                    Expr::Current(signal),
                )
            })
            .collect();
        if fields.is_empty() {
            return self.struct_local_val(name);
        }
        fields.sort_by(|left, right| left.0.cmp(&right.0));
        Some(Val::Fields(fields))
    }

    /// The declared index range `(left, right)` in written order of a vector or
    /// array type — `Logic[7..0]` -> `(7, 0)`, a named `range` const keeps its
    /// direction, a width-only `Bit[4]` -> `(0, 3)` (ascending). `None` for a
    /// non-indexed type.
    pub(super) fn declared_range(
        &self,
        ty: &ast::Type,
        env: &HashMap<String, i64>,
    ) -> Option<(i64, i64)> {
        let ast::Type::Indexed {
            index: Some(idx), ..
        } = ty
        else {
            return None;
        };
        match idx.as_ref() {
            // A written range keeps its direction (`[7..0]` is descending); the
            // `Range` fields are first/second as written, not numerically sorted.
            ast::Expr::Range { lo, hi, .. } => {
                Some((self.eval_const(lo, env)?, self.eval_const(hi, env)?))
            }
            // A named range constant (`const BYTE: range = 7..0;`).
            ast::Expr::Path(path) => {
                if let Some(bounds) = self
                    .free_fns
                    .constant_path_key(path)
                    .and_then(|key| self.const_ranges.get(&key).copied())
                {
                    Some(bounds)
                } else {
                    let n = self.eval_const(idx, env)?;
                    Some((0, (n - 1).max(0)))
                }
            }
            // A width-only index (`Bit[4]`, `unsigned[8]`) is ascending `0..N-1`.
            _ => {
                let n = self.eval_const(idx, env)?;
                Some((0, (n - 1).max(0)))
            }
        }
    }

    /// Lower a system attribute. `clk.rising()`/`falling`/`edge` expand into
    /// `Event`/`Old`/`Current` so the scheduler needs no special knowledge.
    pub(super) fn persisted_layout(&self, local_path: &str) -> Option<&SourceLayout> {
        self.out
            .source_layouts
            .get(&format!("{}.{}", self.cur_instance_path, local_path))
    }

    /// The declared value range retained for a local, when lowering kept one.
    pub(super) fn persisted_range(&self, local_path: &str) -> Option<(i64, i64)> {
        self.persisted_layout(local_path)
            .and_then(SourceLayout::index_range)
            .map(|range| (range.left, range.right))
    }

    /// Lower a system attribute (`'event`, `'old`, `'length`) into its IR form.
    pub(super) fn lower_sysattr(&self, base: &ast::Expr, attr: &str) -> Expr {
        // `::length` is elaboration-time metadata: an array's element count,
        // else a signal's bit width (they coincide for a flat vector, so one
        // attribute serves both — VHDL's `'length`).
        if attr == "length" {
            if let Some(ty) = self.block_local_type(base) {
                if let Some((_, indices)) = array_of(
                    &ty,
                    &self.cur_env,
                    &self.const_ranges,
                    &self.array_families,
                    &self.free_fns,
                ) {
                    return Expr::Const(indices.len() as u64);
                }
                return Expr::Const(self.block_local_width(&ty) as u64);
            }
            if let Some(layout) = expr_path(base).and_then(|path| self.persisted_layout(&path)) {
                let length = match &layout.kind {
                    LayoutKind::Array { range, .. } => range.and_then(LayoutRange::len),
                    LayoutKind::Packed { width, .. } | LayoutKind::Scalar { width, .. } => {
                        Some(u64::from(*width))
                    }
                    LayoutKind::Opaque { width, .. } => width.map(u64::from),
                    LayoutKind::Struct { .. } => None,
                };
                if let Some(length) = length {
                    return Expr::Const(length);
                }
            }
            if let Some(sig) = self.base_signal(base) {
                return Expr::Const(self.out.signals[sig.0 as usize].width as u64);
            }
            return Expr::Unknown;
        }
        // Range bounds from the declared index range (VHDL `'left`/`'right`/
        // `'high`/`'low`/`'ascending`): `left`/`right` in written order,
        // `high`/`low` numeric, `ascending` the direction (`to` vs `downto`).
        if matches!(attr, "left" | "right" | "high" | "low" | "ascending") {
            let local_declared = self
                .block_local_type(base)
                .and_then(|ty| self.declared_range(&ty, &self.cur_env));
            if let Some((l, r)) = local_declared
                .or_else(|| expr_path(base).and_then(|path| self.persisted_range(&path)))
            {
                let v = match attr {
                    "left" => l,
                    "right" => r,
                    "high" => l.max(r),
                    "low" => l.min(r),
                    "ascending" => (l <= r) as i64,
                    _ => unreachable!(),
                };
                return Expr::Const(v as u64);
            }
            return Expr::Unknown;
        }
        let Some(sig) = self.base_signal(base) else {
            // An aggregate has no signal of its own — elaboration flattens it
            // into one leaf per field or element. `'old` still lands on a leaf
            // (`p'old.data` sinks to `p.data'old`, `a'old[0]` indexes first),
            // but `'event` has nothing to sink to and returned `Unknown`, so
            // `p'event` and `a'event` failed to lower at all. The spec defines
            // both: "any field changed" / "any element changed" — which is the
            // OR over the leaves.
            if attr == "event" {
                if let Some(leaves) = self.aggregate_leaves(base) {
                    return leaves
                        .into_iter()
                        .map(Expr::Event)
                        .reduce(or_expr)
                        .unwrap_or(Expr::Const(0));
                }
            }
            return Expr::Unknown;
        };
        match attr {
            // `::event`/`::old` are the primitives; the edge helpers are the
            // std `ClockLike` methods, which inline to these plus a comparison.
            "event" => Expr::Event(sig),
            "old" => Expr::Old(sig),
            _ => Expr::Unknown,
        }
    }

    /// The leaf signals a struct or array path flattens into, in signal order
    /// so the lowered expression is stable. `None` when the path names no
    /// aggregate (an unknown name, or a scalar handled by `base_signal`).
    pub(super) fn aggregate_leaves(&self, base: &ast::Expr) -> Option<Vec<SignalId>> {
        let path = expr_path(base)?;
        let (field, element) = (format!("{path}."), format!("{path}["));
        let mut leaves: Vec<SignalId> = self
            .locals
            .iter()
            .filter(|(name, _)| name.starts_with(&field) || name.starts_with(&element))
            .map(|(_, id)| *id)
            .collect();
        if leaves.is_empty() {
            return None;
        }
        leaves.sort_by_key(|id| id.0);
        Some(leaves)
    }

    /// One position of an elementwise array expression: every path naming an
    /// array of the same length becomes that array's `k`-th element, so
    /// `a and b` at position 0 is `a[a0] and b[b0]` — paired by position, so
    /// a descending range keeps its own indices. `None` when an operand is
    /// not such an array, which leaves the existing paths to report it.
    pub(super) fn elementwise_at(&self, e: &ast::Expr, k: usize, len: usize) -> Option<ast::Expr> {
        match e {
            ast::Expr::Path(_) => {
                let indices = self.local_array.get(&expr_path(e)?)?;
                let index = *indices.get(k).filter(|_| indices.len() == len)?;
                if index < 0 {
                    return None;
                }
                let span = ast::expr_span(e);
                Some(ast::Expr::Index {
                    base: Box::new(e.clone()),
                    index: Box::new(ast::Expr::Int {
                        text: index.to_string(),
                        span,
                    }),
                    span,
                })
            }
            ast::Expr::Binary { op, lhs, rhs, span } => Some(ast::Expr::Binary {
                op: op.clone(),
                lhs: Box::new(self.elementwise_at(lhs, k, len)?),
                rhs: Box::new(self.elementwise_at(rhs, k, len)?),
                span: *span,
            }),
            // The condition is a scalar, so it is shared by every element;
            // only the branches are per-element. `y = if c { a } else { b }`
            // on an array had no form at all and reported the *target* as
            // unassignable, the same misleading shape the operators had.
            ast::Expr::IfExpr {
                cond,
                then,
                els,
                span,
            } => Some(ast::Expr::IfExpr {
                cond: cond.clone(),
                then: Box::new(self.elementwise_at(then, k, len)?),
                els: Box::new(self.elementwise_at(els, k, len)?),
                span: *span,
            }),
            // `match` selects a whole branch the way `if` does; the two
            // share `MatchArm` and have drifted apart before, so they are
            // lifted together here.
            ast::Expr::Match {
                scrutinee,
                arms,
                span,
            } => {
                let mut lifted = Vec::with_capacity(arms.len());
                for a in arms {
                    let value = self.elementwise_at(a.value_expr()?, k, len)?;
                    lifted.push(ast::MatchArm {
                        pattern: a.pattern.clone(),
                        body: ast::Block {
                            stmts: vec![ast::Stmt::Expr(value)],
                            span: a.body.span,
                        },
                        span: a.span,
                    });
                }
                Some(ast::Expr::Match {
                    scrutinee: scrutinee.clone(),
                    arms: lifted,
                    span: *span,
                })
            }
            ast::Expr::Unary { op, rhs, span } => Some(ast::Expr::Unary {
                op: *op,
                rhs: Box::new(self.elementwise_at(rhs, k, len)?),
                span: *span,
            }),
            _ => None,
        }
    }

    /// The signal at the base of an access expression.
    pub(super) fn base_signal(&self, base: &ast::Expr) -> Option<SignalId> {
        if let ast::Expr::Path(p) = base {
            if p.segments.len() == 1 {
                // `self` inside an inlined method body binds to the receiver.
                if p.segments[0].text == "self" {
                    if let Some(sig) = self.self_signal.get() {
                        return Some(sig);
                    }
                }
                return self.locals.get(&p.segments[0].text).copied();
            }
        }
        // A struct field or array element is a signal in its own right, named
        // by its flattened path (`p.valid`, `xs[0]`).
        self.locals.get(&expr_path(base)?).copied()
    }
}
