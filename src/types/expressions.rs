//! `check_expr`: the walk that records a checked type for every expression.

use super::*;

impl<'a> Checker<'a> {
    /// Walk an expression for the Phase-2 `::ddt` guard (the only expression-
    /// local check so far).
    pub(super) fn check_expr(&mut self, e: &Expr, sym: &HashMap<String, Ty>) {
        match e {
            Expr::SysAttr { base, attr, span } => {
                if PHASE2_ATTRS.contains(&attr.text.as_str()) {
                    self.error(
                        codes::PHASE2_SYNTAX,
                        *span,
                        format!(
                            "`'{}` is Phase-2 analogue syntax, not available in Phase 1",
                            attr.text
                        ),
                    );
                    // The whole construct is unavailable in this phase. Do
                    // not descend into its receiver and turn one rejected
                    // analogue expression into unrelated value/type errors.
                    return;
                }
                // Anything outside the implemented set is reported here.
                // Silently lowering it produced an `Unknown` that only failed
                // at codegen, naming a driver index rather than the attribute.
                let a = attr.text.as_str();
                if !PHASE2_ATTRS.contains(&a) && !SYS_ATTRS.contains(&a) {
                    // The edge helpers are ordinary trait methods now, so this
                    // is the one wrong attribute worth a migration hint.
                    let help = if matches!(a, "rising" | "falling" | "edge") {
                        format!("the edge helpers are `ClockLike` methods now — write `.{a}()`")
                    } else {
                        format!(
                            "known system attributes: {}",
                            SYS_ATTRS
                                .iter()
                                .map(|s| format!("`'{s}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    self.error_with_help(
                        codes::UNKNOWN_NAME,
                        *span,
                        format!("unknown system attribute `'{a}`"),
                        help,
                    );
                }
                if matches!(attr.text.as_str(), "event" | "old") {
                    let base_ty = self.type_of(base, sym);
                    // An unresolved receiver (notably generic `self` in a
                    // trait method) cannot be classified here; avoid a
                    // cascading error and let its declaration/use checks speak.
                    if base_ty != Ty::Error && !self.is_digital_ty(&base_ty) {
                        self.error(
                            codes::INVALID_ATTR_TARGET,
                            *span,
                            format!(
                                "`::{}` requires a digital value, found {}",
                                attr.text,
                                ty_name(&base_ty)
                            ),
                        );
                    }
                }
                self.check_expr(base, sym);
            }
            Expr::Match {
                scrutinee,
                arms,
                span,
            } => {
                self.check_expr(scrutinee, sym);
                if matches!(self.type_of(scrutinee, sym), Ty::Void) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        expr_span(scrutinee),
                        "a procedure call has no value and cannot be matched".to_string(),
                    );
                    return;
                }
                // An expression must yield a value for every case it can meet,
                // so a missing variant matters at least as much here as in a
                // statement — and this form was not checked at all.
                self.check_arms_exhaustive(scrutinee, arms, *span, sym);
                // Unreachability was statement-only for the same reason
                // exhaustiveness was: the two forms share `MatchArm` but not
                // the code that walks it.
                self.check_unreachable_arms(arms);
                for arm in arms {
                    self.check_pattern_form(&arm.pattern);
                    if let Some(v) = arm.value_expr() {
                        self.check_expr(v, sym);
                    }
                }
                let values: Vec<&Expr> = arms.iter().filter_map(MatchArm::value_expr).collect();
                let anchor = self.value_domain_anchor(&values, sym);
                if let Some(anchor) = anchor {
                    for value in values {
                        if !self.value_expressions_compatible(anchor, value, sym) {
                            self.error(
                                codes::TYPE_MISMATCH,
                                expr_span(value),
                                format!(
                                    "match arms yield incompatible types: {} and {}",
                                    self.ty_display(&self.type_of(anchor, sym)),
                                    self.ty_display(&self.type_of(value, sym))
                                ),
                            );
                        }
                    }
                }
            }
            Expr::Field { base, field, .. } => {
                self.check_expr(base, sym);
                if matches!(self.type_of(base, sym), Ty::Void) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        expr_span(base),
                        "a procedure call has no value and therefore has no fields".to_string(),
                    );
                    return;
                }
                self.check_field_exists(base, field, sym);
            }
            Expr::Index { base, index, .. } => {
                self.check_expr(base, sym);
                match index.as_ref() {
                    Expr::PartialRange { lo, hi, .. } => {
                        if let Some(lo) = lo {
                            self.check_expr(lo, sym);
                        }
                        if let Some(hi) = hi {
                            self.check_expr(hi, sym);
                        }
                    }
                    _ => self.check_expr(index, sym),
                }
                if matches!(self.type_of(base, sym), Ty::Void)
                    || matches!(self.type_of(index, sym), Ty::Void)
                {
                    self.error(
                        codes::TYPE_MISMATCH,
                        expr_span(e),
                        "a procedure call has no value and cannot be indexed".to_string(),
                    );
                    return;
                }
                self.check_index_operand(base, index, sym);
                self.check_index_bounds(base, index, sym);
                self.check_custom_index(base, index, sym);
            }
            Expr::Range { lo, hi, .. } => {
                self.check_expr(lo, sym);
                self.check_expr(hi, sym);
            }
            Expr::PartialRange { lo, hi, span } => {
                if let Some(lo) = lo {
                    self.check_expr(lo, sym);
                }
                if let Some(hi) = hi {
                    self.check_expr(hi, sym);
                }
                self.error(
                    codes::TYPE_MISMATCH,
                    *span,
                    "a partial range needs an indexed value to supply its omitted bounds"
                        .to_string(),
                );
            }
            Expr::Unary { op, rhs, span } => {
                self.check_expr(rhs, sym);
                if matches!(self.type_of(rhs, sym), Ty::Void) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        *span,
                        "a procedure call has no value for a unary operator".to_string(),
                    );
                    return;
                }
                // `not` is per-bit boolean — bit-derived / Boolean operands only.
                if matches!(op, UnOp::Not) {
                    let t = self.type_of(rhs, sym);
                    if matches!(t, Ty::Real | Ty::Char) {
                        self.error(
                            codes::TYPE_MISMATCH,
                            *span,
                            format!(
                                "`not` is a per-bit operator; `{}` is not a bit-derived type",
                                self.ty_display(&t)
                            ),
                        );
                    }
                    if let Some(owner) = self.ty_head(&t) {
                        if !self.has_impl("not", &owner) {
                            self.error(
                                codes::TYPE_MISMATCH,
                                *span,
                                format!("`not` needs an `impl Operator<\"not\", …> for {owner}`"),
                            );
                        }
                    }
                }
            }
            Expr::IfExpr {
                cond, then, els, ..
            } => {
                // Same condition rule as statement `if` (must be Boolean).
                self.check_condition(cond, sym);
                self.check_expr(cond, sym);
                self.check_expr(then, sym);
                self.check_expr(els, sym);
                if !self.value_expressions_compatible(then, els, sym) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        expr_span(els),
                        format!(
                            "`if` branches yield incompatible types: {} and {}",
                            self.ty_display(&self.type_of(then, sym)),
                            self.ty_display(&self.type_of(els, sym))
                        ),
                    );
                }
            }
            Expr::Binary { op, lhs, rhs, span } => {
                self.check_expr(lhs, sym);
                self.check_expr(rhs, sym);
                if matches!(self.type_of(lhs, sym), Ty::Void)
                    || matches!(self.type_of(rhs, sym), Ty::Void)
                {
                    self.error(
                        codes::TYPE_MISMATCH,
                        *span,
                        "a procedure call has no value for a binary operator".to_string(),
                    );
                    return;
                }
                let comparison_handled = if is_comparison(op) {
                    self.check_comparison_fit(lhs, rhs, sym);
                    self.check_comparison_operands(op, lhs, rhs, *span, sym)
                } else {
                    false
                };
                self.check_intrinsic_binary_operands(op, lhs, rhs, *span, sym);
                // A constant zero divisor is always a mistake: hardware has no
                // trap for it, so today it just yields 0 with no complaint.
                if matches!(op, BinOp::Div) && Self::const_literal(rhs) == Some(0) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        expr_span(rhs),
                        "division by a constant zero".to_string(),
                    );
                }
                // A character literal's identity comes from its counterpart's
                // type (spec: type kernel); a numeric counterpart cannot read
                // one — conversion goes through an encoding table.
                for (lit, other) in [(lhs, rhs), (rhs, lhs)] {
                    if matches!(lit.as_ref(), Expr::CharLit { .. })
                        && matches!(
                            self.type_of(other, sym),
                            Ty::Array {
                                family: Some(_),
                                ..
                            } | Ty::Integer
                                | Ty::Real
                        )
                    {
                        self.error(
                            codes::TYPE_MISMATCH,
                            *span,
                            "a character literal has no numeric identity; convert it                              through an encoding table (std::text)"
                                .to_string(),
                        );
                    }
                }
                let op_str = crate::syntax::pretty::bin_op(op);
                if let BinOp::Custom { symbol, .. } = op {
                    let lhs_ty = self.type_of(lhs, sym);
                    let rhs_ty = self.type_of(rhs, sym);
                    // An integer literal on the right coerces to a
                    // Self-typed parameter, as it does for the symbolic
                    // operators — `a + 255` worked and `a xor 255` did not,
                    // purely because the textual half dispatches here.
                    let matching = self.operator_accepts_expr(symbol, &lhs_ty, &rhs_ty, rhs, sym);
                    // A plain array has no type head, so the exact
                    // `(symbol, owner)` lookup above cannot see the blanket
                    // `for T[]` impl that lifts the element's operator.
                    // `and`/`or` never reach here — they are built-in
                    // operators with their own array handling — which is why
                    // only the textual half of the logic family failed on
                    // arrays.
                    let lifted = is_liftable_array_key(symbol)
                        && self
                            .array_operand_element(&lhs_ty)
                            .zip(self.array_operand_element(&rhs_ty))
                            .is_some_and(|(left, right)| {
                                left == right && self.has_operator_impl(symbol, &left)
                            });
                    if !matching && !lifted {
                        self.error(
                            codes::TYPE_MISMATCH,
                            *span,
                            format!(
                                "custom operator `{symbol}` has no implementation for these operand types"
                            ),
                        );
                    }
                }
                // The core boolean operators (`and`/`or`) are "boolean,
                // per bit": on a bit array they act element-wise and return
                // the same array, on `Bool` they are plain boolean. They are
                // only meaningful on Boolean and bit-derived types — never on
                // `real` or `Char`.
                if matches!(op_str, "and" | "or") {
                    for operand in [lhs, rhs] {
                        let t = self.type_of(operand, sym);
                        // A literal is a bit-mask that coerces to the other
                        // operand's width (`b and 31`); a non-literal number
                        // (`integer`/`real`) or a `Char` is not bit-derived.
                        let is_lit = matches!(
                            operand.as_ref(),
                            Expr::Int { .. } | Expr::SuffixLit { .. } | Expr::BitStrLit { .. }
                        );
                        let bad = matches!(t, Ty::Real | Ty::Char)
                            || (matches!(t, Ty::Integer) && !is_lit);
                        if bad {
                            self.error(
                                codes::TYPE_MISMATCH,
                                *span,
                                format!(
                                    "`{op_str}` needs bit-derived operands (Bit/Logic/Bool/unsigned/signed); `{}` is a number",
                                    self.ty_display(&t)
                                ),
                            );
                            break;
                        }
                    }
                }
                // Comparing an enum-valued operand (`Bit`/`Logic`/`Bool` or a
                // user `enum`) to a bare integer literal is almost always a
                // mistake: its values are written as char/variant literals
                // (`'1'`, `Idle`), and an integer silently compares the raw
                // discriminant (`b == 1` instead of `b == '1'`). Numeric
                // vectors (`unsigned`/`signed`) legitimately compare to integers, so
                // they are excluded. (W-P008)
                if matches!(op_str, "==" | "!=") {
                    for (lit, other) in [(lhs, rhs), (rhs, lhs)] {
                        let is_int_lit =
                            matches!(lit.as_ref(), Expr::Int { text, .. } if !text.contains('.'));
                        if is_int_lit {
                            if let Some(name) = self.enum_operand_name(&self.type_of(other, sym)) {
                                let hint = match name.as_str() {
                                    "Bit" | "Logic" => {
                                        "compare against a value literal, e.g. `== '1'`"
                                    }
                                    "Bool" => {
                                        "compare against `true`/`false`, or use the value directly"
                                    }
                                    _ => "compare against a variant, e.g. `== Idle`",
                                };
                                self.warn(
                                    codes::SUSPICIOUS_LOGIC_COMPARE,
                                    *span,
                                    format!("comparing `{name}` to an integer literal"),
                                    hint,
                                );
                            }
                        }
                    }
                }
                // A user struct/enum or nominal vector operand needs an exact
                // operator-trait overload (spec 3.25); overload resolution
                // includes the right operand, not merely the impl owner.
                // Equality on enums alone stays intrinsic (a discriminant
                // compare); structs derive it from `<=>` like ordering does.
                if !matches!(op, BinOp::Custom { .. }) {
                    if let Some(name) = self.named_operand_name(lhs, sym) {
                        let intrinsic_enum_equality = matches!(op_str, "==" | "!=")
                            && self.enum_operand_name(&self.type_of(lhs, sym)).is_some();
                        let intrinsic_vector_operator = (is_comparison(op)
                            || matches!(
                                op,
                                BinOp::Add
                                    | BinOp::Sub
                                    | BinOp::Mul
                                    | BinOp::Div
                                    | BinOp::Shl
                                    | BinOp::Shr
                            ))
                            && self.is_packed_array_newtype(&name);
                        if !intrinsic_enum_equality && !intrinsic_vector_operator {
                            let operator = if is_comparison(op) { "<=>" } else { op_str };
                            let left = self.type_of(lhs, sym);
                            let right = self.type_of(rhs, sym);
                            // `Error` is the recovery type for an expression this
                            // pass cannot yet model (generic constants, composite
                            // field calls, and similar forms). Never reinterpret
                            // it as a known mismatching overload argument.
                            let accepts = matches!(right, Ty::Error)
                                || self.operator_accepts_expr(operator, &left, &right, rhs, sym);
                            if !accepts && !comparison_handled {
                                let shown_name = self.key_leaf(&name);
                                self.error(
                                    codes::TYPE_MISMATCH,
                                    *span,
                                    format!(
                                        "no `{op_str}` operator for `{shown_name}` with a right operand of type `{}`",
                                        self.ty_display(&right)
                                    ),
                                );
                            }
                        }
                    }
                }
            }
            Expr::Call {
                callee,
                type_args,
                args,
                bang,
                span,
            } => {
                // A method callee is a `Field` node, but its name is a method,
                // not a field — check the receiver and let `check_method_exists`
                // judge the name, so one mistake yields one diagnostic.
                match callee.as_ref() {
                    Expr::Field { base, .. } => self.check_expr(base, sym),
                    // A bare path in callee position is checked by
                    // `check_known_call` below. Treating it as an ordinary
                    // value here would double-report unknown functions and
                    // reject runtime/compiler-provided primitives that have
                    // no source declaration.
                    Expr::Path(_) => {}
                    _ => self.check_expr(callee, sym),
                }
                for a in args {
                    self.check_expr(a, sym);
                }
                if *bang {
                    self.check_format_arity(callee, args);
                } else if matches!(callee.as_ref(), Expr::Field { .. }) {
                    self.check_method_call(callee, args, sym);
                } else if matches!(callee.as_ref(), Expr::Path(path) if path.segments.len() > 1) {
                    self.check_associated_call(callee, args, sym);
                }
                // Reset assertion is normally level-sensitive inside the
                // design clock's event block. Edge-detecting a conventionally
                // named reset creates an accidental second clock domain.
                if let Expr::Field { base, field, .. } = callee.as_ref() {
                    let reset_name = path_string(base).is_some_and(|name| {
                        let leaf = name.rsplit('.').next().unwrap_or(&name);
                        leaf.eq_ignore_ascii_case("rst")
                            || leaf.eq_ignore_ascii_case("reset")
                            || leaf.ends_with("_rst")
                            || leaf.ends_with("_reset")
                    });
                    if reset_name && matches!(field.text.as_str(), "rising" | "falling" | "edge") {
                        self.sink.emit(
                            Diagnostic::warning(format!(
                                "reset signal is edge-detected with `.{}`",
                                field.text
                            ))
                            .with_code(codes::SUSPICIOUS_RESET)
                            .at(*span)
                            .help("test the reset level inside the design's clocked block instead"),
                        );
                    }
                }
                // A constant conversion argument must FIT the target
                // (spec 3.17/3.26): `unsigned[4](300)` is a compile-time error,
                // like `let b: Byte = 300`. Dynamic values get simulation
                // range checks later (with the S3 reporting machinery).
                self.check_conversion_fit(callee, args, e);
                self.check_conversion_arity(callee, args);
                self.check_generic_bounds(callee, args, sym);
                self.check_call_arity(callee, args, sym);
                self.check_runtime_call_contract(callee, type_args, args, *bang, sym);
            }
            Expr::Construct {
                ty, args, spread, ..
            } => {
                // An explicitly typed literal is self-describing even when it
                // is nested inside another expression (`Packet { .. }.get()`).
                // Contextual checks cover name-less literals, but relying only
                // on those consumers let nested constructions bypass private
                // representation and field-shape validation.
                if let Some(ty) = ty {
                    let expected = self.ast_ty(ty);
                    if let Some(head) = self.type_key(ty) {
                        if self.structs.contains_key(&head) {
                            self.check_struct_literal_for_head(&expected, &head, e, sym);
                        }
                    }
                }
                for c in args {
                    if let Some(v) = &c.value {
                        self.check_expr(v, sym);
                    }
                }
                if let Some(base) = spread {
                    self.check_expr(base, sym);
                }
                // `{ .a = '1', .a = '0' }` silently kept one of them.
                let mut seen: HashSet<&str> = HashSet::new();
                for c in args {
                    let Some(f) = &c.field else { continue };
                    if !seen.insert(f.text.as_str()) {
                        self.error(
                            codes::DUPLICATE_ITEM,
                            f.span,
                            format!("field `{}` is given twice in this literal", f.text),
                        );
                    }
                }
            }
            Expr::Concat { parts, .. } => {
                for p in parts {
                    self.check_expr(p, sym);
                }
            }
            Expr::Array { elems, .. } => {
                for e in elems {
                    self.check_expr(e, sym);
                }
            }
            Expr::SuffixLit { suffix, span, .. } => {
                match self.suffix_types.get(&suffix.text).map(|v| v.as_slice()) {
                    Some([_]) => {} // one `impl Suffix` fn defines it
                    Some(tys) => {
                        let list = tys
                            .iter()
                            .map(|t| format!("{t}::{}", suffix.text))
                            .collect::<Vec<_>>()
                            .join(", ");
                        self.error(
                            codes::UNKNOWN_NAME,
                            *span,
                            format!("literal suffix `{}` is ambiguous: {list}", suffix.text),
                        );
                    }
                    // No Suffix impl in scope: the fixed fs/Hz table backs
                    // bare files (spec 3.24).
                    None => {
                        if suffix_scale(&suffix.text).is_none() {
                            self.error(
                                codes::UNKNOWN_NAME,
                                *span,
                                format!("unknown literal suffix `{}`", suffix.text),
                            );
                        }
                    }
                }
            }
            Expr::BitStrLit { base, digits, span } => {
                // std owns which prefixes exist (`impl Prefix<sym, _> for T`): when it is
                // in scope, a prefix it doesn't declare is unknown. When std is
                // absent (some unit tests) the compiler still recognizes its
                // intrinsic radix prefixes — mirroring the suffix fs/Hz fallback.
                // (A plain string `"1X10"` needs no prefix; `b"…"` is gone.)
                if !self.prefix_types.is_empty()
                    && !self.prefix_types.contains_key(&base.to_string())
                {
                    self.error(
                        codes::TYPE_MISMATCH,
                        *span,
                        format!(
                            "unknown bit-string prefix `{base}` — no `impl Prefix` declares it"
                        ),
                    );
                    return;
                }
                // Evaluation is a compiler intrinsic until const string ops
                // exist, so only the known radix prefixes carry an alphabet.
                // The alphabet follows from the radix rather than being listed
                // again: a digit is well-formed exactly when `to_digit`
                // accepts it there.
                if !crate::syntax::is_radix_prefix(*base) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        *span,
                        format!("bit-string prefix `{base}` has no compiler evaluation yet"),
                    );
                    return;
                }
                let radix = crate::syntax::radix_of(*base);
                // `_` separates digits (`x"AB_CD"`) as it does in `0xAB_CD`;
                // a literal of nothing but separators is still empty.
                let significant: String = crate::syntax::radix_digits(digits).collect();
                let ok = !significant.is_empty() && significant.chars().all(|c| c.is_digit(radix));
                let kind = if radix == 16 { "hex" } else { "octal" };
                if !ok {
                    self.error(
                        codes::TYPE_MISMATCH,
                        *span,
                        format!("invalid {kind} bit-string literal `{base}\"{digits}\"`"),
                    );
                }
            }
            Expr::Path(path) => {
                let local = path
                    .segments
                    .first()
                    .is_some_and(|name| path.segments.len() == 1 && sym.contains_key(&name.text));
                // Enum variants may be used unqualified (`return Equal;`).
                // Resolution intentionally leaves those to type context, so
                // they are known values even without a span -> DefId entry.
                let unqualified_variant = path.segments.first().is_some_and(|name| {
                    path.segments.len() == 1
                        && self
                            .enum_variants
                            .values()
                            .any(|variants| variants.contains(&name.text))
                });
                if !local && !unqualified_variant && self.resolved.resolved(path.span).is_none() {
                    self.error(
                        codes::UNKNOWN_NAME,
                        path.span,
                        format!(
                            "unknown value `{}`",
                            path.segments
                                .iter()
                                .map(|segment| segment.text.as_str())
                                .collect::<Vec<_>>()
                                .join("::")
                        ),
                    );
                }
            }
            Expr::Int { .. } | Expr::CharLit { .. } | Expr::StrLit { .. } => {}
        }
    }
}
