//! Literal, operator, coercion, and derived-type expression lowering.

use super::*;

impl<'a> Lowering<'a> {
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
}
