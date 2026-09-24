//! `match` checking: exhaustiveness, pattern domains and forms, and
//! unreachable arms. Also holds the signal reset-value rule.

use super::*;

impl<'a> Checker<'a> {
    /// Warn (spec Stage 10) when a `match` on an enum omits variants and has no
    /// `_` wildcard.
    pub(super) fn check_match_exhaustive(&mut self, m: &MatchStmt, sym: &HashMap<String, Ty>) {
        self.check_arms_exhaustive(&m.scrutinee, &m.arms, m.span, sym);
    }

    /// Shared by the statement and expression forms. A match *expression* was
    /// not checked at all, so a missing variant drew no diagnostic and only
    /// surfaced much later as a design no engine would run.
    /// The inclusive value range a numeric scrutinee can hold, or `None` when
    /// it is unbounded (`integer`) or not numeric at all.
    pub(super) fn numeric_domain(&self, ty: &Ty) -> Option<(i128, i128)> {
        let Ty::Array { len, family, .. } = ty else {
            return None;
        };
        let width = *len;
        // Beyond 127 bits the domain no longer fits the arithmetic here, and a
        // match over it could not be spelled out arm by arm anyway.
        if width == 0 || width > 127 {
            return None;
        }
        match family.as_deref().map(|name| self.key_leaf(name)) {
            Some("signed") => {
                let half = 1i128 << (width - 1);
                Some((-half, half - 1))
            }
            Some("unsigned") => Some((0, (1i128 << width) - 1)),
            _ => None,
        }
    }

    /// Warn when a match on a numeric value leaves part of its domain
    /// uncovered. The enum form of this has always been checked; the numeric
    /// form was not, so a missing case was silent — and lowering then had no
    /// base arm for it.
    pub(super) fn check_numeric_arms_exhaustive(&mut self, ty: &Ty, arms: &[MatchArm], span: Span) {
        let Some((lo, hi)) = self.numeric_domain(ty) else {
            return;
        };
        // Collect covered intervals. A bit pattern carries don't-cares, whose
        // coverage is not an interval, so its presence makes the answer
        // unknown and the check steps aside rather than guessing.
        let mut covered: Vec<(i128, i128)> = Vec::new();
        for arm in arms {
            if !collect_pattern_ranges(&arm.pattern, &mut covered) {
                return;
            }
        }
        covered.sort_unstable();
        // Walk the domain, consuming intervals that touch or overlap the
        // frontier. The first interval starting beyond it opens the gap; if
        // none does, the gap runs to the top of the domain.
        let mut frontier = lo;
        let mut gap_end = hi;
        for (start, end) in covered {
            if start > frontier {
                gap_end = (start - 1).min(hi);
                break;
            }
            frontier = frontier.max(end.saturating_add(1));
            if frontier > hi {
                return;
            }
        }
        if frontier > hi {
            return;
        }
        let missing = if frontier == gap_end {
            format!("`{frontier}`")
        } else {
            format!("`{frontier}..{gap_end}`")
        };
        self.sink.emit(
            Diagnostic::warning(format!("non-exhaustive match: {missing} is not covered"))
                .with_code(codes::NON_EXHAUSTIVE_MATCH)
                .at(span)
                .help("add the missing arms, or a `_` wildcard"),
        );
    }

    /// A signal's initializer is its **reset value** (spec 3.4), so it has to
    /// be a constant. A runtime expression there was silently dropped — the
    /// signal simply kept its default, and `let v: unsigned[8] = if c { 7 }
    /// else { 9 };` read 0 with nothing said. A testbench `let` is sequential
    /// storage, where a computed initial value is meaningful and is evaluated.
    pub(super) fn check_signal_reset_value(&mut self, l: &LetDecl) {
        if self.in_testbench.get() {
            return;
        }
        let Some(value) = &l.value else { return };
        // An instance's connection block, a struct/array literal and a
        // constant expression are all fine; anything that reads a signal is
        // not, because there is no time at which a reset value could sample it.
        if !matches!(value, Expr::IfExpr { .. } | Expr::Match { .. }) {
            return;
        }
        self.error_with_help(
            codes::TYPE_MISMATCH,
            expr_span(value),
            format!("`{}`'s initial value is not constant", l.name.text),
            "a signal's initializer is its reset value, so it must be constant; \
             drive it instead (`let x: T; x = <expr>;`)"
                .to_string(),
        );
    }

    /// Check every arm's pattern against the scrutinee's type.
    pub(super) fn check_pattern_domains(&mut self, ty: &Ty, arms: &[MatchArm]) {
        for arm in arms {
            self.check_pattern_domain(ty, &arm.pattern);
        }
    }

    /// Check one pattern against a type: that it is a form the type admits, and
    /// that any literal lies inside its domain.
    pub(super) fn check_pattern_domain(&mut self, ty: &Ty, pattern: &Pattern) {
        if matches!(ty, Ty::Error) {
            return;
        }
        match pattern {
            Pattern::Wildcard | Pattern::CharLit { .. } => {}
            Pattern::Or { alts, .. } => {
                for alternative in alts {
                    self.check_pattern_domain(ty, alternative);
                }
            }
            Pattern::Path(path) if path.segments.len() == 2 => {
                let qualifier = &path.segments[0];
                let qualifier_key = self
                    .ident_key(qualifier)
                    .unwrap_or_else(|| qualifier.text.clone());
                match self.enum_operand_name(ty) {
                    Some(expected) if qualifier_key == expected => {}
                    Some(expected) => self.error_with_help(
                        codes::TYPE_MISMATCH,
                        path.span,
                        format!(
                            "pattern `{}` belongs to enum `{}`, but the matched value is `{expected}`",
                            path.segments[1].text, qualifier.text
                        ),
                        format!(
                            "use a `{}::…` variant in this match",
                            self.key_leaf(&expected)
                        ),
                    ),
                    None => self.error(
                        codes::TYPE_MISMATCH,
                        path.span,
                        format!(
                            "enum pattern `{}::{}` cannot match a {} value",
                            qualifier.text,
                            path.segments[1].text,
                            self.ty_display(ty)
                        ),
                    ),
                }
            }
            // Bare/deeper paths receive their spelling diagnostic from
            // `check_pattern_form`; avoid adding a dependent type error.
            Pattern::Path(_) => {}
            Pattern::Range { span, .. } => {
                let numeric = matches!(ty, Ty::Integer | Ty::Real)
                    || matches!(
                        ty,
                        Ty::Array {
                            family: Some(_),
                            ..
                        }
                    );
                if !numeric {
                    self.error(
                        codes::TYPE_MISMATCH,
                        *span,
                        format!(
                            "an integer pattern cannot match a {} value",
                            self.ty_display(ty)
                        ),
                    );
                }
            }
            Pattern::BitPattern { span, .. } => {
                if !matches!(
                    ty,
                    Ty::Array {
                        family: Some(_),
                        ..
                    }
                ) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        *span,
                        format!(
                            "a bit pattern needs a packed vector, found {}",
                            self.ty_display(ty)
                        ),
                    );
                }
            }
        }
    }

    /// A character pattern names a variant of a character-valued enum, so it
    /// is only meaningful against one. Expression position has always rejected
    /// the rest — `s == '0'` on a numeric or a `State` is "a character literal
    /// has no numeric identity" — but pattern position was checked by nobody
    /// when char patterns landed, and a character has no intrinsic value, so
    /// the arm compared two unrelated discriminants and *matched*:
    /// `match s { '0' => .. }` on `enum State { Idle, Run }` selected the arm,
    /// because `State::Idle` and `'0'` are both 0.
    pub(super) fn check_char_patterns(&mut self, ty: &Ty, arms: &[MatchArm]) {
        let variants = match ty {
            Ty::Named(id) => self
                .definition_key(*id)
                .and_then(|name| self.enum_variants.get(&name).map(|v| (name, v.clone()))),
            _ => None,
        };
        let mut bad: Vec<(char, Span)> = Vec::new();
        for arm in arms {
            collect_char_patterns(&arm.pattern, &mut bad);
        }
        for (ch, span) in bad {
            match &variants {
                None => self.error(
                    codes::TYPE_MISMATCH,
                    span,
                    "a character literal has no numeric identity; convert it \
                     through an encoding table (std::text)"
                        .to_string(),
                ),
                Some((name, vs)) if !vs.iter().any(|v| v == &format!("'{ch}'")) => self
                    .error_with_help(
                        codes::INVALID_PATTERN,
                        span,
                        format!("`'{ch}'` is not a variant of enum `{name}`"),
                        format!(
                            "`{name}` names its variants without quotes; a character pattern \
                             only matches an enum declared with character literals, like `Logic`"
                        ),
                    ),
                Some(_) => {}
            }
        }
    }

    /// Report a match that does not cover its scrutinee (W-P007), after checking
    /// the individual patterns.
    pub(super) fn check_arms_exhaustive(
        &mut self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        span: Span,
        sym: &HashMap<String, Ty>,
    ) {
        let ty = self.type_of(scrutinee, sym);
        self.check_pattern_domains(&ty, arms);
        self.check_char_patterns(&ty, arms);
        let Ty::Named(id) = ty else {
            // A numeric scrutinee has a domain rather than a variant list.
            // Only enums were ever checked, so `match s { 0 => .. }` on an
            // `unsigned[2]` passed silently while the same hole over an enum
            // was reported.
            self.check_numeric_arms_exhaustive(&ty, arms, span);
            return;
        };
        let Some(enum_name) = self.definition_key(id) else {
            return;
        };
        let Some(variants) = self.enum_variants.get(&enum_name).cloned() else {
            return;
        };

        // Collect the covered variant names, flattening or-patterns; a wildcard
        // (bare or inside an `|`) makes the match exhaustive.
        let mut covered: HashSet<String> = HashSet::new();
        for a in arms {
            let (vars, wild) = pattern_covers(&a.pattern);
            if wild {
                return;
            }
            covered.extend(vars);
        }
        let missing: Vec<String> = variants
            .into_iter()
            .filter(|v| !covered.contains(v.as_str()))
            .collect();
        if !missing.is_empty() {
            let names = missing
                .iter()
                .map(|v| format!("`{v}`"))
                .collect::<Vec<_>>()
                .join(", ");
            self.sink.emit(
                Diagnostic::warning(format!(
                    "non-exhaustive match on `{}`: missing {names}",
                    self.key_leaf(&enum_name)
                ))
                .with_code(codes::NON_EXHAUSTIVE_MATCH)
                .at(span)
                .help("add the missing arms, or a `_` wildcard"),
            );
        }
    }

    /// Reject a pattern shape the lowering cannot honour (spec Stage 10,
    /// "invalid pattern"). Variants are `::`-qualified (`Color::Red`), so a
    /// bare name matches nothing the compiler knows — and `arm_match_cond`
    /// treats every pattern it cannot lower as a wildcard, which would make
    /// such an arm silently swallow the whole match, `_` arms included.
    pub(super) fn check_pattern_form(&mut self, p: &Pattern) {
        match p {
            Pattern::Path(path) if path.segments.len() == 1 => {
                let name = &path.segments[0].text;
                self.sink.emit(
                    Diagnostic::error(format!("`{name}` is not a valid pattern"))
                        .with_code(codes::INVALID_PATTERN)
                        .at(path.segments[0].span)
                        .help(
                            "enum patterns name their type (`Color::Red`); a bare name is not \
                             a binding — use `_` to match anything",
                        ),
                );
            }
            Pattern::Path(path) if path.segments.len() != 2 => {
                self.sink.emit(
                    Diagnostic::error("an enum pattern must be written as `Type::Variant`")
                        .with_code(codes::INVALID_PATTERN)
                        .at(path.span)
                        .help("import the enum type, then use exactly its type and variant names"),
                );
            }
            // A pattern whose text is not a well-formed mask (a digit outside
            // the radix) is just as invisible: IR
            // lowering wildcards it while the runner never matches it, so the
            // engines disagree on top of it silently swallowing the arm.
            Pattern::BitPattern { text, span }
                if crate::syntax::bit_pattern_mask(text).is_none() =>
            {
                self.sink.emit(
                    Diagnostic::error(format!("`{text}` is not a valid bit pattern"))
                        .with_code(codes::INVALID_PATTERN)
                        .at(*span)
                        .help(
                            "a bare string is per-bit with `-` as the don't-care (`\"01--\"`); \
                             an `x`/`o` prefix takes hex/octal digits with `?` masking a group",
                        ),
                );
            }
            Pattern::Or { alts, .. } => {
                for a in alts {
                    self.check_pattern_form(a);
                }
            }
            _ => {}
        }
    }

    /// Warn (spec Stage 10) on arms that can never match: anything after a `_`
    /// wildcard, or a variant already covered by an earlier arm.
    pub(super) fn check_unreachable_arms(&mut self, arms: &[MatchArm]) {
        let mut after_wildcard = false;
        let mut seen: HashSet<String> = HashSet::new();
        // Inclusive integer ranges already matched (a bare literal is lo==hi).
        let mut ranges: Vec<(i64, i64)> = Vec::new();
        for arm in arms {
            let reason = if after_wildcard {
                Some("a previous `_` already matches everything".to_string())
            } else {
                match &arm.pattern {
                    Pattern::Wildcard => {
                        after_wildcard = true;
                        None
                    }
                    Pattern::Path(p) if p.segments.len() >= 2 => {
                        let var = p.segments[1].text.clone();
                        (!seen.insert(var.clone()))
                            .then(|| format!("`{var}` is already matched by an earlier arm"))
                    }
                    Pattern::CharLit { ch, .. } => {
                        let var = format!("'{ch}'");
                        (!seen.insert(var.clone()))
                            .then(|| format!("`{var}` is already matched by an earlier arm"))
                    }
                    // A range (or bare literal) wholly inside one already
                    // matched can never be reached — first match wins.
                    Pattern::Range { lo, hi, .. } => {
                        let (lo, hi) = (*lo.min(hi), *lo.max(hi));
                        let covered = ranges.iter().find(|(a, b)| lo >= *a && hi <= *b).copied();
                        ranges.push((lo, hi));
                        covered.map(|(a, b)| {
                            let this = if lo == hi {
                                format!("`{lo}`")
                            } else {
                                format!("`{lo}..{hi}`")
                            };
                            let prev = if a == b {
                                format!("`{a}`")
                            } else {
                                format!("`{a}..{b}`")
                            };
                            format!("{this} is already covered by the earlier arm {prev}")
                        })
                    }
                    _ => None,
                }
            };
            if let Some(reason) = reason {
                self.sink.emit(
                    Diagnostic::warning(format!("unreachable match arm: {reason}"))
                        .with_code(codes::UNREACHABLE_MATCH_ARM)
                        .at(arm.span),
                );
            }
        }
    }

    /// Whether a type implements `Boolean`, so it may be used as a condition.
    pub(super) fn implements_boolean(&self, name: &str) -> bool {
        self.has_impl("Boolean", name)
    }

    /// The name a type is keyed by in the trait-impl table (`unsigned[8]` and
    /// `unsigned` share `unsigned`). `Void`/`Error`/array types have no name.
    pub(super) fn type_kind_name(&self, t: &Ty) -> Option<String> {
        match t {
            Ty::Integer => Some("integer".to_string()),
            Ty::Real => Some("real".to_string()),
            Ty::Char => Some("Char".to_string()),
            Ty::Named(id) => self.definition_key(*id),
            Ty::Array {
                family: Some(name), ..
            } => Some(name.clone()),
            Ty::Array { .. } | Ty::Void | Ty::Error => None,
        }
    }
}
