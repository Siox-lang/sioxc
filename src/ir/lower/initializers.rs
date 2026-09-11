//! File-backed, aggregate, and default initializer lowering.

use super::*;

impl<'a> Lowering<'a> {
    /// A `read<T>("path")` initializer's requested type and literal path.
    pub(super) fn fs_read_call(e: &ast::Expr) -> Option<(&ast::Type, &str)> {
        let ast::Expr::Call {
            callee,
            type_args,
            args,
            ..
        } = e
        else {
            return None;
        };
        let ast::Expr::Path(p) = callee.as_ref() else {
            return None;
        };
        if p.segments.len() != 1 || p.segments[0].text != "read" {
            return None;
        }
        match (type_args.as_slice(), args.as_slice()) {
            ([requested], [ast::Expr::StrLit { text, .. }]) => Some((requested, text)),
            _ => None,
        }
    }

    /// Seed a struct literal's leaves, recursing into a nested literal.
    ///
    /// `{ .p = { .x = 7 } }` names no leaf at `p` — the leaves are `p.x` and
    /// `p.y` — so the field loop used to `continue` past it and the inner
    /// values were dropped without a word, while a sibling scalar field on the
    /// same literal seeded correctly.
    /// Whether `value` is a call at all — `Pair::new()` or `Pair()`, once it is
    /// known to name no declared function, is a default construction rather
    /// than an initializer that failed to fold.
    pub(super) fn is_default_construction(value: &ast::Expr) -> bool {
        matches!(value, ast::Expr::Call { .. })
    }

    /// Whether `value` is a call to a function this compilation declares —
    /// which separates "a body too complex to fold" from a default
    /// construction like `Pair::new()`, whose name resolves to no function at
    /// all and whose structural zeros are the right answer.
    pub(super) fn resolves_to_declared_fn(&self, value: &ast::Expr) -> bool {
        let ast::Expr::Call { callee, .. } = value else {
            return false;
        };
        self.free_fns.get(callee).is_some()
    }

    /// The expression a call returns, with the arguments written at the call
    /// substituted for the parameters.
    ///
    /// `None` unless the callee is a declared function whose body is a single
    /// returned expression — anything else has no one expression to stand for
    /// the call, and the caller keeps its own handling.
    pub(super) fn returned_expr_from_call(&self, value: &ast::Expr) -> Option<ast::Expr> {
        let ast::Expr::Call { callee, args, .. } = value else {
            return None;
        };
        let f = self.free_fns.get(callee)?;
        let [ast::Stmt::Return {
            value: Some(returned),
            ..
        }] = f.body.as_ref()?.stmts.as_slice()
        else {
            return None;
        };
        let mut bound: HashMap<String, ast::Expr> = HashMap::new();
        for (param, arg) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(name) = param.name.as_ref() {
                bound.insert(name.text.clone(), arg.clone());
            }
        }
        Some(subst_expr_paths(returned, &bound))
    }

    /// The struct literal a call returns, with the arguments written at the
    /// call substituted for the parameters — so a struct-typed `let` can seed
    /// its fields from `let p: Pair = make(6)` the way it already does from
    /// `let p: Pair = { .a = 6, .b = 7 }`.
    ///
    /// An initializer is a power-on value folded at elaboration, and the scalar
    /// path folds a call: `let a: unsigned[8] = double(6)` is 12. The struct
    /// path folded nothing, because the block that does the folding is reached
    /// only through `locals[name]` and a struct local has no signal under its
    /// bare name — only `p.a`, `p.b`. So every field powered on at zero with no
    /// diagnostic, while the same call written as a separate assignment was
    /// right.
    ///
    /// `None` when the callee is not a known function (`Pair::new()` and
    /// `Pair()` are the structural default, not a call to fold) or its body is
    /// anything but a single returned literal — the caller reports those.
    pub(super) fn struct_literal_from_call(
        &self,
        value: &ast::Expr,
    ) -> Option<(Vec<ast::ConnectArg>, Option<ast::Expr>)> {
        let ast::Expr::Call { callee, args, .. } = value else {
            return None;
        };
        let f = self.free_fns.get(callee)?;
        let body = f.body.as_ref()?;
        let [ast::Stmt::Return {
            value: Some(returned),
            ..
        }] = body.stmts.as_slice()
        else {
            return None;
        };
        let ast::Expr::Construct {
            args: fields,
            spread,
            ..
        } = returned
        else {
            return None;
        };
        // Bind each declared parameter to its argument. `self` takes no
        // argument, so it is skipped rather than consuming one.
        let mut bound: HashMap<String, ast::Expr> = HashMap::new();
        for (param, arg) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(name) = param.name.as_ref() {
                bound.insert(name.text.clone(), arg.clone());
            }
        }
        let fields = fields
            .iter()
            .map(|field| ast::ConnectArg {
                field: field.field.clone(),
                value: field.value.as_ref().map(|v| subst_expr_paths(v, &bound)),
                span: field.span,
            })
            .collect();
        Some((
            fields,
            spread.as_deref().map(|s| subst_expr_paths(s, &bound)),
        ))
    }

    /// Seed a struct-typed signal's leaves from a literal, so unnamed fields
    /// take their declared defaults.
    pub(super) fn seed_struct_literal(
        &mut self,
        prefix: &str,
        struct_name: Option<&str>,
        args: &[ast::ConnectArg],
        spread: Option<&ast::Expr>,
        span: crate::diag::Span,
    ) {
        // `{ ..base, .x = v }` takes every leaf from `base` first, so the
        // explicit fields below overwrite what they name. Without this the
        // fields the literal did not mention powered on at 0 rather than at
        // the base's value, while the ones it did name seeded correctly.
        if let Some(base) = spread.and_then(expr_path) {
            let from = format!("{base}.");
            let leaves: Vec<(String, SignalId)> = self
                .locals
                .iter()
                .filter(|(name, _)| name.starts_with(&from))
                .map(|(name, id)| (name.clone(), *id))
                .collect();
            for (name, src) in leaves {
                let target = format!("{prefix}.{}", &name[from.len()..]);
                if let Some(&dst) = self.locals.get(&target) {
                    let init = self.out.signals[src.0 as usize].init.clone();
                    self.out.signals[dst.0 as usize].init = init;
                }
            }
        }
        let fields: Vec<(String, ast::Type)> = struct_name
            .and_then(|h| self.structs.get(h))
            .map(|sd| {
                sd.fields
                    .iter()
                    .map(|f| (f.name.text.clone(), f.ty.clone()))
                    .collect()
            })
            .unwrap_or_default();
        for (i, arg) in args.iter().enumerate() {
            // Named (`.a = 1`) or positional (bound by declaration order).
            let field = match &arg.field {
                Some(f) => Some(f.text.clone()),
                None => fields.get(i).map(|(n, _)| n.clone()),
            };
            let (Some(field), Some(value)) = (field, arg.value.as_ref()) else {
                continue;
            };
            let path = format!("{prefix}.{field}");
            // A struct-typed field's value is read against *that field's*
            // type, so a positional literal nested inside a named one
            // (`{ .inner = { 1, 2 }, .tag = 9 }`) is a struct literal too. It
            // stayed a concat and seeded nothing.
            let field_ty = fields.iter().find(|(n, _)| *n == field).map(|(_, t)| t);
            let value = &self.as_struct_literal(field_ty, value);
            // A field whose value is itself an aggregate names no leaf of its
            // own, so each of these used to fall through the lookup below and
            // seed nothing — silently, and without even reaching the
            // non-constant check.
            match value {
                ast::Expr::Construct {
                    args: inner,
                    spread: inner_spread,
                    ..
                } => {
                    let inner_ty = fields
                        .iter()
                        .find(|(n, _)| *n == field)
                        .and_then(|(_, ty)| self.free_fns.type_head_key(ty));
                    self.seed_struct_literal(
                        &path,
                        inner_ty.as_deref(),
                        inner,
                        inner_spread.as_deref(),
                        span,
                    );
                    continue;
                }
                // `{ .arr = [4, 5, 6] }` seeds `arr[0..2]`.
                ast::Expr::Array { elems, .. } => {
                    self.seed_elements(&path, elems.iter().collect::<Vec<_>>(), span);
                    continue;
                }
                // `{ .name = "abc" }` seeds one element per character.
                ast::Expr::StrLit { text, .. } => {
                    let indices = self.local_array.get(&path).cloned().unwrap_or_default();
                    for (c, i) in text.chars().zip(&indices) {
                        if let Some(&id) = self.locals.get(&format!("{path}[{i}]")) {
                            let en = self.out.signals[id.0 as usize].enum_type.clone();
                            let v = en
                                .and_then(|e| self.char_disc(c, &e))
                                .unwrap_or(c as u32 as u64);
                            self.out.signals[id.0 as usize].init = vec![v];
                        }
                    }
                    continue;
                }
                _ => {}
            }
            // Only constants seed an init. A non-constant is *not* lowered as
            // a driver here, whatever the comment used to claim: `{ .y = src +
            // 1 }` left `p.y` at 0 for every value of `src`, and the undriven
            // lint does not reach a struct leaf, so nothing said anything.
            let Some(&id) = self.locals.get(&path) else {
                continue;
            };
            // The field's own enum type resolves a character literal
            // (`.state = 'Z'`) to its variant.
            let en = self.out.signals[id.0 as usize].enum_type.clone();
            let is_char = self.out.signals[id.0 as usize].char;
            if let Some(v) = self.const_init_value(value, en.as_deref(), is_char) {
                self.out.signals[id.0 as usize].init = vec![v];
            } else {
                self.report_non_constant_init(&path, span);
            }
        }
    }

    /// Seed each element of an array-valued field or local from a literal.
    pub(super) fn seed_elements(
        &mut self,
        prefix: &str,
        elems: Vec<&ast::Expr>,
        span: crate::diag::Span,
    ) {
        let indices = self.local_array.get(prefix).cloned().unwrap_or_default();
        for (elem, i) in elems.into_iter().zip(indices) {
            let path = format!("{prefix}[{i}]");
            let Some(&id) = self.locals.get(&path) else {
                // An element that is itself a struct has no scalar leaf of its
                // own — its fields are `ps[0].a` — so the lookup above found
                // nothing and the element was skipped in silence, leaving every
                // field of every element at zero. It is a struct literal in
                // element position, read against the element's type like any
                // other.
                if let Some(struct_name) = self.local_struct.get(&path).cloned() {
                    match elem {
                        ast::Expr::Construct { args, spread, .. } => self.seed_struct_literal(
                            &path,
                            Some(&struct_name),
                            args,
                            spread.as_deref(),
                            span,
                        ),
                        _ => {
                            if let Some(args) = self.positional_struct_args(&struct_name, elem) {
                                self.seed_struct_literal(
                                    &path,
                                    Some(&struct_name),
                                    &args,
                                    None,
                                    span,
                                );
                            }
                        }
                    }
                }
                continue;
            };
            let en = self.out.signals[id.0 as usize].enum_type.clone();
            let is_char = self.out.signals[id.0 as usize].char;
            if let Some(v) = self.const_init_value(elem, en.as_deref(), is_char) {
                self.out.signals[id.0 as usize].init = vec![v];
            } else {
                self.report_non_constant_init(&path, span);
            }
        }
    }

    /// An initializer is a power-on value, folded at elaboration (spec 3.29).
    /// One that reads another signal cannot fold, and every site that seeds an
    /// init — scalar, struct field, array element — used to drop it in
    /// silence, leaving the signal at its type's default. A driver is the
    /// spelling that computes from other signals, and is a different thing:
    /// continuous rather than once.
    /// Seed a struct local's leaves from a whole struct constant
    /// (`let p: Pair = K`). `false` when `value` names no struct constant, so
    /// the caller can fall through to its diagnostic.
    pub(super) fn seed_from_struct_const(&mut self, prefix: &str, value: &ast::Expr) -> bool {
        let Some(fields) = expr_path(value).and_then(|name| self.const_struct_value(&name)) else {
            return false;
        };
        for (field, folded) in fields {
            let Expr::Const(word) = folded else { continue };
            let Some(&id) = self.locals.get(&format!("{prefix}.{field}")) else {
                continue;
            };
            let width = self.out.signals[id.0 as usize].width;
            let masked = if width > 0 && width < 64 {
                word & ((1u64 << width) - 1)
            } else {
                word
            };
            self.out.signals[id.0 as usize].init = vec![masked];
        }
        true
    }

    /// A *positional* struct literal, as the named form's arguments.
    ///
    /// `{ 6, 7 }` carries no field names, so it lexes as a bit concatenation
    /// and every struct-typed use of it saw a concat where `{ .a = 6, .b = 7 }`
    /// gives a construction. Nothing bound the parts to fields, so a `let`
    /// initialized this way seeded nothing and an assignment written this way
    /// was dropped entirely — its leaves then reported as never driven.
    /// Binding part *i* to declared field *i* is what the named form means.
    ///
    /// Which reading applies is decided by the *assigned type*, not by the
    /// shape of the braces: against a struct these braces are a struct
    /// literal, against an array or packed vector they stay a concatenation.
    /// So this is keyed on the target's struct name, and a part count that
    /// does not match the field count binds what it can — the fields left
    /// unbound are then reported by the checks that already look for them.
    pub(super) fn positional_struct_args(
        &self,
        struct_name: &str,
        value: &ast::Expr,
    ) -> Option<Vec<ast::ConnectArg>> {
        let ast::Expr::Concat { parts, span } = value else {
            return None;
        };
        let declared = self.structs.get(struct_name)?;
        Some(
            parts
                .iter()
                .zip(&declared.fields)
                .map(|(part, field)| ast::ConnectArg {
                    field: Some(field.name.clone()),
                    value: Some(part.clone()),
                    span: *span,
                })
                .collect(),
        )
    }

    /// `value` as a struct literal when the type it is being assigned to is a
    /// struct: a positional `{ 6, 7 }` becomes the named form, and anything
    /// else is returned unchanged.
    ///
    /// Every position that knows its destination's type goes through here — a
    /// field of an enclosing literal, an instance's port connection, a
    /// function's parameter and its return — so the one rule ("the assigned
    /// type decides how to read the braces") is applied in one way rather than
    /// re-derived per site.
    pub(super) fn as_struct_literal(&self, ty: Option<&ast::Type>, value: &ast::Expr) -> ast::Expr {
        let Some(args) = ty
            .and_then(|ty| self.free_fns.type_head_key(ty))
            .and_then(|head| self.positional_struct_args(&head, value))
        else {
            return value.clone();
        };
        ast::Expr::Construct {
            ty: None,
            args,
            spread: None,
            span: ast::expr_span(value),
        }
    }

    /// A struct constant's folded fields, keyed off the dotted entries
    /// `fold_const` left in the constant table. `None` when `name` names no
    /// struct constant.
    pub(super) fn const_struct_value(&self, name: &str) -> Option<Vec<(String, Expr)>> {
        let prefix = format!("{name}.");
        let mut fields: Vec<(String, Expr)> = self
            .const_values
            .iter()
            .filter_map(|(key, value)| {
                key.strip_prefix(&prefix)
                    .map(|field| (field.to_string(), value.clone()))
            })
            .collect();
        if fields.is_empty() {
            return None;
        }
        // Leaves are matched by name downstream; a stable order only keeps the
        // emitted IR reproducible.
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        Some(fields)
    }

    /// A struct constant's `(field, value)` pairs. A named argument binds by
    /// name; a positional one binds to the declared field at that position, so
    /// both spellings of a literal fold the same way. `None` when the type is
    /// not a known struct or an argument carries no value.
    pub(super) fn const_struct_fields(
        &self,
        ty: &ast::Type,
        args: &[ast::ConnectArg],
    ) -> Option<Vec<(String, ast::Expr)>> {
        let declared = self.structs.get(&self.free_fns.type_head_key(ty)?)?;
        let mut out = Vec::new();
        for (position, arg) in args.iter().enumerate() {
            let field = match &arg.field {
                Some(name) => name.text.clone(),
                None => declared.fields.get(position)?.name.text.clone(),
            };
            out.push((field, arg.value.clone()?));
        }
        Some(out)
    }

    /// Report a `let` initializer that is not constant (E-P021). An initializer
    /// is a power-on value folded at elaboration, so one reading another signal
    /// cannot be honoured.
    pub(super) fn report_non_constant_init(&mut self, name: &str, span: crate::diag::Span) {
        self.sink.emit(
            crate::diag::Diagnostic::error(format!(
                "the initializer for `{name}` is not a constant"
            ))
            .with_code(crate::diag::codes::NON_CONSTANT_INITIALIZER)
            .at(span)
            .help(format!(
                "an initializer is the signal's power-on value and is folded at \
                 elaboration. To compute it from other signals, drive it instead: \
                 declare `{name}` without a value, then assign it"
            )),
        );
    }

    /// The initial value of a constant `let` initializer, folded at compile
    /// time into a signal's power-on `init`. Two shapes reach here:
    /// **literals** — a real's f64 bits, or a character's position in its enum
    /// (a `'g'` has no intrinsic value; its type gives it one) — and **enum
    /// variants** — `Color::Red`, or `Bool`'s `true`/`false`, resolved to their
    /// discriminant. An integer or const-fn expression folds through
    /// `eval_const_fns`. A string initializer is a `Char` array, not a scalar,
    /// so it is written element-wise elsewhere, not here.
    pub(super) fn const_init_value(
        &self,
        e: &ast::Expr,
        target: Option<&str>,
        is_char: bool,
    ) -> Option<u64> {
        match e {
            // --- literals: a value read as bits ---
            ast::Expr::Int { text, .. } if text.contains('.') => {
                text.replace('_', "").parse::<f64>().ok().map(f64::to_bits)
            }
            // A character reads by its position in the target enum (VHDL
            // `T'pos`), else std's default logic type. No value table here.
            // A `Char` target reads it through the Unicode table (its code
            // point), as `typed_char_literal` does for an operand. Only the
            // enum paths were tried here, and `Char` is a kernel type with no
            // variants, so `let c: Char = 'A';` folded to nothing — and once
            // a non-constant initializer became an error, that turned into
            // "the initializer for `c` is not a constant" on an obviously
            // constant character.
            ast::Expr::CharLit { ch, .. } if is_char => Some(*ch as u32 as u64),
            ast::Expr::CharLit { ch, .. } => target
                .and_then(|en| self.char_disc(*ch, en))
                .or_else(|| self.char_disc(*ch, DEFAULT_LOGIC_TYPE)),
            // --- enum variants: a name resolved to its discriminant ---
            // Includes `Bool`'s `true`/`false` (desugared to `Bool::true` etc.).
            ast::Expr::Path(p) if p.segments.len() >= 2 => self.enum_variant_path(p),
            // A radix bit-string initializer (`let v: unsigned[8] = x"AB"`) —
            // its value bits (metavalue positions carried separately, stage 1b).
            ast::Expr::BitStrLit { base, digits, .. } => {
                Some(self.decode_bit_string(*base, digits).0)
            }
            // A plain string on a logic-vector target reads as a logic array —
            // each character is a `std_ulogic` (no prefix needed). Only
            // reached for a single-signal (packed vector) target; a `Char[]`
            // flattens and never lands here.
            ast::Expr::StrLit { text, .. } => Some(self.decode_bit_string('b', text).0),
            // --- integer / const-fn arithmetic ---
            // A newtype constructor is value-transparent, so `let b: Byte =
            // Byte(200);` seeds 200. `eval_const_fns` has no rule for a call,
            // so the signal kept its default and read 0.
            ast::Expr::Call { callee, args, .. }
                if (match callee.as_ref() {
                    ast::Expr::Path(path) => self.free_fns.struct_path_key(path),
                    _ => None,
                })
                .and_then(|name| self.structs.get(&name).cloned())
                .is_some_and(|s| s.fields.is_empty() && s.base.is_some()) =>
            {
                self.const_init_value(args.first()?, target, is_char)
            }
            _ => eval_const_fns(e, &self.cur_env, &self.free_fns, 0).map(|v| v as u64),
        }
    }

    /// Fold each `impl New for T { fn new() -> T { return <const>; } }` to the
    /// type's uninitialized default value. Runs after `impls`/`enum_variants`
    /// are collected.
    pub(super) fn compute_new_defaults(&self) -> HashMap<String, u64> {
        let mut out = HashMap::new();
        // Trait impls land in `op_impls` keyed by (trait, type); `impl New for T`
        // has a `new()` whose constant body is `T`'s uninitialized default.
        for ((tr, ty), fns) in &self.op_impls {
            if tr != "New" {
                continue;
            }
            for (f, _) in fns {
                if f.name.text != "new" {
                    continue;
                }
                if let Some(body) = &f.body {
                    for st in &body.stmts {
                        if let ast::Stmt::Return { value: Some(e), .. } = st {
                            let is_char = ty == "Char"
                                || struct_derives_kernel(ty, "Char", &self.structs, &self.free_fns);
                            if let Some(v) = self.const_init_value(e, Some(ty), is_char) {
                                out.insert(ty.clone(), v);
                            }
                        }
                    }
                }
            }
        }
        out
    }
}
