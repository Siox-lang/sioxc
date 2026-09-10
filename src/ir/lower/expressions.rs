//! Expression, operator, conversion, call, and attribute lowering.

use super::*;

impl<'a> Lowering<'a> {
    /// Lower an expression into IR.
    pub(super) fn lower_expr(&self, e: &ast::Expr) -> Expr {
        match e {
            ast::Expr::Call { callee, args, .. } => {
                // `T()` — the nullary constructor — resolves to the type's
                // default before any free-fn/conversion lookup (scalar context).
                if let Some(Val::Scalar(v)) = self.lower_new(callee, args) {
                    return v;
                }
                self.lower_conversion(callee, args, &HashMap::new())
                    .or_else(
                        || match self.lower_free_call(callee, args, &HashMap::new()) {
                            Some(Val::Scalar(v)) => Some(v),
                            _ => None,
                        },
                    )
                    .or_else(
                        || match self.lower_method_call(callee, args, &HashMap::new()) {
                            Some(Val::Scalar(v)) => Some(v),
                            _ => None,
                        },
                    )
                    .or_else(|| match self.lower_from(callee, args, &HashMap::new()) {
                        Some(Val::Scalar(v)) => Some(v),
                        _ => None,
                    })
                    .unwrap_or(Expr::Unknown)
            }
            // `if c { a } else { b }` is a mux: lower to a select.
            ast::Expr::IfExpr {
                cond, then, els, ..
            } => Expr::Select {
                cond: Box::new(self.lower_expr(cond)),
                then: Box::new(self.lower_expr(then)),
                els: Box::new(self.lower_expr(els)),
            },
            // A match-expression is a first-match `Select` chain over the arms.
            ast::Expr::Match {
                scrutinee, arms, ..
            } => self.lower_match_expr(scrutinee, arms),
            // A decimal point makes it a `real` literal (`1.5`).
            ast::Expr::Int { text, .. } if text.contains('.') => {
                Expr::Real(text.replace('_', "").parse().unwrap_or(0.0))
            }
            ast::Expr::Int { text, .. } => integer_const(text).unwrap_or(Expr::Const(0)),
            // A suffix with an `impl Suffix` fn inlines it (scalar results
            // only here; struct results flow through `lower_val_env`).
            // Otherwise `1ns` / `10MHz` scale by the fixed fs/Hz table.
            ast::Expr::SuffixLit { text, suffix, .. } => match self.inline_suffix(e) {
                Some(Val::Scalar(v)) => v,
                Some(Val::Fields(_)) => Expr::Unknown,
                None => Expr::Const(
                    parse_int(text)
                        .map(|v| {
                            v.saturating_mul(ast::suffix_scale(&suffix.text).unwrap_or(1) as u64)
                        })
                        .unwrap_or(0),
                ),
            },
            // Keep every word: `decode_bit_string` returns the low one, so a
            // literal past 64 bits used to lose its top half in driver
            // position while the same literal as an initializer (which
            // already decoded to words) kept it — `w = x"DEADBEEF0123..."`
            // drove `0x0123...` and `let w: unsigned[96] = x"DEADBEEF0123..."`
            // did not.
            ast::Expr::BitStrLit { base, digits, .. } => {
                words_const(self.decode_bit_string_words(*base, digits).0)
            }
            // A plain string in value position is a logic-value vector
            // (`out = "1X10"`) — decode it per-char like a binary bit string.
            // (Char/enum arrays are filled element-wise before reaching here.)
            ast::Expr::StrLit { text, .. } => {
                words_const(self.decode_bit_string_words('b', text).0)
            }
            ast::Expr::CharLit { ch, .. } => Expr::Logic(*ch),
            ast::Expr::Path(path) => {
                let leaf = path
                    .segments
                    .last()
                    .map(|name| name.text.as_str())
                    .unwrap_or("");
                if path.segments.len() == 1 {
                    if let Some(Val::Scalar(value)) = self.block_local_value(e) {
                        return value;
                    }
                    if let Some(id) = self.locals.get(leaf) {
                        return Expr::Current(*id);
                    }
                }
                // Module constants use their resolved qualified identity;
                // implementation constants and parameters retain a local leaf
                // key. This lookup intentionally precedes enum variants so
                // `a::VALUE` is not mistaken for `Enum::Variant`.
                if let Some(key) = self.free_fns.constant_path_key(path) {
                    if let Some(value) = self.const_values.get(&key) {
                        return value.clone();
                    }
                    if let Some(&v) = self.cur_env.get(&key) {
                        return Expr::Const(v as u64);
                    }
                    if let Some(&f) = self.consts_real.get(&key) {
                        return Expr::Real(f);
                    }
                }
                if path.segments.len() >= 2 {
                    return self
                        .enum_variant_path(path)
                        .map(Expr::Const)
                        .unwrap_or(Expr::Unknown);
                }
                // A generic parameter of the entity being lowered has no
                // value when that entity is analysed on its own rather than
                // through an instantiation — `check` roots every
                // uninstantiated entity so library code is analysed too. That
                // is parametric, not unknown, and the same parameter in *type*
                // position (`unsigned[N]`) has always been tolerated this way;
                // only the value position reported the author's own parameter
                // as an undeclared name.
                let parametric = self
                    .lower_stack
                    .last()
                    .and_then(|entity| self.entities.get(entity))
                    .is_some_and(|decl| decl.params.params.iter().any(|q| q.name.text == leaf));
                if !parametric {
                    // Nothing declares this name. Every signal, constant and
                    // in-scope parameter is known here, so record it rather
                    // than lowering to a silent `Unknown` that `check` called
                    // ok.
                    self.unresolved_names
                        .borrow_mut()
                        .push((leaf.to_string(), path.span));
                }
                Expr::Unknown
            }
            // An element of a constant lookup table (`TAB[2]`, `TAB[addr]`).
            // A signal array has had both forms since dynamic indexing landed;
            // a `const` array had neither, because constants are stored one
            // scalar per name. Both lowered to `Unknown` and were reported as
            // a driver index with no name attached.
            ast::Expr::Index { base, index, .. }
                if self
                    .free_fns
                    .constant_expr_key(base)
                    .is_some_and(|key| self.const_arrays.contains_key(&key)) =>
            {
                let key = self.free_fns.constant_expr_key(base).unwrap();
                let values = &self.const_arrays[&key];
                if let Some(i) = self.eval_const(index, &self.consts) {
                    return usize::try_from(i)
                        .ok()
                        .and_then(|i| values.get(i).cloned())
                        // Out of range reads 0, as a dynamic index does.
                        .unwrap_or(Expr::Const(0));
                }
                // A runtime index selects between the elements, the same mux
                // chain `lower_dynamic_read` builds over a signal array.
                let labels: Vec<i64> = (0..values.len())
                    .filter_map(|index| i64::try_from(index).ok())
                    .collect();
                let idx = self
                    .checked_runtime_index(index, &labels)
                    .unwrap_or_else(|| self.lower_expr(index));
                let mut acc = Expr::Const(0);
                for (i, value) in values.iter().enumerate().rev() {
                    acc = Expr::Select {
                        cond: Box::new(Expr::Binary {
                            op: BinOp::Eq,
                            lhs: Box::new(idx.clone()),
                            rhs: Box::new(Expr::Const(i as u64)),
                        }),
                        then: Box::new(value.clone()),
                        els: Box::new(acc),
                    };
                }
                acc
            }
            // A bit slice `base[a..b]` (constant bounds, possibly a named
            // range constant). Direction follows the written order: `7..4`
            // (descending) extracts MSB-first — the natural bit order —
            // while `4..7` (ascending) extracts with the bit order reversed.
            ast::Expr::Index { base, index, .. }
                if self.storage_slice_bounds(base, index).is_some() =>
            {
                let (a, b) = self.storage_slice_bounds(base, index).unwrap();
                let lowered = self.lower_expr(base);
                if a >= b {
                    Expr::Slice {
                        base: Box::new(lowered),
                        hi: a,
                        lo: b,
                    }
                } else {
                    // Ascending: reassemble bits a..=b with significance
                    // reversed: source bit (a+k) lands at result bit (w-1-k).
                    let w = b - a + 1;
                    let mut acc = Expr::Const(0);
                    for k in 0..w {
                        let bit = Expr::Slice {
                            base: Box::new(lowered.clone()),
                            hi: a + k,
                            lo: a + k,
                        };
                        let shifted = Expr::Binary {
                            op: BinOp::Shl,
                            lhs: Box::new(bit),
                            rhs: Box::new(Expr::Const((w - 1 - k) as u64)),
                        };
                        acc = Expr::Binary {
                            op: BinOp::Add,
                            lhs: Box::new(acc),
                            rhs: Box::new(shifted),
                        };
                    }
                    acc
                }
            }
            // A struct-field (`s.data`) or constant array-element (`a[2]`) access
            // resolves to its flattened signal; a *dynamic* array index
            // (`mem[addr]`) becomes a mux tree over the element signals.
            ast::Expr::Field { .. } | ast::Expr::Index { .. } => {
                if let Some(Val::Scalar(value)) = self.block_local_value(e) {
                    return value;
                }
                // `p'old.valid` / `xs'old[0]`: a struct or array is stored as
                // leaf signals, so there is no one signal to take the previous
                // value of. The attribute belongs on the leaf, and
                // `p.valid'old` means the same thing (spec 3.9 writes the
                // first form).
                if let Some(sunk) = sunk_sysattr(e) {
                    return self.lower_expr(&sunk);
                }
                if let Some(id) = expr_path(e).and_then(|p| self.locals.get(&p).copied()) {
                    return Expr::Current(id);
                }
                // A struct constant's field (`K.a`). It has no signal — a
                // constant is a value, not storage — so the dotted path is
                // looked up in the constant table the same way a plain `N`
                // is, one entry per field.
                if let Some(value) = self
                    .free_fns
                    .constant_expr_key(e)
                    .and_then(|key| self.const_values.get(&key))
                {
                    return value.clone();
                }
                // A constant index into an array literal (`[3, 4][0]`). This is
                // what an array-literal argument becomes once the parameter is
                // substituted, and it is a value with no storage behind it, so
                // there is no signal to find — the element is simply picked.
                if let ast::Expr::Index { base, index, .. } = e {
                    if let ast::Expr::Array { elems, .. } = base.as_ref() {
                        if let Some(element) = self
                            .eval_const(index, &self.cur_env)
                            .and_then(|i| usize::try_from(i).ok())
                            .and_then(|i| elems.get(i))
                        {
                            return self.lower_expr(element);
                        }
                    }
                }
                if let Some(v) = self.lower_block_dynamic_access(e) {
                    return v;
                }
                if let Some(v) = self.lower_dynamic_access(e) {
                    return v;
                }
                if let ast::Expr::Index { base, index, .. } = e {
                    if let Some(v) = self.lower_custom_index(base, index) {
                        return v;
                    }
                }
                if self.record_unelaborated_instance_use(e) {
                    return Expr::Unknown;
                }
                if self.is_unresolved_instance_array_reference(e) {
                    return Expr::Unknown;
                }
                // No signal, no mux tree, no `Index` impl. Record the shape
                // while the source is still in hand: from the IR this was an
                // anonymous `Unknown`, and the reader was told only which
                // signal's driver contained one.
                self.unsupported_exprs
                    .borrow_mut()
                    .push((crate::syntax::pretty::expr_string(e), ast::expr_span(e)));
                Expr::Unknown
            }
            ast::Expr::SysAttr { base, attr, .. } => self.lower_sysattr(base, &attr.text),
            ast::Expr::Unary { op, rhs, .. } => {
                // `not` on an enum-typed operand inlines its impl (`impl
                // "not" for Logic`), like binary operators.
                if *op == ast::UnOp::Not {
                    if let Some(Val::Scalar(v)) = self.inline_unary("not", rhs) {
                        return v;
                    }
                    // "Boolean per bit": `not` on a vector-valued signal
                    // reference (name, field, element, slice) inverts every
                    // bit — lower to `x xor mask` so the engines need no
                    // width knowledge. A 1-bit operand keeps the boolean
                    // form (same 0<->1 either way), as do compound
                    // expressions (`not (a == b)`) and enum-typed signals
                    // (their `not` is the impl above, or undefined).
                    let is_vector_ref = match rhs.as_ref() {
                        // A slice is always a bit vector.
                        ast::Expr::Index { base, index, .. }
                            if self.slice_bounds(base, index).is_some() =>
                        {
                            true
                        }
                        ast::Expr::Path(_) | ast::Expr::Field { .. } | ast::Expr::Index { .. } => {
                            expr_path(rhs)
                                .and_then(|p| self.locals.get(&p))
                                .map(|&id| self.out.signals[id.0 as usize].enum_type.is_none())
                                .unwrap_or(false)
                        }
                        _ => false,
                    };
                    let _ = is_vector_ref;
                    if let Some(v) = self.vector_not(rhs, |e| self.lower_expr(e)) {
                        return v;
                    }
                }
                self.make_unary(*op, self.lower_expr(rhs))
            }
            ast::Expr::Binary { op, lhs, rhs, .. } => {
                // An operator on an enum/struct-typed operand inlines its
                // operator-trait impl body (spec 3.25); `==`/`!=` stay
                // built-in discriminant comparison unless `<=>` derives them.
                let op_str = crate::syntax::pretty::bin_op(op);
                if let Some(native) =
                    self.native_vector_logical(op_str, lhs, rhs, &|e| self.lower_expr(e))
                {
                    return native;
                }
                // Every route to a comparison is marked, because they all owe
                // the same answer: an `Operator` impl, the `<=>` derivation,
                // and the built-in below.
                if !matches!(op_str, "==" | "!=") {
                    if let Some(Val::Scalar(inlined)) =
                        self.inline_op(op_str, lhs, rhs, &HashMap::new())
                    {
                        return self.mark_vector_compare(op, lhs, rhs, inlined);
                    }
                }
                if let Some(derived) = self.inline_cmp(op_str, lhs, rhs, &HashMap::new()) {
                    return self.mark_vector_compare(op, lhs, rhs, derived);
                }
                let (mut l, mut r) = (self.lower_expr(lhs), self.lower_expr(rhs));
                // A character literal's identity comes from its counterpart's
                // type (`c == 'x'` with c: Char reads 'x' as Unicode).
                if let ast::Expr::CharLit { ch, .. } = lhs.as_ref() {
                    if let Some(v) = self.typed_char_literal(*ch, rhs) {
                        l = v;
                    }
                }
                if let ast::Expr::CharLit { ch, .. } = rhs.as_ref() {
                    if let Some(v) = self.typed_char_literal(*ch, lhs) {
                        r = v;
                    }
                }
                let built = self.make_binary(
                    op.clone(),
                    l,
                    r,
                    self.binary_uses_kernel_integer(lhs, rhs),
                    self.declares_kernel_integer(lhs) || self.declares_kernel_integer(rhs),
                );
                self.mark_vector_compare(op, lhs, rhs, built)
            }
            // `{a, b, c}`: fold into `(((0 << w_a) or a) << w_b) or b ...`.
            // First part is the MSBs.
            //
            // `or` rather than `+`: the parts do not overlap, so the two agree
            // bit for bit, but the metavalue companion reads a `+` as
            // arithmetic and poisons the whole result. Joining two fields is
            // not arithmetic, and `"X100" & "1101"` should keep its `'X'` in
            // place rather than turn eight elements unknown.
            ast::Expr::Concat { parts, .. } => {
                let mut acc = Expr::Const(0);
                for part in parts {
                    let w = self.ast_width(part);
                    let e = self.lower_expr(part);
                    let shifted = Expr::Binary {
                        op: BinOp::Shl,
                        lhs: Box::new(acc),
                        rhs: Box::new(Expr::Const(w as u64)),
                    };
                    acc = Expr::Binary {
                        op: BinOp::Or,
                        lhs: Box::new(shifted),
                        rhs: Box::new(e),
                    };
                }
                acc
            }
            _ => Expr::Unknown,
        }
    }

    /// The bit width of a source expression, for sizing concatenations. A nested
    /// concat sums its parts; a slice is its span; a signal/field/element is its
    /// declared width; a literal is its minimal width.
    /// The width of a *direct width-bearing reference* on the RHS of an
    /// assignment — a signal name, struct field, constant array element, bit
    /// slice, or concatenation — for the strict assignment-width check. Returns
    /// `None` for everything else (arithmetic, literals, conversions, muxes,
    /// calls): those are exempt because operator results are not auto-widened
    /// (overflow wraps at the operand width; a different width is an explicit
    /// `resize`), so only signal-to-signal width equality is enforced.
    /// A concat assignment target has an exact width (the sum of its parts),
    /// so the source must match it — the same strict rule scalar targets
    /// follow (spec 3.17). Without this the lowering just slices whatever it
    /// is given: an 8-bit `{y, z}` fed 4 bits silently zero-filled `y`.
    pub(super) fn check_concat_target_width(
        &mut self,
        parts: &[ast::Expr],
        value: &ast::Expr,
        span: crate::diag::Span,
    ) {
        let want: u32 = parts.iter().map(|p| self.ast_width(p)).sum();
        let Some(have) = self.ref_width(value) else {
            return;
        };
        if want > 0 && have > 0 && want != have {
            self.sink.emit(
                crate::diag::Diagnostic::error(format!(
                    "width mismatch: this concatenation target is {want} bits but the \
                     assigned value is {have} bits"
                ))
                .with_code(crate::diag::codes::TYPE_MISMATCH)
                .at(span)
                .help(
                    "widths must match; use a conversion (`unsigned[N](x)` / \
                     `resize(x, N)`) to change width",
                ),
            );
        }
    }

    /// The width of the value an expression refers to, when it names storage.
    pub(super) fn ref_width(&self, e: &ast::Expr) -> Option<u32> {
        if let Some(ty) = self.block_local_type(e) {
            return Some(self.block_local_width(&ty));
        }
        // Indexing is precisely where syntax-only lowering cannot distinguish
        // a nominal array family from its scalar element. Literal and match types are
        // intentionally contextual, so their best-effort Stage-4 default
        // (`integer`) must not override the assignment target's width here.
        if matches!(e, ast::Expr::Index { .. }) {
            if let Some(width) = self
                .expr_types
                .get(&ast::expr_span(e))
                .and_then(crate::types::Ty::bit_width)
            {
                return Some(width);
            }
        }
        match e {
            ast::Expr::Path(_) | ast::Expr::Field { .. } => {
                let p = expr_path(e)?;
                self.locals
                    .get(&p)
                    .map(|&id| self.out.signals[id.0 as usize].width)
            }
            ast::Expr::Index { base, index, .. } if self.slice_bounds(base, index).is_some() => {
                let (a, b) = self.slice_bounds(base, index)?;
                // A single element of a `Logic`-vector *is* a `Logic` — its
                // width is the element's (a 4-bit disc), not one value bit — so
                // `s: Logic = v[i]` matches, and a metavalue reconstructs.
                if a == b
                    && expr_path(base)
                        .and_then(|p| self.locals.get(&p))
                        .is_some_and(|&id| self.out.signals[id.0 as usize].width > 1)
                {
                    return Some(4);
                }
                Some((a.max(b) - a.min(b) + 1) as u32)
            }
            ast::Expr::Index { .. } => {
                // A constant element index (`v[2]`) reads its element signal.
                let p = expr_path(e)?;
                self.locals
                    .get(&p)
                    .map(|&id| self.out.signals[id.0 as usize].width)
            }
            ast::Expr::Concat { parts, .. } => Some(parts.iter().map(|p| self.ast_width(p)).sum()),
            _ => None,
        }
    }

    /// The width to bind for an operand of an inlined impl. A bare integer
    /// literal has no width of its own — `2` is two bits, and its top bit is
    /// set — so `rhs'length` made every positive literal look negative:
    /// `s / 2` took signed division's both-operands-negative branch and
    /// returned |s| / |2| with the sign dropped, 28 where -28 was meant. A
    /// literal operand is as wide as the value it is used with, the same rule
    /// its *family* already follows.
    pub(super) fn literal_aware_width(&self, e: &ast::Expr, other: u32) -> u32 {
        // Any constant expression, not just a bare literal: `0 - 2` is two
        // bits by its operands' own reckoning, so a negative literal divisor
        // failed the sign test the same way a positive one passed it.
        if other > 0 && self.eval_const(e, &self.cur_env).is_some() {
            return other;
        }
        self.ast_width(e)
    }

    /// The width an expression produces, from its operands and context.
    pub(super) fn ast_width(&self, e: &ast::Expr) -> u32 {
        if let Some(ty) = self.block_local_type(e) {
            return self.block_local_width(&ty);
        }
        // A bound parameter carries the caller's width, recorded at the
        // inline; without it a nested inline sees no width at all.
        if let Some(p) = expr_path(e) {
            if let Some(&w) = self.param_widths.borrow().get(&p) {
                return w;
            }
        }
        match e {
            ast::Expr::IfExpr { then, .. } => self.ast_width(then),
            ast::Expr::Match { arms, .. } => arms
                .iter()
                .filter_map(|a| a.value_expr())
                .map(|v| self.ast_width(v))
                .max()
                .unwrap_or(1),
            // `not x` is as wide as `x`. Without this the fallback gave 1, so
            // `sext(not s)` bound `x'length` to 1 and returned 0 where the
            // testbench — which does look through the unary — said 55.
            ast::Expr::Unary { rhs, .. } => self.ast_width(rhs),
            // Arithmetic and shifts are as wide as their operands. Without
            // this the fallback below gave 1, so `sext(s + 0)` bound
            // `x'length` to 1 and tested bit 0 for the sign: -56 came back
            // as 200, while `sext(s)` — the same value, named — was right.
            ast::Expr::Binary { op, lhs, rhs, .. } if op.keeps_operand_family() => {
                self.ast_width(lhs).max(self.ast_width(rhs))
            }
            // A conversion is as wide as its target (64 for kernel integer).
            ast::Expr::Call { callee, args, .. } => match callee.as_ref() {
                ast::Expr::Index { base, index, .. }
                    if expr_path(base)
                        .as_deref()
                        .is_some_and(|h| self.array_families.contains(h)) =>
                {
                    self.eval_const(index, &self.cur_env)
                        .map(|w| w as u32)
                        .unwrap_or(64)
                }
                ast::Expr::Path(p) if p.segments.len() == 1 && p.segments[0].text == "resize" => {
                    args.get(1)
                        .and_then(|n| self.eval_const(n, &self.cur_env))
                        .map(|w| w as u32)
                        .unwrap_or(64)
                }
                // An ordinary call is as wide as its declared return type. The
                // 64 below is the kernel-integer default; taking it for a
                // `signed[8]` result made a nested inline read `self'length`
                // as 64 and test bit 63 for the sign.
                _ => self
                    .free_fns
                    .get(callee)
                    .and_then(|f| f.ret.as_ref())
                    .map(|ret| {
                        type_width(
                            ret,
                            &self.cur_env,
                            &self.free_fns,
                            &self.structs,
                            &self.const_ranges,
                        )
                    })
                    .filter(|w| *w > 0)
                    .unwrap_or(64),
            },
            ast::Expr::Concat { parts, .. } => parts.iter().map(|p| self.ast_width(p)).sum(),
            ast::Expr::Index { base, index, .. } if self.slice_bounds(base, index).is_some() => {
                let (a, b) = self.slice_bounds(base, index).unwrap();
                (a.max(b) - a.min(b) + 1) as u32
            }
            ast::Expr::Int { text, .. } => {
                (u64::BITS - parse_int(text).unwrap_or(0).leading_zeros()).max(1)
            }
            // A bit-string literal has an explicit digit-count width.
            ast::Expr::BitStrLit { base, digits, .. } => (crate::syntax::radix_digits(digits)
                .count() as u32
                * crate::syntax::bits_per_digit(*base))
            .max(1),
            // A signal reference (name, struct field, constant array element).
            _ => expr_path(e)
                .and_then(|p| self.locals.get(&p))
                .map(|&id| self.out.signals[id.0 as usize].width)
                .unwrap_or(1),
        }
    }

    /// Inline the operator-trait impl body for `lhs OP rhs` when the left
    /// operand is an enum- or struct-typed local with a matching impl. The
    /// body must be a pure expression tree: `return e;` or `if c { .. } else
    /// { .. }` chains ending in returns (which become [`Expr::Select`], per
    /// field for struct values). `None` falls back to built-in lowering.
    // ponytail: operand types come from the outer locals, so `self + rhs`
    // nested *inside* an impl body doesn't re-inline; loops/match in bodies
    // unsupported until needed.
    /// Lower a derived logical operator on a *packed* vector natively, as
    /// `and`/`or` already are, instead of inlining std's body.
    ///
    /// std spells these arithmetically -- `xor` is `(a or b) - (a and b)`, and
    /// the complements subtract from an all-ones mask. That agrees on
    /// two-valued data, but the metavalue companion then sees a subtraction and
    /// poisons the whole vector, where `std_logic_1164` applies its table per
    /// element: `not "0000X100"` came back all `'X'` rather than `1111X011`.
    ///
    /// std cannot express the fix itself. A packed vector has no per-element
    /// signals, so the per-element blanket impls over `T[]` do not lower for
    /// one, and a complement needs `not` of a compound value -- which is the
    /// boolean form, not a bitwise one. That is the same reason `and` and `or`
    /// are core operators rather than library ones.
    ///
    /// A `Logic` scalar keeps its impl: `xor` on a nine-value discriminant is a
    /// table lookup, and `^` of two discriminants would be nonsense.
    pub(super) fn native_vector_logical(
        &self,
        op: &str,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        lower: &dyn Fn(&ast::Expr) -> Expr,
    ) -> Option<Expr> {
        if !matches!(op, "xor" | "nand" | "nor" | "xnor") {
            return None;
        }
        if ![lhs, rhs].iter().all(|e| {
            self.operand_type_name(e)
                .is_some_and(|f| self.out.array_element_of_family.contains_key(&f))
        }) {
            return None;
        }
        let (a, b) = (lower(lhs), lower(rhs));
        let bin = |op, lhs, rhs| Expr::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
        let inner = match op {
            "xor" | "xnor" => bin(BinOp::Xor, a, b),
            "nand" => bin(BinOp::And, a, b),
            _ => bin(BinOp::Or, a, b),
        };
        if op == "xor" {
            return Some(inner);
        }
        // The complement is `x xor all-ones`, which the companion lowering
        // reads per element the same way it reads any other `xor`.
        let width = self.ast_width(lhs);
        let mut ones = vec![0u64; (width as usize).max(1).div_ceil(64)];
        for i in 0..width {
            ones[i as usize / 64] |= 1u64 << (i % 64);
        }
        Some(bin(BinOp::Xor, inner, words_const(ones)))
    }

    /// Inline an operator impl's body at the call site, since hardware has no
    /// calls: the body becomes nested selects.
    pub(super) fn inline_op(
        &self,
        op: &str,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let lhs_ty = self.operand_type_name(lhs)?;
        let rhs_ty = self.operand_type_name(rhs);
        // `a + b` dispatches to the Rust-style trait (`Add`), spec 3.25.
        let tr = op;
        let fns = self.op_impls.get(&(tr.to_string(), lhs_ty.clone()))?;

        // Overload selection. Each candidate's declared rhs type is the
        // impl's trait argument (`impl Add<integer>`) or the fn's rhs
        // parameter type, with `Self` reading as the impl target. Pass 1:
        // exact rhs match. Pass 2: an `integer` operand (a literal) coerces
        // to a Self-typed rhs (`a + 1`). A sole candidate is accepted only
        // when the rhs operand's type is unknown — never on a known mismatch
        // (so `10 + x` with x: unsigned does not inline a Complex impl).
        let declared = |f: &ast::FnDecl, rhs_arg: &Option<String>| -> Option<String> {
            let d = rhs_arg.clone().or_else(|| {
                f.params
                    .iter()
                    .find(|p| !p.is_self)
                    .and_then(|p| p.ty.as_ref())
                    .and_then(|ty| self.free_fns.type_head_key(ty))
            })?;
            Some(if d == "Self" { lhs_ty.clone() } else { d })
        };
        let f = match &rhs_ty {
            Some(r) => fns
                .iter()
                .find(|(f, a)| declared(f, a).as_deref() == Some(r.as_str()))
                .or_else(|| {
                    if r == "integer" {
                        fns.iter()
                            .find(|(f, a)| declared(f, a).as_deref() == Some(lhs_ty.as_str()))
                    } else {
                        None
                    }
                }),
            None => {
                if fns.len() == 1 {
                    fns.first()
                } else {
                    None
                }
            }
        };
        // No candidate accepted this right operand. For a packed nominal array that
        // is fine — the caller falls back to builtin arithmetic on the packed
        // word. For an aggregate struct there is nothing to fall back to: the
        // expression yields no fields, and the assignment it feeds is dropped
        // without a word, leaving only a downstream "never driven" warning
        // that names the symptom rather than the operator.
        // An *aggregate* struct is the test, not "not an array family": a
        // multi-field struct is still many signals with no packed-word
        // arithmetic to fall back on. A
        // field-less newtype (`struct Q(unsigned[8])`) is a word and is
        // correctly left to builtin arithmetic.
        // Only an *aggregate* struct. A field-less newtype is one word with
        // builtin arithmetic behind it, and std's families are exactly that
        // shape (`pub struct unsigned(Logic[])`), so `contains_key` alone
        // would report on ordinary vector expressions. Testing for "not a
        // nominal-array-family test would be wrong for unrelated aggregate
        // structs, which still have nothing to fall back on.
        if f.is_none()
            && self
                .structs
                .get(lhs_ty.as_str())
                .is_some_and(|st| !st.fields.is_empty())
        {
            self.bad_operators.borrow_mut().push((
                op.to_string(),
                lhs_ty.clone(),
                rhs_ty.clone(),
                ast::expr_span(lhs).to(ast::expr_span(rhs)),
            ));
        }
        let (f, _) = f?;
        let body = f.body.as_ref()?;

        // Bind `self` to the left operand and the first named param to the
        // right — plus each operand's bit width, so a body can say
        // `self::length` (needed for e.g. sign-aware `signed` comparison).
        let mut fenv: HashMap<String, Val> = HashMap::new();
        fenv.insert("self".to_string(), self.lower_val_env(lhs, env));
        fenv.insert(
            "self::length".to_string(),
            Val::Scalar(Expr::Const(self.ast_width(lhs) as u64)),
        );
        if let Some(p) = f.params.iter().find(|p| !p.is_self) {
            if let Some(n) = &p.name {
                fenv.insert(n.text.clone(), self.lower_val_env(rhs, env));
                fenv.insert(
                    format!("{}::length", n.text),
                    Val::Scalar(Expr::Const(
                        self.literal_aware_width(rhs, self.ast_width(lhs)) as u64,
                    )),
                );
            }
        }
        self.inline_block(&body.stmts, &fenv)
    }

    /// The written (left, right) constant bounds of a slice index: a range
    /// expression with const-evaluable bounds, or a named range constant.
    pub(super) fn slice_bounds(&self, base: &ast::Expr, index: &ast::Expr) -> Option<(i64, i64)> {
        let path = expr_path(base)?;
        let block_binding = self.block_local_binding(base);
        let declared = block_binding
            .as_ref()
            .and_then(|binding| self.declared_range(&binding.ty, &self.cur_env))
            .or_else(|| self.persisted_range(&path))?;
        match index {
            ast::Expr::Range { lo, hi, .. } => Some((
                self.eval_const(lo, &self.cur_env)?,
                self.eval_const(hi, &self.cur_env)?,
            )),
            ast::Expr::PartialRange { lo, hi, .. } => {
                let (left, right) = declared;
                Some((
                    match lo {
                        Some(lo) => self.eval_const(lo, &self.cur_env)?,
                        None => left,
                    },
                    match hi {
                        Some(hi) => self.eval_const(hi, &self.cur_env)?,
                        None => right,
                    },
                ))
            }
            ast::Expr::Path(constant) => {
                if let Some(bounds) = self
                    .free_fns
                    .constant_path_key(constant)
                    .and_then(|key| self.const_ranges.get(&key).copied())
                {
                    Some(bounds)
                } else if self.locals.contains_key(&path)
                    || block_binding
                        .as_ref()
                        .is_some_and(|binding| matches!(binding.value, Val::Scalar(_)))
                {
                    let n = self.eval_const(index, &self.cur_env)?;
                    Some((n, n))
                } else {
                    None
                }
            }
            // A single constant index is the one-bit slice `w[n..n]`, but only
            // on a packed vector — which is one signal. An array's elements are
            // signals of their own and `a[2]` resolves through those, so the
            // base having its own entry in `locals` is what tells them apart.
            _ if self.locals.contains_key(&path)
                || block_binding.is_some_and(|binding| matches!(binding.value, Val::Scalar(_))) =>
            {
                let n = self.eval_const(index, &self.cur_env)?;
                Some((n, n))
            }
            _ => None,
        }
    }

    /// Map a vector's declared index labels onto its zero-based storage bits.
    /// A nonzero range keeps numeric significance: `unsigned[15..8]` stores
    /// label 8 in bit 0 and label 15 in bit 7. Direction still controls slice
    /// ordering, but does not waste storage below the declared low bound.
    pub(super) fn packed_positions(&self, path: &str) -> Option<Vec<(i64, u32)>> {
        if matches!(
            self.persisted_layout(path).map(|layout| &layout.kind),
            Some(LayoutKind::Array { .. })
        ) {
            return None;
        }
        let (left, right) = self.persisted_range(path)?;
        let low = left.min(right);
        let high = left.max(right);
        let signal = *self.locals.get(path)?;
        let width = self.out.signals[signal.0 as usize].width;
        let span = i128::from(high) - i128::from(low) + 1;
        if span != i128::from(width) {
            return None;
        }
        (low..=high)
            .map(|logical| {
                u32::try_from(i128::from(logical) - i128::from(low))
                    .ok()
                    .map(|physical| (logical, physical))
            })
            .collect()
    }

    /// The `(index, offset)` positions of a packed type's elements.
    pub(super) fn block_packed_positions(&self, ty: &ast::Type) -> Option<Vec<(i64, u32)>> {
        let (left, right) = self.declared_range(ty, &self.cur_env)?;
        let low = left.min(right);
        let high = left.max(right);
        let width = self.block_local_width(ty);
        let span = i128::from(high) - i128::from(low) + 1;
        if span != i128::from(width) {
            return None;
        }
        (low..=high)
            .map(|logical| {
                u32::try_from(i128::from(logical) - i128::from(low))
                    .ok()
                    .map(|physical| (logical, physical))
            })
            .collect()
    }

    /// Constant source bounds translated from declared labels to packed
    /// storage positions. Arrays are excluded because their elements are
    /// separate signals rather than bits of one scalar.
    pub(super) fn storage_slice_bounds(
        &self,
        base: &ast::Expr,
        index: &ast::Expr,
    ) -> Option<(u32, u32)> {
        let named = expr_path(base).and_then(|path| {
            let range = self
                .block_local_binding(base)
                .and_then(|binding| self.declared_range(&binding.ty, &self.cur_env))
                .or_else(|| self.persisted_range(&path))?;
            Some((self.slice_bounds(base, index)?, range))
        });
        let ((a, b), (left, right)) = if let Some(named) = named {
            named
        } else {
            // A computed packed value has no declaration path from which to
            // recover labels. Its result uses the array family's canonical
            // zero-based storage labels, so an explicit/partial slice can still
            // select it (`(a + b)[3..0]`) instead of lowering to `Unknown`.
            if !matches!(
                self.expr_types.get(&ast::expr_span(base)),
                Some(crate::types::Ty::Array {
                    family: Some(_),
                    ..
                })
            ) {
                return None;
            }
            let width = self.ast_width(base);
            if width == 0 {
                return None;
            }
            let declared = (i64::from(width - 1), 0);
            let bounds = match index {
                ast::Expr::Range { lo, hi, .. } => (
                    self.eval_const(lo, &self.cur_env)?,
                    self.eval_const(hi, &self.cur_env)?,
                ),
                ast::Expr::PartialRange { lo, hi, .. } => (
                    match lo.as_deref() {
                        Some(lo) => self.eval_const(lo, &self.cur_env)?,
                        None => declared.0,
                    },
                    match hi.as_deref() {
                        Some(hi) => self.eval_const(hi, &self.cur_env)?,
                        None => declared.1,
                    },
                ),
                ast::Expr::Path(_) => {
                    let value = self.eval_const(index, &self.cur_env)?;
                    (value, value)
                }
                _ => {
                    let value = self.eval_const(index, &self.cur_env)?;
                    (value, value)
                }
            };
            (bounds, declared)
        };
        let low = left.min(right);
        let high = left.max(right);
        if a < low || a > high || b < low || b > high {
            return None;
        }
        let to_storage = |label: i64| {
            let offset = i128::from(label) - i128::from(low);
            u32::try_from(offset).ok()
        };
        Some((to_storage(a)?, to_storage(b)?))
    }

    /// Lower indexing through a type's own index contract rather than the
    /// built-in array form.
    pub(super) fn lower_custom_index(&self, base: &ast::Expr, index: &ast::Expr) -> Option<Expr> {
        let arg = self.index_argument(index)?;
        let span = ast::expr_span(index);
        let callee = ast::Expr::Field {
            base: Box::new(base.clone()),
            field: ast::Ident {
                text: "index".to_string(),
                span,
            },
            span,
        };
        match self.lower_method_call(&callee, &[arg], &HashMap::new())? {
            Val::Scalar(value) => Some(value),
            Val::Fields(_) => None,
        }
    }

    /// The argument a custom index passes to its contract.
    pub(super) fn index_argument(&self, index: &ast::Expr) -> Option<ast::Expr> {
        let ast::Expr::Range { lo, hi, span } = index else {
            return (!matches!(index, ast::Expr::PartialRange { .. })).then(|| index.clone());
        };
        let path = ast::Path {
            segments: vec![ast::Ident {
                text: "Range".to_string(),
                span: *span,
            }],
            span: *span,
        };
        let field = |name: &str, value: ast::Expr| ast::ConnectArg {
            field: Some(ast::Ident {
                text: name.to_string(),
                span: *span,
            }),
            value: Some(value),
            span: *span,
        };
        Some(ast::Expr::Construct {
            ty: Some(ast::Type::Path(path)),
            args: vec![
                field("left", lo.as_ref().clone()),
                field("right", hi.as_ref().clone()),
            ],
            spread: None,
            span: *span,
        })
    }

    /// Resolve an exact `using` alias chain without looping on malformed
    /// cyclic declarations (which resolution diagnoses separately).
    pub(super) fn resolve_alias<'t>(&'t self, ty: &'t ast::Type) -> &'t ast::Type {
        let mut ty = ty;
        let mut seen = std::collections::HashSet::new();
        loop {
            let ast::Type::Path(path) = ty else {
                return ty;
            };
            let Some(name) = self.free_fns.type_alias_path_key(path) else {
                return ty;
            };
            if !seen.insert(name.clone()) {
                return ty;
            }
            let Some(alias) = self.aliases.get(&name) else {
                return ty;
            };
            ty = alias;
        }
    }

    /// Whether a type resolves to `expected` after alias expansion.
    pub(super) fn type_resolves_to(&self, ty: &ast::Type, expected: &str) -> bool {
        let mut ty = ty;
        let mut seen = std::collections::HashSet::new();
        loop {
            let Some(name) = type_head_name(ty) else {
                return false;
            };
            if name == expected {
                return true;
            }
            let ast::Type::Path(path) = ty else {
                return false;
            };
            let Some(key) = self.free_fns.type_alias_path_key(path) else {
                return false;
            };
            if !seen.insert(key.clone()) {
                return false;
            }
            let Some(alias) = self.aliases.get(&key) else {
                return false;
            };
            ty = alias;
        }
    }

    /// Declare `name` as a `Char[n]` array (string-literal inference).
    pub(super) fn add_char_array(
        &mut self,
        entity: &str,
        name: &str,
        n: usize,
        declaration_span: crate::diag::Span,
    ) {
        self.local_array
            .insert(name.to_string(), (0..n as i64).collect());
        for i in 0..n {
            let elem = format!("{name}[{i}]");
            self.add_signal(entity, &elem, 32, declaration_span);
            if let Some(&id) = self.locals.get(&elem) {
                self.out.signals[id.0 as usize].char = true;
            }
            self.local_char.insert(elem);
        }
    }

    /// Resolve a character literal against its counterpart's type (the
    /// literal has no identity of its own): a `Char` counterpart reads it
    /// through the Unicode table (code point); an enum counterpart reads it
    /// as the matching variant. `None` keeps the default logic-literal form.
    pub(super) fn typed_char_literal(&self, c: char, other: &ast::Expr) -> Option<Expr> {
        let t = self.operand_type_name(other)?;
        if t == "Char" {
            return Some(Expr::Const(c as u32 as u64));
        }
        let vars = self.enum_variants.get(&t)?;
        vars.get(&format!("'{c}'")).map(|&d| Expr::Const(d))
    }

    /// A char literal's value in enum `en` — its position in that enum's own
    /// declaration (VHDL `T'pos`), from `enum_variants`. Char variants are keyed
    /// with quotes (`'g'`). `None` if `en` has no such variant.
    pub(super) fn char_disc(&self, ch: char, en: &str) -> Option<u64> {
        self.enum_variant(en, &format!("'{ch}'"))
    }

    /// The discriminant of `variant` in enum `en`, from std's declaration —
    /// the one place enum values come from. `None` if either is unknown.
    pub(super) fn enum_variant(&self, en: &str, variant: &str) -> Option<u64> {
        self.enum_variants
            .get(en)
            .and_then(|m| m.get(variant))
            .copied()
    }

    /// The discriminant an `Enum::Variant` path names.
    pub(super) fn enum_variant_path(&self, path: &ast::Path) -> Option<u64> {
        let (enumeration, variant) = self.free_fns.enum_variant_key(path)?;
        self.enum_variant(&enumeration, &variant)
    }

    /// Decode a bit-string literal into `(value, discs)`, MSB-first: `value` is
    /// the per-element 0/1 bit (element *i* at bit *i*); `discs` is the full
    /// per-element `std_ulogic` discriminant packed 4 bits each (element *i* at
    /// nibble *i*), so a metavalue's exact value survives. A hex string (`x"…"`)
    /// is pure 2-value. This is the front-end half of X/Z vector support (see
    /// "X/Z propagation through vectors" in `docs/simulation.md`); `discs` is
    /// stored in the element-container companion.
    pub(super) fn decode_bit_string(&self, base: char, digits: &str) -> (u64, u64) {
        let (value, discs) = self.decode_bit_string_words(base, digits);
        (
            value.first().copied().unwrap_or(0),
            discs.first().copied().unwrap_or(0),
        )
    }

    /// Arbitrary-width counterpart of [`Self::decode_bit_string`]. Both
    /// results are low-word-first, with one value bit and one discriminant
    /// nibble per source element respectively.
    pub(super) fn decode_bit_string_words(&self, base: char, digits: &str) -> (Vec<u64>, Vec<u64>) {
        // Radix-expanded 2-value strings: hex is 4 bits/digit, octal 3.
        if let Some(bits) = crate::syntax::is_radix_prefix(base)
            .then(|| crate::syntax::bits_per_digit(base) as usize)
        {
            // `_` is a separator, not a digit: it contributes no bits and
            // must not shift the ones after it.
            let width = crate::syntax::radix_digits(digits)
                .count()
                .saturating_mul(bits);
            let mut value = vec![0u64; width.div_ceil(64).max(1)];
            let mut discs = vec![0u64; width.saturating_mul(4).div_ceil(64).max(1)];
            for (digit_index, ch) in crate::syntax::radix_digits(digits).rev().enumerate() {
                let digit = ch.to_digit(crate::syntax::radix_of(base)).unwrap_or(0);
                for bit in 0..bits {
                    let pos = digit_index * bits + bit;
                    let value_bit = u64::from((digit & (1 << bit)) != 0);
                    value[pos / 64] |= value_bit << (pos % 64);
                    discs[(4 * pos) / 64] |= value_bit << ((4 * pos) % 64);
                }
            }
            return (value, discs);
        }
        let n = digits.len();
        let mut value = vec![0u64; n.div_ceil(64).max(1)];
        let mut discs = vec![0u64; n.saturating_mul(4).div_ceil(64).max(1)];
        for (i, ch) in digits.chars().enumerate() {
            let pos = n - 1 - i; // MSB-first: first digit is the top bit
            let disc = self.char_disc(ch, DEFAULT_LOGIC_TYPE).unwrap_or(0);
            let bit = self
                .logic_encoding(DEFAULT_LOGIC_TYPE)
                .and_then(|encoding| encoding.value_bit(disc))
                .unwrap_or(0);
            value[pos / 64] |= bit << (pos % 64);
            discs[(4 * pos) / 64] |= (disc & 0xF) << ((4 * pos) % 64);
        }
        (value, discs)
    }

    /// The `(base, digits)` of a bit-string-like value: an explicit radix
    /// literal `x"…"`/`o"…"`, or a plain string (internal base `'b'`, per-char
    /// binary) — a string of logic values reads as a logic array, no prefix.
    pub(super) fn bit_string_parts(e: &ast::Expr) -> Option<(char, &str)> {
        match e {
            ast::Expr::BitStrLit { base, digits, .. } => Some((*base, digits)),
            ast::Expr::StrLit { text, .. } => Some(('b', text)),
            _ => None,
        }
    }

    /// Preserve the discriminant plane of a metavalue-carrying bit string
    /// alongside its value driver until [`Self::propagate_metavalues`] creates
    /// the target's companion. A raw `Const`/`WideConst` retains only the low
    /// value bit of each element and cannot recover whether that bit was `X`,
    /// `Z`, or another nine-value symbol.
    pub(super) fn bit_string_meta(&self, e: &ast::Expr) -> Option<Expr> {
        let (base, digits) = Self::bit_string_parts(e)?;
        let (_, discs) = self.decode_bit_string_words(base, digits);
        self.has_metavalue(&discs).then(|| words_const(discs))
    }

    /// The std-derived encoding for a logic type, which owns the value table
    /// rather than the backend.
    pub(super) fn logic_encoding(&self, ty: &str) -> Option<&LogicEncoding> {
        self.logic_encodings.get(ty)
    }

    /// Whether any discriminant in the set is a metavalue, so a companion plane
    /// is needed.
    pub(super) fn has_metavalue(&self, discs: &[u64]) -> bool {
        let Some(encoding) = self.logic_encoding(DEFAULT_LOGIC_TYPE) else {
            return false;
        };
        (0..discs.len() * 16).any(|element| {
            let disc = (discs[element / 16] >> (4 * (element % 16))) & 0xF;
            !encoding.binary.contains(&disc)
        })
    }

    /// The discriminant of `'X'`, from std's encoding rather than a constant.
    pub(super) fn x_disc(&self) -> u64 {
        self.logic_encoding(DEFAULT_LOGIC_TYPE)
            .and_then(LogicEncoding::canonical_unknown)
            .unwrap_or(0)
    }

    /// Rewrite every `Expr::Logic(c)` left in the design — those a typed context
    /// (enum signal, comparison counterpart) did not already resolve — to its
    /// position in std's [`DEFAULT_LOGIC_TYPE`]. After this the backends see
    /// only `Const`s, so no engine hardcodes what `'0'`/`'Z'`/… mean.
    pub(super) fn normalize_logic_literals(&mut self) {
        let lut = self
            .enum_variants
            .get(DEFAULT_LOGIC_TYPE)
            .cloned()
            .unwrap_or_default();
        for d in &mut self.out.drivers {
            if let Some(c) = &mut d.cond {
                resolve_logic_expr(c, &lut);
            }
            resolve_logic_expr(&mut d.expr, &lut);
        }
        for b in &mut self.out.event_blocks {
            resolve_logic_expr(&mut b.condition, &lut);
            for u in &mut b.updates {
                if let Some(c) = &mut u.cond {
                    resolve_logic_expr(c, &lut);
                }
                resolve_logic_expr(&mut u.expr, &lut);
            }
        }
    }

    /// Coerce a driven value to the target's representation: integer
    /// constants become f64 bits when the target signal is `real`.
    pub(super) fn coerce_to_target(&self, target: SignalId, expr: Expr) -> Expr {
        let sig = &self.out.signals[target.0 as usize];
        // A char literal assigned to an enum-typed signal takes that variant's
        // position in the enum's *own* declaration (VHDL `T'pos`) — data-driven
        // from `enum_variants`, not a hardcoded Logic map, so a user char enum
        // (`enum Color { 'r','g','b' }`) resolves correctly.
        if let (Some(en), Expr::Logic(c)) = (&sig.enum_type, &expr) {
            if let Some(d) = self.char_disc(*c, en) {
                return Expr::Const(d);
            }
        }
        if sig.char {
            if let Expr::Logic(c) = expr {
                return Expr::Const(c as u32 as u64);
            }
        }
        if sig.real {
            self.coerce_real(expr)
        } else {
            expr
        }
    }

    /// Whether a lowered expression produces f64-bit (`real`) values.
    pub(super) fn is_real_expr(&self, e: &Expr) -> bool {
        match e {
            Expr::Real(_) => true,
            Expr::Current(id) | Expr::Old(id) => self.out.signals[id.0 as usize].real,
            Expr::Binary { op, .. } => {
                matches!(op, BinOp::FAdd | BinOp::FSub | BinOp::FMul | BinOp::FDiv)
            }
            Expr::Select { then, els, .. } => self.is_real_expr(then) || self.is_real_expr(els),
            Expr::CCall { f64_ret, .. } => *f64_ret,
            _ => false,
        }
    }

    /// Reinterpret an integer value flowing into a real context (`.re = 10`,
    /// `self.re + 3`, a constant-folded `10 + 0`) as its f64 form: constants
    /// convert, integer arithmetic becomes float arithmetic, selects recurse.
    pub(super) fn coerce_real(&self, e: Expr) -> Expr {
        if self.is_real_expr(&e) {
            return e;
        }
        match e {
            Expr::Const(v) => Expr::Real(v as f64),
            Expr::Unary { op: UnOp::Neg, rhs } => Expr::Binary {
                op: BinOp::FSub,
                lhs: Box::new(Expr::Real(0.0)),
                rhs: Box::new(self.coerce_real(*rhs)),
            },
            Expr::Select { cond, then, els } => Expr::Select {
                cond,
                then: Box::new(self.coerce_real(*then)),
                els: Box::new(self.coerce_real(*els)),
            },
            Expr::Binary { op, lhs, rhs } => {
                let fop = match op {
                    BinOp::Add | BinOp::SAdd => Some(BinOp::FAdd),
                    BinOp::Sub | BinOp::SSub => Some(BinOp::FSub),
                    BinOp::Mul | BinOp::SMul => Some(BinOp::FMul),
                    BinOp::Div | BinOp::SDiv => Some(BinOp::FDiv),
                    _ => None,
                };
                match fop {
                    Some(f) => Expr::Binary {
                        op: f,
                        lhs: Box::new(self.coerce_real(*lhs)),
                        rhs: Box::new(self.coerce_real(*rhs)),
                    },
                    None => Expr::Binary { op, lhs, rhs },
                }
            }
            e => e,
        }
    }

    /// A unary node, switching negation to float form when the operand is
    /// real. `UnOp::Neg` negates a *word*, and a real carries f64 bits, so
    /// `-2.5` produced the two's-complement of the bit pattern — a different
    /// number entirely, and one that compared unequal to `0.0 - 2.5`.
    pub(super) fn make_unary(&self, op: ast::UnOp, rhs: Expr) -> Expr {
        if matches!(op, ast::UnOp::Neg) && self.is_real_expr(&rhs) {
            return Expr::Binary {
                op: BinOp::FSub,
                lhs: Box::new(Expr::Real(0.0)),
                rhs: Box::new(rhs),
            };
        }
        Expr::Unary {
            op: lower_unop(op),
            rhs: Box::new(rhs),
        }
    }

    /// Build a binary node, switching `+ - * /` to float arithmetic (and
    /// coercing integer constants) when either operand is real. `==`/`!=`
    /// compare f64 bits exactly, which is right once constants are coerced.
    /// Mark a comparison whose operands are metavalue-capable vectors, so the
    /// `numeric_std` rule can be applied once companions exist. Returns the
    /// value unchanged for anything else -- a scalar, an integer, a comparison
    /// on a type that has no metavalue plane.
    pub(super) fn mark_vector_compare(
        &self,
        op: &ast::BinOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        built: Expr,
    ) -> Expr {
        use ast::BinOp as A;
        if !matches!(op, A::Eq | A::Ne | A::Lt | A::Le | A::Gt | A::Ge) {
            return built;
        }
        let vector = |e: &ast::Expr| {
            self.operand_type_name(e)
                .is_some_and(|f| self.out.array_element_of_family.contains_key(&f))
        };
        if !vector(lhs) && !vector(rhs) {
            return built;
        }
        Expr::MetaCmp {
            ne: matches!(op, A::Ne),
            operands: vec![self.lower_expr(lhs), self.lower_expr(rhs)],
            inner: Box::new(built),
        }
    }

    /// Build a binary IR node, selecting the unsigned, signed or float form from
    /// the operand kinds.
    pub(super) fn make_binary(
        &self,
        op: ast::BinOp,
        lhs: Expr,
        rhs: Expr,
        integer: bool,
        declared: bool,
    ) -> Expr {
        if self.is_real_expr(&lhs) || self.is_real_expr(&rhs) {
            let (lhs, rhs) = (self.coerce_real(lhs), self.coerce_real(rhs));
            let op = match op {
                ast::BinOp::Add => BinOp::FAdd,
                ast::BinOp::Sub => BinOp::FSub,
                ast::BinOp::Mul => BinOp::FMul,
                ast::BinOp::Div => BinOp::FDiv,
                // Comparisons need ordered float semantics, not integer compare
                // on the bit patterns (which misorders negatives / `±0.0`).
                ast::BinOp::Eq => BinOp::FEq,
                ast::BinOp::Ne => BinOp::FNe,
                ast::BinOp::Lt => BinOp::FLt,
                ast::BinOp::Le => BinOp::FLe,
                ast::BinOp::Gt => BinOp::FGt,
                ast::BinOp::Ge => BinOp::FGe,
                other => match lower_binop(other) {
                    Some(op) => op,
                    None => return Expr::Unknown,
                },
            };
            return Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        // Generic/library impl bodies can leave their parameter expressions
        // typed as the kernel default even after substitution. The concrete
        // lowered signal is authoritative: a `signed[N]`, `unsigned[N]`, Bit,
        // enum, or user-newtype signal must keep the implementation's raw
        // vector operations rather than inherit kernel-integer signedness.
        // A declared kernel-integer operand overrides the signal test: what
        // its body happened to read does not change the type it returns.
        let integer = integer
            && (declared
                || !self.has_non_integer_signal(&lhs) && !self.has_non_integer_signal(&rhs));
        match lower_binop(op) {
            Some(op) => {
                let op = if integer {
                    match op {
                        BinOp::Add => BinOp::SAdd,
                        BinOp::Sub => BinOp::SSub,
                        BinOp::Mul => BinOp::SMul,
                        BinOp::Div => BinOp::SDiv,
                        BinOp::Shr => BinOp::AShr,
                        BinOp::Lt => BinOp::SLt,
                        BinOp::Le => BinOp::SLe,
                        BinOp::Gt => BinOp::SGt,
                        BinOp::Ge => BinOp::SGe,
                        op => op,
                    }
                } else {
                    op
                };
                Expr::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                }
            }
            None => Expr::Unknown,
        }
    }

    /// Whether an expression reads a signal that is not a kernel `integer`,
    /// which decides whether signed operators apply.
    pub(super) fn has_non_integer_signal(&self, e: &Expr) -> bool {
        match e {
            Expr::MetaCmp { inner, .. } => self.has_non_integer_signal(inner),
            Expr::Current(id) | Expr::Old(id) => !self.out.signals[id.0 as usize].integer,
            Expr::Event(_) => true,
            Expr::Unary { rhs, .. } | Expr::Slice { base: rhs, .. } => {
                self.has_non_integer_signal(rhs)
            }
            Expr::TableLookup { index, .. } => self.has_non_integer_signal(index),
            Expr::CheckedIndex { index, .. } => self.has_non_integer_signal(index),
            Expr::Binary { lhs, rhs, .. } => {
                self.has_non_integer_signal(lhs) || self.has_non_integer_signal(rhs)
            }
            // A select's condition controls which value flows but does not
            // participate in that value's numeric representation.
            Expr::Select { then, els, .. } => {
                self.has_non_integer_signal(then) || self.has_non_integer_signal(els)
            }
            // Argument representations do not determine a foreign call's
            // declared return type.
            Expr::CCall { .. } => false,
            Expr::Const(_)
            | Expr::WideConst(_)
            | Expr::Real(_)
            | Expr::Logic(_)
            | Expr::Unknown => false,
        }
    }

    /// Whether an operand *declares* itself a kernel integer, so the
    /// representation of the signals inside it says nothing about the
    /// operation's signedness.
    ///
    /// `sext(x)` exists to turn a `signed[8]` into a negative kernel value,
    /// and `integer(r)` to truncate a `real` to one. Both are inlined, so the
    /// `signed[8]` (or `real`) signal they read is still visible in the
    /// lowered tree — and the guard below, seeing a non-integer signal, forced
    /// the comparison unsigned. `if sext(x) < 0` was therefore false for every
    /// negative `x`, and `abs(sext(x))` returned 251 for -5.
    ///
    /// This is the reasoning the `CCall` arm of `has_non_integer_signal`
    /// already carries for a foreign call: argument representations do not
    /// determine a declared return type. An inlined siox call is no different.
    pub(super) fn declares_kernel_integer(&self, e: &ast::Expr) -> bool {
        if self
            .block_local_type(e)
            .and_then(|ty| self.free_fns.type_head_key(&ty))
            .is_some_and(|name| name == "integer")
        {
            return true;
        }
        // A parameter of the function being inlined, declared `integer`. The
        // value bound to it may read a `signed[N]` signal (`abs(sext(x))`),
        // and that must not decide the body's signedness either.
        if expr_path(e).is_some_and(|n| self.param_integers.borrow().contains(&n)) {
            return true;
        }
        let ast::Expr::Call { callee, .. } = e else {
            return false;
        };
        // The kernel conversion `integer(x)`, whose whole purpose is to produce
        // a signed kernel value (`integer(r)` truncates a `real`, which may be
        // negative). It reads the real/vector signal it converts, so the signal
        // scan would otherwise force the comparison unsigned. (This needs the
        // matching `fit_signed` on the LLVM side of `RealToInt`: the two are
        // interdependent — a signed compare that zero-extends its operand is no
        // better than an unsigned one.)
        if let ast::Expr::Path(p) = callee.as_ref() {
            if p.segments.len() == 1 && p.segments[0].text == "integer" {
                return true;
            }
        }
        // A module function whose declared return type is `integer`.
        self.free_fns
            .get(callee)
            .and_then(|f| f.ret.as_ref())
            .and_then(type_head_name)
            == Some("integer")
    }

    /// Whether a binary operation uses kernel-`integer` semantics, from both
    /// operands.
    pub(super) fn binary_uses_kernel_integer(
        &self,
        lhs_ast: &ast::Expr,
        rhs_ast: &ast::Expr,
    ) -> bool {
        // An explicit kernel-integer declaration is authoritative. In
        // particular, the type table may describe `integer(real_value)` with
        // the source family after inlining; rejecting arrays first made a
        // direct negative comparison unsigned even though assigning the same
        // conversion to an integer local worked.
        if self.declares_kernel_integer(lhs_ast) || self.declares_kernel_integer(rhs_ast) {
            return true;
        }
        let lhs = self.expr_types.get(&ast::expr_span(lhs_ast));
        let rhs = self.expr_types.get(&ast::expr_span(rhs_ast));
        // Literals retain their default `integer` type even when they occur
        // inside an inlined library-vector implementation. A concrete
        // array/newtype operand owns that operation through std; it must not be
        // reinterpreted as a signed kernel-integer operation merely because
        // its other operand happens to be an integer literal.
        if matches!(lhs, Some(crate::types::Ty::Array { .. }))
            || matches!(rhs, Some(crate::types::Ty::Array { .. }))
        {
            return false;
        }
        // A parameter of the function being inlined, declared `integer`. Its
        // recorded type is `Error` (the body is checked without parameters in
        // scope), so without this the declaration is simply not consulted.
        let declared_integer =
            |e: &ast::Expr| expr_path(e).is_some_and(|n| self.param_integers.borrow().contains(&n));
        matches!(lhs, Some(crate::types::Ty::Integer))
            || matches!(rhs, Some(crate::types::Ty::Integer))
            || declared_integer(lhs_ast)
            || declared_integer(rhs_ast)
    }

    /// Derive a comparison from the three-way `<=>` impl (spaceship, spec
    /// 3.25): `a < b` becomes `(a <=> b) == Ordering::Less`, etc. The impl
    /// returns std::ops' `Ordering { Less, Equal, Greater }` (0/1/2), so no
    /// signed arithmetic is needed. `None` when the operand type has no
    /// `<=>` impl — built-in comparison applies.
    pub(super) fn inline_cmp(
        &self,
        op_str: &str,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        env: &HashMap<String, Val>,
    ) -> Option<Expr> {
        // (`Ordering` variant to compare against, negate?). The discriminant
        // comes from std's `Ordering` enum, not a baked-in 0/1/2 — the fallback
        // is only the conventional layout for a std-less unit test.
        let (variant, fallback, ne) = match op_str {
            "<" => ("Less", 0u64, false),
            "==" => ("Equal", 1, false),
            ">" => ("Greater", 2, false),
            ">=" => ("Less", 0, true),
            "!=" => ("Equal", 1, true),
            "<=" => ("Greater", 2, true),
            _ => return None,
        };
        let want = self.enum_variant("Ordering", variant).unwrap_or(fallback);
        let Val::Scalar(cmp) = self.inline_op("<=>", lhs, rhs, env)? else {
            return None;
        }; // -> Ord::cmp
        Some(Expr::Binary {
            op: if ne { BinOp::Ne } else { BinOp::Eq },
            lhs: Box::new(cmp),
            rhs: Box::new(Expr::Const(want)),
        })
    }

    /// Inline a unary operator impl (`not a`): binds only `self`.
    pub(super) fn inline_unary(&self, op: &str, rhs: &ast::Expr) -> Option<Val> {
        let ty = self.operand_type_name(rhs)?;
        let tr = op;
        let fns = self.op_impls.get(&(tr.to_string(), ty))?;
        let (f, _) = fns.first()?;
        let body = f.body.as_ref()?;
        let mut env: HashMap<String, Val> = HashMap::new();
        env.insert("self".to_string(), self.lower_val_env(rhs, &HashMap::new()));
        env.insert(
            "self::length".to_string(),
            Val::Scalar(Expr::Const(self.ast_width(rhs) as u64)),
        );
        self.inline_block(&body.stmts, &env)
    }

    /// Synthesize a total derivation conversion `target(x)` when no explicit
    /// `From` impl exists (spec: derived types §14). Two total cases:
    ///  - enums connected by a derivation chain where every source variant
    ///    exists in the target — representation-identity (base-first
    ///    discriminants), so the value passes through unchanged;
    ///  - a source struct that derives (transitively) from the target struct
    ///    — project onto the inherited fields.
    pub(super) fn derived_conversion(
        &self,
        target: &str,
        src: Option<&str>,
        arg: &ast::Expr,
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let src = src?;
        // Enum case: chain-connected and source variants subset of target.
        if let (Some(sv), Some(tv)) = (self.enum_variants.get(src), self.enum_variants.get(target))
        {
            let connected = self.enum_ancestor(src, target) || self.enum_ancestor(target, src);
            let total = sv.keys().all(|v| tv.contains_key(v));
            if connected && total {
                return Some(self.lower_val_env(arg, env)); // identity
            }
            return None;
        }
        // Struct case: project a derived struct onto its base fields by
        // reading the source's per-field signals (a bare struct path isn't
        // itself a Val::Fields).
        if self.struct_derives_from(src, target) {
            let base = expr_path(arg)?;
            let fields = self
                .struct_field_names(target)
                .into_iter()
                .map(|n| {
                    let expr = self
                        .locals
                        .get(&format!("{base}.{n}"))
                        .map(|&id| Expr::Current(id))
                        .unwrap_or(Expr::Unknown);
                    (n, expr)
                })
                .collect();
            return Some(Val::Fields(fields));
        }
        None
    }

    /// Whether `anc` is a (transitive) enum-derivation ancestor of `name`.
    pub(super) fn enum_ancestor(&self, anc: &str, name: &str) -> bool {
        let mut cur = name.to_string();
        let mut seen = HashSet::new();
        while let Some(b) = self.enum_bases.get(&cur) {
            if !seen.insert(cur.clone()) {
                break;
            }
            if b == anc {
                return true;
            }
            cur = b.clone();
        }
        false
    }

    /// Whether struct `name` derives (transitively) from struct `base`.
    pub(super) fn struct_derives_from(&self, name: &str, base: &str) -> bool {
        let mut cur = name.to_string();
        let mut seen = HashSet::new();
        while let Some(s) = self.structs.get(&cur) {
            if !seen.insert(cur.clone()) {
                break;
            }
            let Some(b) = s
                .base
                .as_ref()
                .and_then(|ty| self.free_fns.type_head_key(ty))
            else {
                return false;
            };
            if b == base {
                return true;
            }
            cur = b;
        }
        false
    }

    /// A struct type's full (inherited + own) field names, base chain first.
    pub(super) fn struct_field_names(&self, name: &str) -> Vec<String> {
        self.struct_field_names_at(name, &mut HashSet::new())
    }

    /// Cycle-safe, like [`Self::struct_derives_from`]: a cyclic derivation is
    /// reported by resolve, but lowering still runs best-effort.
    pub(super) fn struct_field_names_at(
        &self,
        name: &str,
        seen: &mut HashSet<String>,
    ) -> Vec<String> {
        if !seen.insert(name.to_string()) {
            return Vec::new();
        }
        let Some(s) = self.structs.get(name) else {
            return Vec::new();
        };
        let mut out = match s
            .base
            .as_ref()
            .and_then(|ty| self.free_fns.type_head_key(ty))
        {
            Some(b) => self.struct_field_names_at(&b, seen),
            None => Vec::new(),
        };
        out.extend(s.fields.iter().map(|f| f.name.text.clone()));
        seen.remove(name);
        out
    }

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
