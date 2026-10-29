//! Expression type inference, and the type a view is backed by.

use super::*;

impl<'a> Checker<'a> {
    /// Whether a system event/history attribute can observe this type.
    /// Named values are classified from their resolved declaration instead of
    /// being accepted blindly (in particular, entity instances are not data).
    pub(super) fn is_digital_ty(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Void | Ty::Error => false,
            Ty::Named(id) => self
                .resolved
                .kind_of(*id)
                .is_some_and(|kind| matches!(kind, DefKind::Struct | DefKind::Enum)),
            Ty::Array { elem, .. } => self.is_digital_ty(elem),
            Ty::Tuple(elems) => elems.iter().all(|elem| self.is_digital_ty(elem)),
            _ => true,
        }
    }

    // --- type inference core ------------------------------------------------

    /// Best-effort type of an expression given the in-scope value table. Unknown
    /// or unsupported cases yield [`Ty::Error`], which suppresses dependent
    /// checks rather than producing a false positive.
    pub(super) fn type_of(&self, e: &Expr, sym: &HashMap<String, Ty>) -> Ty {
        let key = (
            e as *const Expr as usize,
            sym as *const HashMap<String, Ty> as usize,
        );
        if let Some(cached) = self.type_cache.borrow().get(&key).cloned() {
            return cached;
        }
        let ty = self.infer_type_of(e, sym);
        self.type_cache.borrow_mut().insert(key, ty.clone());
        self.expr_types
            .borrow_mut()
            .insert(expr_span(e), ty.clone());
        ty
    }

    /// Infer an expression's type without reporting; [`Self::check_expr`] reports.
    pub(super) fn infer_type_of(&self, e: &Expr, sym: &HashMap<String, Ty>) -> Ty {
        match e {
            // A numeric literal is `integer`, or `real` when it has a point.
            Expr::Int { text, .. } if text.contains('.') => Ty::Real,
            Expr::Int { .. } => Ty::Integer,
            Expr::IfExpr { then, els, .. } => self.joined_value_type(&[then, els], sym),
            Expr::Match { arms, .. } => {
                let values: Vec<&Expr> = arms.iter().filter_map(MatchArm::value_expr).collect();
                self.joined_value_type(&values, sym)
            }
            // A suffix defined by `impl Suffix for T` types the literal as T;
            // the fixed fs/Hz table backs bare files as integer.
            Expr::SuffixLit { suffix, .. } => {
                if let Some([ty]) = self.suffix_types.get(&suffix.text).map(|v| v.as_slice()) {
                    return self
                        .resolved
                        .defs()
                        .iter()
                        .enumerate()
                        .find(|(index, definition)| {
                            matches!(definition.kind, DefKind::Struct | DefKind::Enum)
                                && self.definition_key_str(DefId(*index as u32))
                                    == Some(ty.as_str())
                        })
                        .map(|(index, _)| Ty::Named(DefId(index as u32)))
                        .unwrap_or(Ty::Error);
                }
                if suffix_scale(&suffix.text).is_some() {
                    Ty::Integer
                } else {
                    Ty::Error
                }
            }
            Expr::BitStrLit { base, digits, .. } => {
                // Only the intrinsic radix prefixes have a known width; an
                // unknown prefix is `Ty::Error` so its diagnostic doesn't
                // cascade into a spurious width mismatch.
                if !crate::syntax::is_radix_prefix(*base) {
                    return Ty::Error;
                }
                let bits = crate::syntax::bits_per_digit(*base);
                Ty::Array {
                    elem: Box::new(self.ty_from_head("Logic")),
                    family: Some(self.array_family_key("unsigned")),
                    len: crate::syntax::radix_digits(digits).count() as u32 * bits,
                    low: 0,
                }
            }
            // A char literal defaults to `Char`; an annotation/target
            // overrides it (Bit/Logic/enum) via `assignable`.
            Expr::CharLit { .. } => Ty::Char,
            // A string literal is `string` = `Char[N]`.
            Expr::StrLit { text, .. } => Ty::Array {
                elem: Box::new(Ty::Char),
                len: text.chars().count() as u32,
                family: None,
                low: 0,
            },
            Expr::Path(p) => {
                if let Some(ty) = self
                    .resolved
                    .resolved(p.span)
                    .filter(|id| self.resolved.kind_of(*id) == Some(DefKind::Const))
                    .and_then(|id| self.const_types.get(&id))
                {
                    return self.ast_ty(ty);
                }
                if p.segments.len() == 1 {
                    sym.get(&p.segments[0].text).cloned().unwrap_or(Ty::Error)
                } else {
                    // `Enum::Variant` has the enum's type, not the variant's.
                    // `Bool`'s variants (`true`/`false`, desugared to
                    // `Bool::true`) are ordinary enum values.
                    match self
                        .resolved
                        .resolved(p.span)
                        .and_then(|id| self.resolved.def(id))
                    {
                        Some(d) if d.kind == DefKind::EnumVariant => {
                            // The enum *named at the use site* decides the
                            // type, not the one that declares the variant.
                            // `enum Mid(Base)` inherits Base's variants, so
                            // `Mid::B` resolves to Base's `B` — typing it as
                            // `Base` would leave a newtype's own variants
                            // impossible to assign to a value of that newtype.
                            let qualifier = p
                                .segments
                                .get(p.segments.len().saturating_sub(2))
                                .and_then(|s| self.resolved.resolved(s.span))
                                .filter(|id| self.resolved.kind_of(*id) == Some(DefKind::Enum));
                            match qualifier.or(d.parent) {
                                Some(pid) => Ty::Named(pid),
                                None => Ty::Error,
                            }
                        }
                        _ => self.named_ty(p.span),
                    }
                }
            }
            Expr::SysAttr { base, attr, .. } => match attr.text.as_str() {
                // `::event` is Bool; the edge helpers are `ClockLike` methods now.
                "event" => self.ty_from_head("Bool"),
                "old" => self.type_of(base, sym),
                "length" | "high" | "low" | "left" | "right" => {
                    // A computed base's type is what `fold_value_lengths`
                    // reads its length from: `(a + b)'length` recorded none.
                    if !matches!(base.as_ref(), Expr::Path(_)) {
                        self.type_of(base, sym);
                    }
                    Ty::Integer
                }
                "ascending" => self.ty_from_head("Bool"),
                name => self
                    .type_attr(base, name, sym)
                    .map_or(Ty::Error, |(_, declaration)| self.ast_ty(&declaration.ty)),
            },
            Expr::Binary { op, lhs, rhs, .. } => {
                if is_comparison(op) {
                    return self.ty_from_head("Bool");
                }
                let lhs_ty = self.type_of(lhs, sym);
                let rhs_ty = self.type_of(rhs, sym);
                // A character literal on the left is the right operand's
                // variant: `'1' and u` is a `Logic`, not a `Char` (#190).
                let lhs_ty = if matches!(lhs.as_ref(), Expr::CharLit { .. })
                    && matches!(rhs_ty, Ty::Named(_))
                    && self.assignable(&rhs_ty, lhs, sym)
                {
                    rhs_ty.clone()
                } else {
                    lhs_ty
                };
                // Mixed numbers promote (spec 3.17).
                if matches!(
                    op,
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem
                ) {
                    if let Some(promoted) = self.numeric_promotion(&lhs_ty, &rhs_ty) {
                        return promoted;
                    }
                }
                let op_str = crate::syntax::pretty::bin_op(op);
                let contextual_rhs =
                    Self::is_contextual_literal(rhs) && self.assignable(&lhs_ty, rhs, sym);
                let numeric_kernel_coercion = matches!(
                    (&lhs_ty, &rhs_ty),
                    (
                        Ty::Array {
                            family: Some(_),
                            ..
                        },
                        Ty::Integer
                    )
                );
                if let Some(Some((output, written))) = self.operator_output(
                    op_str,
                    &lhs_ty,
                    &rhs_ty,
                    contextual_rhs || numeric_kernel_coercion,
                ) {
                    if let Some(ty) = self.declared_operator_output(written.as_ref(), &lhs_ty) {
                        return ty;
                    }
                    if let Some(owner) = self.ty_head(&lhs_ty) {
                        if output == "Self" || output == owner {
                            return lhs_ty;
                        }
                        if self.ty_head(&rhs_ty).as_deref() == Some(output.as_str()) {
                            return rhs_ty;
                        }
                        return self.ty_from_head(&output);
                    }
                }
                // Runtime lowering promotes either ordering of mixed
                // integer/real arithmetic to f64. This applies only to
                // arithmetic: a real shift count (or logical/custom operator)
                // does not turn the whole expression into a real value.
                if matches!(
                    op,
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem
                ) && (matches!(lhs_ty, Ty::Real) || matches!(rhs_ty, Ty::Real))
                {
                    return Ty::Real;
                }
                if matches!(op, BinOp::Custom { .. }) {
                    // A literal on the left takes the right operand's type
                    // (`255 xor a`), as the operator check accepts it.
                    if Self::is_contextual_literal(lhs) && self.assignable(&rhs_ty, lhs, sym) {
                        if let (Some(Some((output, written))), Some(owner)) = (
                            self.operator_output(op_str, &rhs_ty, &rhs_ty, false),
                            self.ty_head(&rhs_ty),
                        ) {
                            if let Some(ty) =
                                self.declared_operator_output(written.as_ref(), &rhs_ty)
                            {
                                return ty;
                            }
                            if output == "Self" || output == owner {
                                return rhs_ty;
                            }
                            return self.ty_from_head(&output);
                        }
                    }
                    // An operator lifted from the element (`for T[]`) gives
                    // the array: `(a xor b) xor c` read `a xor b` as an
                    // error and rejected the outer `xor`.
                    if let BinOp::Custom { symbol, .. } = op {
                        if is_liftable_array_key(symbol)
                            && self
                                .array_operand_element(&lhs_ty)
                                .zip(self.array_operand_element(&rhs_ty))
                                .is_some_and(|(left, right)| {
                                    left == right && self.has_operator_impl(symbol, &left)
                                })
                        {
                            return lhs_ty;
                        }
                    }
                    return Ty::Error;
                }
                // An integer literal joins the other operand's numeric type
                // (`100 / r` with r: signed[8] is an signed[8], via the std
                // `impl Div<signed> for integer`; `1 + n` with n: natural is
                // a natural).
                if matches!(lhs_ty, Ty::Integer)
                    && Self::is_contextual_literal(lhs)
                    && matches!(self.type_of(rhs, sym), Ty::Natural)
                {
                    return Ty::Natural;
                }
                if matches!(lhs_ty, Ty::Integer) {
                    if let r @ Ty::Array {
                        family: Some(_), ..
                    } = self.type_of(rhs, sym)
                    {
                        return r;
                    }
                }
                // A mixed-operand operator impl (`10 + 5i`) yields the
                // impl-owning operand's type.
                if !matches!(lhs_ty, Ty::Named(_)) {
                    if let Ty::Named(id) = self.type_of(rhs, sym) {
                        let has_impl = self.resolved.def(id).map(|d| &d.name).is_some_and(|name| {
                            let tr = crate::syntax::pretty::bin_op(op);
                            self.has_impl(tr, name)
                        });
                        if has_impl {
                            return Ty::Named(id);
                        }
                        // No impl: the operator error says so, and the
                        // recovery type keeps it from also failing as a
                        // mistyped initializer (`let z: Complex = 2 - a`).
                        if matches!(lhs_ty, Ty::Integer | Ty::Real)
                            && self.resolved.kind_of(id) == Some(DefKind::Struct)
                        {
                            return Ty::Error;
                        }
                    }
                }
                lhs_ty
            }
            Expr::Unary {
                op: UnOp::Not, rhs, ..
            } => {
                let rhs_ty = self.type_of(rhs, sym);
                if let Some(owner) = self.ty_head(&rhs_ty) {
                    if let Some((_, Some((output, written)))) = self
                        .operator_sigs
                        .get(&("not".to_string(), owner.clone()))
                        .and_then(|sigs| sigs.first())
                    {
                        if let Some(ty) = self.declared_operator_output(written.as_ref(), &rhs_ty) {
                            return ty;
                        }
                        if output == "Self" || output == &owner {
                            return rhs_ty;
                        }
                        return self.ty_from_head(output);
                    }
                }
                rhs_ty
            }
            Expr::Unary { rhs, .. } => self.type_of(rhs, sym),
            // A name-less struct literal (`ty: None`) takes its type from the
            // assignment target, which `type_of` does not see here.
            Expr::Construct { ty, .. } => ty.as_ref().map(|t| self.ast_ty(t)).unwrap_or(Ty::Error),
            Expr::Tuple { elems, .. } => {
                Ty::Tuple(elems.iter().map(|elem| self.type_of(elem, sym)).collect())
            }
            // A concatenation is an anonymous packed Logic array, as wide as
            // its parts when each one's width is known: unsized, `{ a, b }`
            // of 4 and 8 bits was accepted into an `unsigned[10]` (#183).
            Expr::Concat { parts, .. } => {
                let len = parts
                    .iter()
                    .try_fold(0u32, |total, part| {
                        let ty = self.type_of(part, sym);
                        if !self.concatenable(&ty) {
                            return None;
                        }
                        let width = match ty {
                            Ty::Array { len, .. } if len > 0 => len,
                            Ty::Named(id) => self
                                .definition_key(id)
                                .filter(|key| self.has_impl("LogicEncoding", key))
                                .map(|_| 1)?,
                            _ => return None,
                        };
                        total.checked_add(width)
                    })
                    .unwrap_or(0);
                Ty::Array {
                    elem: Box::new(self.ty_from_head("Logic")),
                    family: Some(self.array_family_key("unsigned")),
                    len,
                    low: 0,
                }
            }
            // An array literal: element type from the first element, length
            // from the count.
            Expr::Array { elems, .. } => {
                let elem = elems
                    .first()
                    .map(|e| self.type_of(e, sym))
                    .unwrap_or(Ty::Error);
                Ty::Array {
                    elem: Box::new(elem),
                    len: elems.len() as u32,
                    family: None,
                    low: 0,
                }
            }
            Expr::Range { .. } | Expr::PartialRange { .. } => self.ty_from_head("Range"),
            Expr::Index { base, index, .. } => {
                let base_ty = self.type_of(base, sym);
                let is_range = matches!(
                    index.as_ref(),
                    Expr::Range { .. } | Expr::PartialRange { .. }
                );
                match &base_ty {
                    Ty::Array {
                        elem,
                        family,
                        len: base_len,
                        low: base_low,
                    } if is_range => {
                        // An open end of an element array's slice (`s[2..]` of
                        // a `Char[5]`) is the array's own: the slice is sized
                        // too, rather than read as a runtime `T[]`.
                        let open = || {
                            let Expr::PartialRange { lo, hi, .. } = index.as_ref() else {
                                return None;
                            };
                            // A packed vector's omitted bound is its declared
                            // `'left` or `'right`: `v[..3]` of an `unsigned[8]`
                            // (`0..7`) is `0..3`, four bits. Unsized, it was
                            // assignable to any width (#232).
                            if family.is_some() {
                                let (left, right) = declared_labels_of(base, &self.array_bounds)?;
                                let left = match lo {
                                    Some(lo) => signed_lit(lo)?,
                                    None => left,
                                };
                                let right = match hi {
                                    Some(hi) => signed_lit(hi)?,
                                    None => right,
                                };
                                return u32::try_from(left.abs_diff(right) + 1).ok();
                            }
                            if *base_len == 0 {
                                return None;
                            }
                            let left = match lo {
                                Some(lo) => signed_lit(lo)?,
                                None => *base_low,
                            };
                            let right = match hi {
                                Some(hi) => signed_lit(hi)?,
                                None => base_low + i64::from(*base_len) - 1,
                            };
                            u32::try_from(left.abs_diff(right) + 1).ok()
                        };
                        // In a testbench an unsized `T[]` is a runtime array,
                        // and so is its slice: sized, `let c: Char[] =
                        // r[1..3]` gave `c` fixed storage for a handle.
                        let runtime = self.in_testbench.get() && family.is_none() && *base_len == 0;
                        let width = if runtime {
                            0
                        } else {
                            explicit_range_len(index).or_else(open).unwrap_or(0)
                        };
                        // A range is a slice at any width: `v[0..0]` is a
                        // one-element vector, as `v(0 downto 0)` is in VHDL and
                        // `&v[0..1]` in Rust; `v[0]` is the element. Decaying
                        // it broke generic code at `W = 1` (#199).
                        // A fixed or float format's slice keeps its binary
                        // point: `x[0..-2]` of a `ufixed<8, 4>` is a
                        // `ufixed<3, 2>`, its lowest label `-2` (#301).
                        let low = if *base_low < 0 && width > 0 {
                            let bounds = match index.as_ref() {
                                Expr::Range { lo, hi, .. } => signed_lit(lo).zip(signed_lit(hi)),
                                Expr::PartialRange { lo, hi, .. } => {
                                    declared_labels_of(base, &self.array_bounds).and_then(
                                        |(left, right)| {
                                            let left = match lo {
                                                Some(lo) => signed_lit(lo)?,
                                                None => left,
                                            };
                                            let right = match hi {
                                                Some(hi) => signed_lit(hi)?,
                                                None => right,
                                            };
                                            Some((left, right))
                                        },
                                    )
                                }
                                _ => None,
                            };
                            bounds.map_or(0, |(left, right)| left.min(right))
                        } else {
                            0
                        };
                        Ty::Array {
                            elem: elem.clone(),
                            family: family.clone(),
                            len: width,
                            low,
                        }
                    }
                    Ty::Array { elem, len, .. } if !is_range => {
                        // An unconstrained array (`len` 0: a runtime string or
                        // `T[]`) checks its bounds when the test runs.
                        if signed_lit(index)
                            .is_some_and(|i| i < 0 || *len != 0 && i as u64 >= *len as u64)
                        {
                            Ty::Error
                        } else {
                            elem.as_ref().clone()
                        }
                    }
                    _ => {
                        let Some(owner) = self.type_kind_name(&base_ty) else {
                            return Ty::Error;
                        };
                        let input = if is_range {
                            self.type_kind_name(&self.ty_from_head("Range"))
                        } else {
                            self.type_kind_name(&self.type_of(index, sym))
                        };
                        let output = self
                            .index_sigs
                            .get(&("Index".to_string(), owner.clone()))
                            .and_then(|sigs| {
                                sigs.iter()
                                    .find(|(i, _)| {
                                        Self::index_contract_type_matches(
                                            i,
                                            input.as_deref(),
                                            &owner,
                                        )
                                    })
                                    .and_then(|(_, output)| output.as_deref())
                            });
                        match output {
                            Some("Self") => base_ty,
                            Some(name) if input.as_deref() == Some(name) => {
                                self.type_of(index, sym)
                            }
                            Some(name) => self.ty_from_head(name),
                            None => Ty::Error,
                        }
                    }
                }
            }
            // Conversion expressions type as their target (spec 3.17):
            // `unsigned[16](x)`, `signed[8](x)`, `integer(x)`, `resize(x, n)`.
            Expr::Call {
                callee,
                args,
                qualifier,
                ..
            } => match callee.as_ref() {
                Expr::Index { base, index, .. } => {
                    let head = match base.as_ref() {
                        Expr::Path(path) => self.path_key(path),
                        _ => None,
                    };
                    // `unsigned[WIDTH](x)` with a named constant is as
                    // sized as `unsigned[16](x)`; unsized, it fit any width
                    // (#240).
                    let w = self.constant_value(index).unwrap_or(0).max(0) as u32;
                    match head.filter(|key| self.array_families.contains(key)) {
                        Some(head) => Ty::Array {
                            elem: Box::new(self.array_element_ty(&head)),
                            family: Some(head),
                            len: w,
                            low: 0,
                        },
                        None => Ty::Error,
                    }
                }
                Expr::Path(p) if p.segments.len() == 1 => match p.segments[0].text.as_str() {
                    // A named struct/enum: a `From` conversion, typed as the
                    // target (fn calls and kernel conversions fall through).
                    name if name != "integer"
                        && name != "resize"
                        && match self.path_ty(p) {
                            Ty::Named(id) => self
                                .resolved
                                .def(id)
                                .is_some_and(|d| matches!(d.kind, DefKind::Struct | DefKind::Enum)),
                            _ => false,
                        } =>
                    {
                        self.path_ty(p)
                    }
                    "integer" => Ty::Integer,
                    "natural" => Ty::Natural,
                    "Char" => self.ty_from_head("Char"),
                    // resize keeps the argument's family at the new width.
                    "resize" => {
                        // A named width too: `resize(v, WIDTH)` (#239).
                        let w = args
                            .get(1)
                            .and_then(|width| self.constant_value(width))
                            .unwrap_or(0)
                            .max(0) as u32;
                        let family = match args.first().map(|a| self.type_of(a, sym)) {
                            Some(Ty::Array { family, .. }) => family,
                            _ => None,
                        };
                        Ty::Array {
                            elem: Box::new(self.ty_from_head("Logic")),
                            family,
                            len: w,
                            low: 0,
                        }
                    }
                    _ => self.free_call_return_type(p, args, sym),
                },
                Expr::Path(path) if path.segments.len() >= 2 => {
                    let owner_segment = &path.segments[path.segments.len() - 2];
                    let owner = self
                        .ident_key(owner_segment)
                        .unwrap_or_else(|| owner_segment.text.clone());
                    let name = &path.segments[path.segments.len() - 1].text;
                    match self.methods.get(&(owner.clone(), name.clone())) {
                        // `ufixed<6, 2>::resize(x)`: the applied type is
                        // `Self`, and so is a result of the bare family.
                        Some(Some(ret)) if qualifier.is_some() => {
                            let applied = qualifier
                                .as_deref()
                                .map_or(Ty::Error, |qualifier| self.ast_ty(qualifier));
                            if matches!(ret, Type::Path(_))
                                && self.type_key(ret).as_deref() == Some(owner.as_str())
                            {
                                applied
                            } else {
                                self.ast_ty_for_owner(ret, &applied)
                            }
                        }
                        Some(Some(ret)) => self.ast_ty_for_owner(ret, &self.ty_from_head(&owner)),
                        Some(None) => Ty::Void,
                        None if matches!(name.as_str(), "default" | "new")
                            && self.is_conversion_name(&owner) =>
                        {
                            self.ty_from_head(&owner)
                        }
                        None => self.free_call_return_type(path, args, sym),
                    }
                }
                // A method call `recv.method(args)` types as the method's
                // declared return type (spec 3.20); the receiver's type head
                // selects the impl. An unknown method or a `self`-only method
                // (no return) is opaque (`Error` suppresses further checks).
                Expr::Field { base, field, .. } => {
                    let recv = self.type_of(base, sym);
                    match self
                        .ty_head(&recv)
                        .and_then(|h| self.methods.get(&(h, field.text.clone())))
                    {
                        Some(Some(ret)) => self.ast_ty_for_owner(ret, &recv),
                        Some(None) => Ty::Void,
                        None => Ty::Error,
                    }
                }
                _ => Ty::Error,
            },
            // A data field access (`p.data`, `self.data`): the field's declared
            // type. This was `Ty::Error`, which suppresses every check that
            // consults it — so the strict assignment-width rule had nothing to
            // compare and `self.data = wide` truncated a 16-bit value into an
            // 8-bit field in silence.
            Expr::Field { base, field, .. } => {
                let recv = self.type_of(base, sym);
                // A tuple position, `t.0`.
                if let Ty::Tuple(elems) = &recv {
                    return field
                        .text
                        .parse::<usize>()
                        .ok()
                        .and_then(|index| elems.get(index).cloned())
                        .unwrap_or(Ty::Error);
                }
                let head = self.ty_head(&recv);
                match head
                    .as_ref()
                    .and_then(|head| self.field_decl_ty(head, &field.text))
                {
                    Some(ty) => self.ast_ty(&ty),
                    // An instance's port (`dut.z`): its declared type. As an
                    // error it read untyped, and `print!("{}", dut.z)` of an
                    // aggregate port found no way to print it.
                    None => head
                        .as_ref()
                        .and_then(|head| self.entities.get(head))
                        .and_then(|ports| ports.iter().find(|port| port.name == field.text))
                        .map(|port| port.ty.clone())
                        .unwrap_or(Ty::Error),
                }
            }
        }
    }

    /// The type-head name used to key impl methods: a named type's def name,
    /// a kernel type's spelling, or the nominal family of an indexed array.
    /// The struct behind a view, given the view's bare name. `views` is keyed
    /// by the `(view, backing)` pair, so a bare name resolves only when one
    /// view carries it; an ambiguous name is left alone rather than guessed.
    pub(super) fn view_backing(&self, name: &str) -> Option<String> {
        let prefix = format!("{name}@");
        let mut targets = self.views.keys().filter_map(|k| k.strip_prefix(&prefix));
        let first = targets.next()?;
        targets.next().is_none().then(|| first.to_string())
    }
}
