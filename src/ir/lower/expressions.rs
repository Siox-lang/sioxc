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
}
