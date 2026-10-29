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
                // `'event` and `'old` are a signal's history: a computed value
                // (`(a and b)'event`) has none, and lowering failed (#248).
                if matches!(attr.text.as_str(), "event" | "old")
                    && !matches!(
                        base.as_ref(),
                        Expr::Path(_) | Expr::Field { .. } | Expr::Index { .. }
                    )
                {
                    self.error_with_help(
                        codes::TYPE_MISMATCH,
                        expr_span(base),
                        format!(
                            "`'{}` reads a signal's history, and `{}` is a computed value",
                            attr.text,
                            crate::syntax::pretty::expr_string(base)
                        ),
                        format!(
                            "apply it to the signals instead, e.g. `a'{} or b'{}`",
                            attr.text, attr.text
                        ),
                    );
                    return;
                }
                // Anything outside the implemented set is reported here.
                // Silently lowering it produced an `Unknown` that only failed
                // at codegen, naming a driver index rather than the attribute.
                let a = attr.text.as_str();
                let type_attr = if SYS_ATTRS.contains(&a) {
                    None
                } else {
                    self.type_attr(base, a, sym)
                        .map(|(owner, declaration)| (owner, declaration.clone()))
                };
                if let Some((owner, declaration)) = &type_attr {
                    self.check_type_attr_read(owner, declaration, *span);
                }
                // A declared attribute's read was folded into its value before
                // resolution; one left here already failed with its reason.
                if type_attr.is_none()
                    && !PHASE2_ATTRS.contains(&a)
                    && !SYS_ATTRS.contains(&a)
                    && !self.attr_value_kinds.contains_key(a)
                {
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
                self.check_arms_exhaustive(scrutinee, arms, *span, sym, true);
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
                                format!("`not` needs an `impl Not<Out> for {owner}`"),
                            );
                        }
                    }
                }
                // A struct negates only through its own `Neg` impl: `-z` on a
                // `Complex` was accepted and computed nonsense.
                if matches!(op, UnOp::Neg) {
                    if let Ty::Named(definition) = self.type_of(rhs, sym) {
                        let owner = self.resolved.def(definition).map(|d| d.name.clone());
                        let is_struct = self.resolved.kind_of(definition) == Some(DefKind::Struct);
                        // Impls are keyed by the qualified name: the bare
                        // one found none, so `-p` was refused (#171).
                        let key = self.definition_key(definition).unwrap_or_default();
                        if let Some(owner) = owner.filter(|_| {
                            is_struct
                                && !self.is_packed_array_newtype(&key)
                                && !self.has_impl("Neg", &key)
                        }) {
                            self.error(
                                codes::TYPE_MISMATCH,
                                *span,
                                format!("no unary `-` for `{owner}`: it needs an `impl Neg<Out> for {owner}`"),
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
                // An operand has no expected type to make `{ .. }` a struct:
                // it is a concatenation, and its parts must be bits (#218).
                let vector = Ty::Array {
                    elem: Box::new(Ty::Error),
                    family: None,
                    len: 0,
                    low: 0,
                };
                if self.report_bitless_concat(&vector, lhs, sym)
                    | self.report_bitless_concat(&vector, rhs, sym)
                {
                    return;
                }
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
                // A tuple compares position by position and has no other
                // operator.
                let (left, right) = (self.type_of(lhs, sym), self.type_of(rhs, sym));
                if matches!(left, Ty::Tuple(_)) || matches!(right, Ty::Tuple(_)) {
                    if !matches!(op, BinOp::Eq | BinOp::Ne) {
                        self.error_with_help(
                            codes::TYPE_MISMATCH,
                            *span,
                            format!(
                                "a tuple has no `{}` operator",
                                crate::syntax::pretty::bin_op(op)
                            ),
                            "tuples compare with `==` and `!=`, position by position".to_string(),
                        );
                    } else {
                        self.check_tuple_equality(&left, &right, rhs, *span, sym);
                    }
                    return;
                }
                // `u + arr`: an `unsigned` and a `Logic[]` are different
                // types, and an operator's operand never converts implicitly
                // (conversions proposal), so one is converted by hand.
                let literal = |e: &Expr| {
                    matches!(
                        e,
                        Expr::StrLit { .. }
                            | Expr::BitStrLit { .. }
                            | Expr::Array { .. }
                            | Expr::Concat { .. }
                            | Expr::Int { .. }
                            | Expr::CharLit { .. }
                    )
                };
                if !literal(lhs) && !literal(rhs) {
                    let (left, right) = (self.type_of(lhs, sym), self.type_of(rhs, sym));
                    if let (
                        Ty::Array {
                            family: a,
                            elem: ea,
                            ..
                        },
                        Ty::Array {
                            family: b,
                            elem: eb,
                            ..
                        },
                    ) = (&left, &right)
                    {
                        if a.is_some() != b.is_some() && ea == eb {
                            let (family, plain) = if a.is_some() {
                                (&left, rhs)
                            } else {
                                (&right, lhs)
                            };
                            self.error_with_help(
                                codes::TYPE_MISMATCH,
                                expr_span(plain),
                                format!(
                                    "`{}` mixes {} and {}",
                                    crate::syntax::pretty::bin_op(op),
                                    self.ty_display(&left),
                                    self.ty_display(&right)
                                ),
                                format!(
                                    "an operand is not converted implicitly; write `{}({})`",
                                    self.ty_display(family),
                                    crate::syntax::pretty::expr_string(plain)
                                ),
                            );
                            return;
                        }
                    }
                }
                // Mixed numbers: each operand that is not already of the
                // promoted type widens to it (spec 3.17).
                if !matches!(
                    op,
                    BinOp::Custom { .. } | BinOp::And | BinOp::Or | BinOp::Shl | BinOp::Shr
                ) {
                    let (left, right) = (self.type_of(lhs, sym), self.type_of(rhs, sym));
                    if let Some(promoted) = self.numeric_promotion(&left, &right) {
                        for (operand, ty) in [(lhs, left), (rhs, right)] {
                            if ty != promoted {
                                self.promotions.insert(expr_span(operand), promoted.clone());
                            }
                        }
                    }
                }
                // A constant must fit the sized vector it meets under any
                // operator, as under a comparison: `x << 300` on an 8-bit
                // vector reached code generation as a 9-bit `unsigned[8]`,
                // and `x + 300` added 44.
                if !is_comparison(op) {
                    self.check_comparison_fit(lhs, rhs, sym);
                }
                let comparison_handled = if is_comparison(op) {
                    self.check_comparison_fit(lhs, rhs, sym);
                    self.check_comparison_operands(op, lhs, rhs, *span, sym)
                } else {
                    false
                };
                self.check_intrinsic_binary_operands(op, lhs, rhs, *span, sym);
                // A constant zero divisor is always a mistake: hardware has no
                // trap for it, so today it just yields 0 with no complaint. A
                // named constant (`10 / ZERO`) is one too (#233).
                if matches!(op, BinOp::Div | BinOp::Rem) && self.constant_value(rhs) == Some(0) {
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
                    let matching = self.operator_accepts_expr(symbol, &lhs_ty, &rhs_ty, rhs, sym)
                        // ...and on the left, taking the right operand's
                        // type: `255 xor a` as `255 and a` already does.
                        || Self::is_contextual_literal(lhs)
                            && self.assignable(&rhs_ty, lhs, sym)
                            && self.operator_accepts(symbol, &rhs_ty, &rhs_ty, false);
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
                    for (operand, other) in [(lhs, rhs), (rhs, lhs)] {
                        // A char literal reads through the other operand's
                        // enum, as in a comparison (`l and '0'`).
                        if let (Expr::CharLit { ch, .. }, Ty::Named(id)) =
                            (operand.as_ref(), self.type_of(other, sym))
                        {
                            if self.enum_has_char_variant(id, *ch) {
                                continue;
                            }
                        }
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
                                    "`{op_str}` needs bit-derived operands (Bit/Logic/Bool/unsigned/signed); `{}` is {}",
                                    self.ty_display(&t),
                                    if matches!(t, Ty::Char) { "a character" } else { "a number" }
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
                // compare); structs need an `Eq` impl, as ordering needs `Ord`.
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
                                    | BinOp::Rem
                                    | BinOp::Shl
                                    | BinOp::Shr
                            ))
                            && self.is_packed_array_newtype(&name);
                        if !intrinsic_enum_equality && !intrinsic_vector_operator {
                            let operator = if is_comparison(op) {
                                super::operators::comparison_symbol(op)
                            } else {
                                op_str
                            };
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
                ..
            } => {
                // A C `void` function runs for its effect, which only a
                // testbench (or a function it calls) can order; hardware has
                // no place to put the call. A call from hardware to a function
                // is checked once every body is seen, for whether the function
                // reaches such an operation.
                if let Expr::Path(path) = callee.as_ref() {
                    let callee_id = self
                        .resolved
                        .resolved(path.span)
                        .filter(|id| self.resolved.kind_of(*id) == Some(DefKind::Fn));
                    let aggregate =
                        callee_id.is_some_and(|id| self.aggregate_extern_fns.contains(&id));
                    let void =
                        callee_id.is_some_and(|id| self.void_extern_fns.contains(&id)) || aggregate;
                    match (callee_id, self.current_fn.get()) {
                        (Some(id), Some(current)) if self.in_fn_body.get() => {
                            if void {
                                self.testbench_only_fns
                                    .borrow_mut()
                                    .insert(current, crate::syntax::pretty::expr_string(callee));
                            }
                            self.fn_callees
                                .borrow_mut()
                                .entry(current)
                                .or_default()
                                .push(id);
                        }
                        // A signal initializer runs at elaboration, where a
                        // function's `read` is compile-time file I/O.
                        (Some(id), _)
                            if !void
                                && !self.in_testbench.get()
                                && !self.in_fn_body.get()
                                && !self.in_initializer.get() =>
                        {
                            self.hardware_fn_calls.borrow_mut().push((
                                id,
                                expr_span(callee),
                                crate::syntax::pretty::expr_string(callee),
                            ));
                        }
                        _ => {}
                    }
                    if void && !self.in_testbench.get() && !self.in_fn_body.get() {
                        let name = crate::syntax::pretty::expr_string(callee);
                        let (message, help) = if aggregate {
                            (
                                format!("`{name}` passes a struct or array to or from C, so it can only be called in a testbench"),
                                "hardware holds a struct or array as separate signals; pass C scalars instead",
                            )
                        } else {
                            (
                                format!("`{name}` returns nothing, so it can only be called in a testbench"),
                                "give the C function a scalar result to use it in hardware",
                            )
                        };
                        self.error_with_help(
                            codes::INVALID_METHOD_CALL,
                            expr_span(callee),
                            message,
                            help.to_string(),
                        );
                    }
                }
                // The runtime's random numbers and file queries run in a
                // testbench; in hardware they lowered to an intrinsic the
                // scheduler could not evaluate (#252). A signal initializer's
                // `read`/`exists` is compile-time I/O, elaborated.
                if let Expr::Path(path) = callee.as_ref() {
                    if let [segment] = path.segments.as_slice() {
                        let runtime = self
                            .resolved
                            .resolved(path.span)
                            .is_none_or(|id| self.resolved.kind_of(id) == Some(DefKind::Builtin));
                        let name = segment.text.as_str();
                        let random = matches!(name, "rand" | "randint" | "uniform" | "seed");
                        let file = matches!(name, "read" | "exists") && !self.in_initializer.get();
                        if runtime && (random || file) && self.in_fn_body.get() {
                            if let Some(current) = self.current_fn.get() {
                                self.testbench_only_fns
                                    .borrow_mut()
                                    .insert(current, name.to_string());
                            }
                        }
                        if runtime
                            && (random || file)
                            && !self.in_testbench.get()
                            && !self.in_fn_body.get()
                        {
                            self.error_with_help(
                                codes::INVALID_METHOD_CALL,
                                segment.span,
                                format!("`{name}` is only available in a testbench"),
                                if file {
                                    "a hardware signal reads a file only where it is declared: \
                                     `let rom: T = read<T>(\"file\");`"
                                        .to_string()
                                } else {
                                    "hardware has no random source; drive the value from a \
                                     `#[test]` entity"
                                        .to_string()
                                },
                            );
                        }
                    }
                }
                // A method callee is a `Field` node, but its name is a method,
                // not a field — check the receiver and let `check_method_exists`
                // judge the name, so one mistake yields one diagnostic.
                match callee.as_ref() {
                    Expr::Field { base, field, .. } => {
                        self.check_expr(base, sym);
                        // `f.ping()` with `ping` from two traits: lowering
                        // took whichever impl came last (#192). As in rustc,
                        // the call must name its trait.
                        let receiver = self.type_of(base, sym);
                        if let Some(owner) = self.ty_head(&receiver) {
                            if let Some(traits) =
                                self.method_traits.get(&(owner.clone(), field.text.clone()))
                            {
                                if traits.len() > 1 {
                                    let names = traits
                                        .iter()
                                        .map(|t| {
                                            format!(
                                                "`{}::{}(..)`",
                                                t.rsplit("::").next().unwrap_or(t),
                                                field.text
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                        .join(" or ");
                                    self.error_with_help(
                                        codes::UNKNOWN_NAME,
                                        field.span,
                                        format!(
                                            "`{}` is defined by more than one trait for `{owner}`",
                                            field.text
                                        ),
                                        format!("name the trait: {names}"),
                                    );
                                }
                            }
                        }
                    }
                    // A bare path in callee position is checked by
                    // `check_known_call` below. Treating it as an ordinary
                    // value here would double-report unknown functions and
                    // reject runtime/compiler-provided primitives that have
                    // no source declaration.
                    Expr::Path(_) => {}
                    // A sized or ranged type constructor (`unsigned[8](x)`,
                    // `unsigned[2..0](x)`) names a type in `base`, not a value;
                    // `check_known_call` validates the constructor family, so
                    // check only the width/range index expression here.
                    Expr::Index { base, index, .. } if matches!(base.as_ref(), Expr::Path(_)) => {
                        // `integer[2](x)`: a sized constructor is a packed
                        // family's; an array of scalars or structs has none,
                        // and the call did not lower (#223).
                        if let Expr::Path(path) = base.as_ref() {
                            let key = self.path_key(path);
                            let kernel = path.segments.len() == 1
                                && matches!(
                                    path.segments[0].text.as_str(),
                                    "integer" | "natural" | "real" | "Bool" | "Char"
                                );
                            let record = key.as_ref().is_some_and(|key| {
                                self.structs
                                    .get(key)
                                    .is_some_and(|(_, fields)| !fields.is_empty())
                            });
                            if kernel || record {
                                self.error_with_help(
                                    codes::TYPE_MISMATCH,
                                    expr_span(callee),
                                    format!(
                                        "`{}` is an array, not a vector family, and has no constructor",
                                        crate::syntax::pretty::expr_string(callee)
                                    ),
                                    "slice the array (`x[0..1]`) or build one from its elements \
                                     (`[x[0], x[1]]`)"
                                        .to_string(),
                                );
                            }
                        }
                        match index.as_ref() {
                            Expr::Range { lo, hi, .. } => {
                                self.check_index_value(lo, sym, "range bound");
                                self.check_index_value(hi, sym, "range bound");
                            }
                            Expr::PartialRange { lo, hi, .. } => {
                                if let Some(lo) = lo {
                                    self.check_index_value(lo, sym, "range bound");
                                }
                                if let Some(hi) = hi {
                                    self.check_index_value(hi, sym, "range bound");
                                }
                            }
                            _ => {}
                        }
                        self.check_expr(index, sym);
                    }
                    // `(integer, integer)(1, 2)` and `(unsigned[8][2])([..])`
                    // name no conversion: `T(x)` is for a named type. Typed
                    // as an error, they passed silently and did not lower
                    // (#180).
                    Expr::Tuple { .. } => self.error(
                        codes::TYPE_MISMATCH,
                        expr_span(callee),
                        "a tuple type cannot be called: write the tuple itself, `(a, b)`"
                            .to_string(),
                    ),
                    Expr::Index { base, .. } if matches!(base.as_ref(), Expr::Index { .. }) => self
                        .error(
                            codes::TYPE_MISMATCH,
                            expr_span(callee),
                            "an array type cannot be called: write the array itself, `[a, b]`"
                                .to_string(),
                        ),
                    _ => self.check_expr(callee, sym),
                }
                for a in args {
                    self.check_expr(a, sym);
                }
                // A kernel conversion's argument is typed too: `real(x)` and
                // `signed[16](x)` dispatch to `impl From<S> for T` by `x`'s type.
                if matches!(callee.as_ref(), Expr::Index { .. })
                    || matches!(callee.as_ref(), Expr::Path(path)
                    if matches!(path.segments.as_slice(), [only]
                        if matches!(only.text.as_str(), "real" | "integer" | "natural" | "Char")))
                {
                    for a in args {
                        self.type_of(a, sym);
                    }
                }
                if *bang {
                    self.check_format_arity(callee, args, sym);
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
                self.check_conversion_route(callee, args, sym);
                self.check_generic_bounds(callee, args, sym);
                // A `name!(..)` still here is a macro expansion already
                // reported (no matching form, or no such macro): it also
                // read as "unknown function `name`".
                let failed_macro = *bang
                    && matches!(callee.as_ref(), Expr::Path(path)
                        if path.segments.last().is_some_and(|name| !self.callee_is_declared(&name.text)));
                if !failed_macro {
                    self.check_call_arity(callee, args, sym);
                }
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
            Expr::Concat { parts, .. } | Expr::Tuple { elems: parts, .. } => {
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

    /// Report each call from hardware to a function that reaches, through
    /// any chain of calls, an operation only a testbench can run. It used to
    /// fail in lowering, with no word of which call was at fault.
    pub(super) fn check_hardware_fn_calls(&mut self) {
        let calls = std::mem::take(&mut *self.hardware_fn_calls.borrow_mut());
        for (id, span, name) in calls {
            let mut seen = HashSet::new();
            let mut stack = vec![id];
            let mut found = None;
            while let Some(next) = stack.pop() {
                if !seen.insert(next) {
                    continue;
                }
                if let Some(operation) = self.testbench_only_fns.borrow().get(&next) {
                    found = Some(operation.clone());
                    break;
                }
                stack.extend(self.fn_callees.borrow().get(&next).into_iter().flatten());
            }
            if let Some(operation) = found {
                self.error_with_help(
                    codes::INVALID_METHOD_CALL,
                    span,
                    format!("`{name}` reaches `{operation}`, which only a testbench can run"),
                    format!(
                        "call `{name}` from a `#[test]` entity, or keep `{operation}` out of it"
                    ),
                );
            }
        }
    }
}
