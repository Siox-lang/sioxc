//! Statements and blocks: return guarantees, dead assignments, effects,
//! stimulus context, and conditions.

use super::*;

impl<'a> Checker<'a> {
    /// Two *unconditional* assignments to one target in the same block: the
    /// first can never be observed, because within a driver context a later
    /// assignment overrides (spec 3.14). Almost always a typo or leftover.
    /// Conservative — any conditional/looping statement in between resets the
    /// scan, so the common `default then override` shapes never trip it.
    pub(super) fn lint_dead_assignments<'s>(&mut self, stmts: impl Iterator<Item = &'s Stmt>) {
        // Testbench assignments are sequential stimulus, not declarative
        // drivers. The native harness settles after every connected-signal
        // write, so even adjacent `clk = '1'; clk = '0';` assignments can be
        // observed by an edge-triggered process. Applying the hardware
        // source-order override rule here is therefore a false positive.
        if self.in_testbench.get() {
            return;
        }
        let mut seen: HashMap<String, Span> = HashMap::new();
        for s in stmts {
            match s {
                Stmt::Assign { target, span, .. } => {
                    let key = crate::syntax::pretty::expr_string(target);
                    if let Some(prev) = seen.insert(key.clone(), *span) {
                        self.sink.emit(
                            Diagnostic::warning(format!(
                                "`{key}` is assigned again here; the earlier \
                                 assignment has no effect"
                            ))
                            .with_code(codes::DEAD_ASSIGNMENT)
                            .at(*span)
                            .label(prev, "this assignment is overridden")
                            .help(
                                "remove the dead assignment, or make one of them \
                                 conditional if you meant a default",
                            ),
                        );
                    }
                }
                // Anything that may write conditionally ends the run.
                _ => seen.clear(),
            }
        }
    }

    /// Check a free function or trait-default body with its declared value
    /// parameters in scope. Previously these bodies were checked against an
    /// empty symbol table, so every parameter expression became `Ty::Error`
    /// and suppressed the very type diagnostics Stage 4 was meant to provide.
    pub(super) fn check_function_block(
        &mut self,
        function: &FnDecl,
        body: &Block,
        self_ty: Option<&Type>,
        require_complete_return: bool,
    ) {
        let mut names = HashMap::new();
        let mut ranged = HashMap::new();
        let mut index_bounds = HashMap::new();
        for parameter in &function.params {
            if parameter.is_self {
                names.insert(
                    "self".to_string(),
                    self_ty.map(|ty| self.ast_ty(ty)).unwrap_or(Ty::Error),
                );
            } else if let (Some(name), Some(ty)) = (&parameter.name, &parameter.ty) {
                names.insert(name.text.clone(), self.ast_ty(ty));
                if let Some(range) = self.declared_range(ty) {
                    ranged.insert(name.text.clone(), range);
                }
                if let Some(range) = self.declared_index_bounds(ty) {
                    index_bounds.insert(name.text.clone(), range);
                }
            }
        }
        let expected = function.ret.as_ref().map(|ty| self.ast_ty(ty));
        if require_complete_return {
            self.check_function_fallthrough(function, body, &names);
        }
        self.check_block_with(
            body,
            &PortDirs::default(),
            &ranged,
            &names,
            &index_bounds,
            expected.as_ref(),
        );
    }

    /// A value-returning function is an expression once inlined, so every
    /// reachable path must produce a value. Letting one fall off the end made
    /// lowering return `None`; callers then failed later as an opaque unknown
    /// driver instead of receiving a diagnostic at the function declaration.
    pub(super) fn check_function_fallthrough(
        &mut self,
        function: &FnDecl,
        body: &Block,
        names: &HashMap<String, Ty>,
    ) {
        let Some(ret) = &function.ret else { return };
        if self.block_guarantees_return(body, names) {
            return;
        }
        let expected = crate::syntax::pretty::type_str(ret);
        self.error_with_help(
            codes::TYPE_MISMATCH,
            function.name.span,
            format!(
                "function `{}` can reach the end without returning {}",
                function.name.text, expected
            ),
            "return a value on every branch, or remove the declared return type".to_string(),
        );
    }

    /// Whether every path through a block returns, so a function with a declared
    /// result cannot fall off its end.
    pub(super) fn block_guarantees_return(
        &self,
        body: &Block,
        outer: &HashMap<String, Ty>,
    ) -> bool {
        // Locals are block-scoped from the start during resolution. Mirror
        // that here so a `match` on a local can prove its domain exhaustive.
        let mut names = outer.clone();
        for statement in &body.stmts {
            if let Stmt::Let(declaration) = statement {
                names.insert(
                    declaration.name.text.clone(),
                    declaration
                        .ty
                        .as_ref()
                        .map(|ty| self.ast_ty(ty))
                        .unwrap_or(Ty::Error),
                );
            }
        }
        body.stmts
            .iter()
            .any(|statement| self.statement_guarantees_return(statement, &names))
    }

    /// Whether one statement guarantees a return on every path.
    pub(super) fn statement_guarantees_return(
        &self,
        statement: &Stmt,
        names: &HashMap<String, Ty>,
    ) -> bool {
        match statement {
            Stmt::Return { .. } => true,
            Stmt::If(if_) => {
                self.block_guarantees_return(&if_.then, names)
                    && match if_.else_.as_deref() {
                        Some(ElseBranch::Block(block)) => {
                            self.block_guarantees_return(block, names)
                        }
                        Some(ElseBranch::If(inner)) => self.if_guarantees_return(inner, names),
                        None => false,
                    }
            }
            Stmt::Match(match_) => {
                !match_.arms.is_empty()
                    && match_
                        .arms
                        .iter()
                        .all(|arm| self.block_guarantees_return(&arm.body, names))
                    && self.match_is_exhaustive(&match_.scrutinee, &match_.arms, names)
            }
            Stmt::Let(_) | Stmt::Assign { .. } | Stmt::For { .. } | Stmt::Expr(_) => false,
        }
    }

    /// Whether an `if` chain returns on every path, which requires an `else`.
    pub(super) fn if_guarantees_return(&self, if_: &IfStmt, names: &HashMap<String, Ty>) -> bool {
        self.block_guarantees_return(&if_.then, names)
            && match if_.else_.as_deref() {
                Some(ElseBranch::Block(block)) => self.block_guarantees_return(block, names),
                Some(ElseBranch::If(inner)) => self.if_guarantees_return(inner, names),
                None => false,
            }
    }

    /// Whether a match covers its scrutinee's type. A wildcard, including one
    /// inside an or-pattern, is sufficient on its own.
    pub(super) fn match_is_exhaustive(
        &self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        names: &HashMap<String, Ty>,
    ) -> bool {
        // A wildcard (including one inside an or-pattern) is sufficient for
        // every scrutinee type.
        if arms.iter().any(|arm| pattern_covers(&arm.pattern).1) {
            return true;
        }
        match self.type_of(scrutinee, names) {
            Ty::Named(id) => {
                let Some(enum_name) = self.definition_key(id) else {
                    return false;
                };
                let Some(variants) = self.enum_variants.get(&enum_name) else {
                    return false;
                };
                let covered: HashSet<String> = arms
                    .iter()
                    .flat_map(|arm| pattern_covers(&arm.pattern).0)
                    .collect();
                variants.iter().all(|variant| covered.contains(variant))
            }
            ty => self.numeric_match_is_exhaustive(&ty, arms),
        }
    }

    /// Whether numeric arms cover the scrutinee's whole value domain.
    pub(super) fn numeric_match_is_exhaustive(&self, ty: &Ty, arms: &[MatchArm]) -> bool {
        let Some((lo, hi)) = self.numeric_domain(ty) else {
            return false;
        };
        let mut covered = Vec::new();
        for arm in arms {
            if !collect_pattern_ranges(&arm.pattern, &mut covered) {
                return false;
            }
        }
        covered.sort_unstable();
        let mut frontier = lo;
        for (start, end) in covered {
            if start > frontier {
                return false;
            }
            frontier = frontier.max(end.saturating_add(1));
            if frontier > hi {
                return true;
            }
        }
        frontier > hi
    }

    /// A method body whose `self` carries directions — an impl on a view
    /// (`impl Stream StreamSource`) — must respect them. Writing an `in` leaf
    /// is rejected inline (`bus.ready = '1'` is `E-P004`), but method bodies
    /// were checked with no directions at all, so the same write hidden in
    /// `fn bad(self) { self.ready = '1'; }` was accepted *and driven*, which
    /// defeats the point of a view.
    pub(super) fn check_block_with(
        &mut self,
        b: &Block,
        view_dirs: &PortDirs,
        bounds: &HashMap<String, (i64, i64)>,
        names: &HashMap<String, Ty>,
        index_bounds: &HashMap<String, (i64, i64)>,
        expected_return: Option<&Ty>,
    ) {
        // Every caller of this is a function body — a trait method, a free
        // function, or an impl method — so `return` is legal inside it.
        let saved = self.in_fn_body.replace(true);
        let saved_index_bounds = self.array_bounds.replace(index_bounds.clone());
        self.check_stmt_sequence(&b.stmts, view_dirs, names, bounds, expected_return);
        self.array_bounds.replace(saved_index_bounds);
        self.in_fn_body.set(saved);
    }

    /// Check one lexical statement sequence with every block-local declaration
    /// in scope, matching resolution's block semantics. Each nested block gets
    /// a cloned environment; its locals shadow outer names but do not leak out.
    pub(super) fn check_stmt_sequence(
        &mut self,
        stmts: &[Stmt],
        outer_dirs: &PortDirs,
        outer_names: &HashMap<String, Ty>,
        outer_ranges: &HashMap<String, (i64, i64)>,
        expected_return: Option<&Ty>,
    ) {
        let saved_index_bounds = self.array_bounds.borrow().clone();
        let mut dirs = outer_dirs.clone();
        let mut names = outer_names.clone();
        let mut ranges = outer_ranges.clone();
        let mut locals = HashSet::new();

        // Resolution binds every local for the whole block before resolving
        // expressions, so collect their declared types first as well. A second
        // pass fills unconstrained array lengths from initializers once every
        // local name is known.
        for statement in stmts {
            let Stmt::Let(declaration) = statement else {
                continue;
            };
            if !locals.insert(declaration.name.text.clone()) {
                self.error_with_help(
                    codes::DUPLICATE_ITEM,
                    declaration.name.span,
                    format!(
                        "`{}` is declared more than once in this block",
                        declaration.name.text
                    ),
                    "rename one local, or assign to the first declaration instead".to_string(),
                );
            }
            let ty = declaration
                .ty
                .as_ref()
                .map(|ty| self.ast_ty(ty))
                .unwrap_or(Ty::Error);
            names.insert(declaration.name.text.clone(), ty);
            if let Some(range) = declaration
                .ty
                .as_ref()
                .and_then(|ty| self.declared_range(ty))
            {
                ranges.insert(declaration.name.text.clone(), range);
            } else {
                ranges.remove(&declaration.name.text);
            }
            self.array_bounds
                .borrow_mut()
                .remove(&declaration.name.text);
            if let Some(range) = declaration
                .ty
                .as_ref()
                .and_then(|ty| self.declared_index_bounds(ty))
            {
                self.array_bounds
                    .borrow_mut()
                    .insert(declaration.name.text.clone(), range);
            }

            let root = declaration.name.text.as_str();
            let shadowed = |candidate: &String| {
                candidate.split(['.', '[']).next().unwrap_or(candidate) == root
            };
            dirs.illegal.retain(|name| !shadowed(name));
            dirs.plain_in_roots.retain(|name| !shadowed(name));
            dirs.consts.retain(|name| !shadowed(name));
        }
        for statement in stmts {
            let Stmt::Let(declaration) = statement else {
                continue;
            };
            let Some(Ty::Array { len: 0, .. }) = names.get(&declaration.name.text) else {
                continue;
            };
            let inferred = match declaration.value.as_ref() {
                Some(Expr::StrLit { text, .. }) => {
                    u32::try_from(text.chars().count()).unwrap_or(u32::MAX)
                }
                Some(Expr::Array { elems, .. }) => u32::try_from(elems.len()).unwrap_or(u32::MAX),
                Some(value) => match self.type_of(value, &names) {
                    Ty::Array { len, .. } => len,
                    _ => 0,
                },
                None => 0,
            };
            if inferred != 0 {
                if let Some(Ty::Array { len, .. }) = names.get_mut(&declaration.name.text) {
                    *len = inferred;
                }
            }
        }

        self.lint_dead_assignments(stmts.iter());
        for statement in stmts {
            self.check_stmt(statement, &dirs, &names, &ranges, expected_return);
        }
        self.array_bounds.replace(saved_index_bounds);
    }

    /// Type-check one statement.
    pub(super) fn check_stmt(
        &mut self,
        s: &Stmt,
        dirs: &PortDirs,
        sym: &HashMap<String, Ty>,
        ranged: &HashMap<String, (i64, i64)>,
        expected_return: Option<&Ty>,
    ) {
        match s {
            Stmt::Let(l) => {
                self.check_instance_placement(l);
                self.require_let_annotation(l);
                self.check_struct_literal_fields(l, sym);
                if let Some(v) = &l.value {
                    self.check_init(l.ty.as_ref(), v, sym);
                    self.check_expr(v, sym);
                }
            }
            Stmt::Assign { target, value, .. } => {
                self.check_write_target(target, dirs);
                self.check_assign_range(target, value, ranged);
                let custom_index = self.check_index_assign(target, value, sym);
                if !custom_index {
                    self.check_assignment(target, value, sym);
                    self.check_expr(target, sym);
                } else if let Expr::Index { base, index, .. } = target {
                    self.check_expr(base, sym);
                    if let Expr::PartialRange { lo, hi, .. } = index.as_ref() {
                        if let Some(lo) = lo {
                            self.check_expr(lo, sym);
                        }
                        if let Some(hi) = hi {
                            self.check_expr(hi, sym);
                        }
                    } else {
                        self.check_expr(index, sym);
                    }
                }
                self.check_expr(value, sym);
            }
            Stmt::If(i) => self.check_if(i, dirs, sym, ranged, expected_return),
            Stmt::Match(m) => {
                self.check_match_exhaustive(m, sym);
                self.check_unreachable_arms(&m.arms);
                for arm in &m.arms {
                    self.check_pattern_form(&arm.pattern);
                }
                self.check_expr(&m.scrutinee, sym);
                let saved = self.in_match_arm.replace(true);
                for arm in &m.arms {
                    self.check_stmt_sequence(&arm.body.stmts, dirs, sym, ranged, expected_return);
                }
                self.in_match_arm.set(saved);
            }
            Stmt::For {
                var, range, body, ..
            } => {
                self.check_expr(range, sym);
                let loop_ty = match range {
                    Expr::Range { lo, hi, .. } => {
                        self.check_index_value(lo, sym, "range bound");
                        self.check_index_value(hi, sym, "range bound");
                        Ty::Integer
                    }
                    // `check_expr` already reports that a partial range needs
                    // an indexed receiver; do not add a second iterable error.
                    Expr::PartialRange { .. } => Ty::Error,
                    _ => match self.type_of(range, sym) {
                        Ty::Array { elem, .. } => *elem,
                        Ty::Error => Ty::Error,
                        found => {
                            self.error_with_help(
                                codes::TYPE_MISMATCH,
                                expr_span(range),
                                format!(
                                    "a `for` loop needs a range or array, found {}",
                                    self.ty_display(&found)
                                ),
                                "use `left..right`, or iterate an array value".to_string(),
                            );
                            Ty::Error
                        }
                    },
                };
                let mut loop_sym = sym.clone();
                loop_sym.insert(var.text.clone(), loop_ty);
                self.check_stmt_sequence(&body.stmts, dirs, &loop_sym, ranged, expected_return);
            }
            Stmt::Expr(e) => {
                self.check_no_effect(e);
                self.check_stimulus_context(e);
                self.check_expr(e, sym);
            }
            Stmt::Return { value, span } => {
                if let Some(v) = value {
                    self.check_expr(v, sym);
                }
                if !self.in_fn_body.get() {
                    self.error_with_help(
                        codes::INVALID_METHOD_CALL,
                        *span,
                        "`return` outside a function".to_string(),
                        "an entity body describes hardware that is always active, so there \
                         is nothing to return from — lowering used to drop this statement \
                         silently"
                            .to_string(),
                    );
                } else {
                    if let (Some(expected), Some(value)) = (expected_return, value) {
                        if self.check_struct_literal_for_ty(expected, value, sym) {
                            return;
                        }
                    }
                    match (expected_return, value) {
                        (Some(expected), Some(value))
                            if !matches!(expected, Ty::Error)
                                && !self.assignable(expected, value, sym) =>
                        {
                            let actual = self.type_of(value, sym);
                            self.error_with_help(
                                codes::TYPE_MISMATCH,
                                expr_span(value),
                                format!(
                                    "cannot return {} from a function declared to return {}",
                                    self.ty_display(&actual),
                                    self.ty_display(expected)
                                ),
                                format!(
                                    "return a {}, or convert the value explicitly",
                                    self.ty_display(expected)
                                ),
                            );
                        }
                        (Some(expected), None) if !matches!(expected, Ty::Error) => {
                            self.error(
                                codes::TYPE_MISMATCH,
                                *span,
                                format!(
                                    "this function must return a {} value",
                                    self.ty_display(expected)
                                ),
                            );
                        }
                        (None, Some(value)) => {
                            self.error(
                                codes::TYPE_MISMATCH,
                                expr_span(value),
                                "this function has no declared return type".to_string(),
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// A statement expression that is not a call cannot do anything: siox has
    /// no side-effecting operators, so `x;` and `a + 1;` compute a value and
    /// discard it. Lowering's catch-all dropped every non-call shape without a
    /// word, so a misspelled name compiled clean — and `continue;`, a Rust
    /// habit siox does not have, looked accepted while a `for` body ran every
    /// iteration anyway. (`break;` was at least a parse error.)
    pub(super) fn check_no_effect(&mut self, e: &Expr) {
        if matches!(e, Expr::Call { .. }) {
            return;
        }
        let loop_keyword = matches!(e, Expr::Path(p) if p.segments.len() == 1
            && matches!(p.segments[0].text.as_str(), "continue" | "break"));
        let help = if loop_keyword {
            "siox has no loop control — a `for` is unrolled at elaboration, so \
             every iteration exists in the hardware. Guard the body with an \
             `if` instead"
        } else {
            "a statement has an effect only if it assigns (`y = ...`) or calls \
             something (`assert!(...)`, `s.send(v)`)"
        };
        self.error_with_help(
            codes::NO_EFFECT_STATEMENT,
            crate::syntax::ast::expr_span(e),
            "this statement has no effect".to_string(),
            help.to_string(),
        );
    }

    /// `await`, `assert!`, `print!` and `warn!` drive and observe a simulation;
    /// an entity body describes hardware that is always active and has no
    /// stimulus to run. Lowering handles them only in a testbench and its
    /// catch-all dropped them elsewhere, so an assertion written into a design
    /// silently never ran.
    pub(super) fn check_stimulus_context(&mut self, e: &Expr) {
        if self.in_testbench.get() || self.in_fn_body.get() {
            return;
        }
        let Expr::Call { callee, span, .. } = e else {
            return;
        };
        let Expr::Path(p) = callee.as_ref() else {
            return;
        };
        let name = match p.segments.as_slice() {
            [seg] => seg.text.as_str(),
            _ => return,
        };
        if !matches!(name, "await" | "wait" | "assert" | "print" | "warn") {
            return;
        }
        self.error_with_help(
            codes::INVALID_METHOD_CALL,
            *span,
            format!("`{name}` is only available in a testbench"),
            "an entity body describes hardware that is always active — put stimulus and \
             checks in a `#[test]` entity, which drives this one"
                .to_string(),
        );
    }

    /// Type-check an `if` chain and its branches.
    pub(super) fn check_if(
        &mut self,
        i: &IfStmt,
        dirs: &PortDirs,
        sym: &HashMap<String, Ty>,
        ranged: &HashMap<String, (i64, i64)>,
        expected_return: Option<&Ty>,
    ) {
        self.check_condition(&i.cond, sym);
        self.check_expr(&i.cond, sym);
        self.check_stmt_sequence(&i.then.stmts, dirs, sym, ranged, expected_return);
        match i.else_.as_deref() {
            Some(ElseBranch::Block(b)) => {
                self.check_stmt_sequence(&b.stmts, dirs, sym, ranged, expected_return)
            }
            Some(ElseBranch::If(inner)) => self.check_if(inner, dirs, sym, ranged, expected_return),
            None => {}
        }
    }

    /// A condition's type must implement `Boolean` (spec 3.16, generalized).
    /// `Bit`/`Bool` have built-in impls; user types opt in with `impl Boolean
    /// for T`; `Logic` has none, so it still requires an explicit comparison.
    /// An unknown (`Error`) condition type is skipped to avoid false positives.
    pub(super) fn check_condition(&mut self, cond: &Expr, sym: &HashMap<String, Ty>) {
        let ty = self.type_of(cond, sym);
        if matches!(ty, Ty::Void) {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(cond),
                "a procedure call has no value and cannot be used as a condition".to_string(),
            );
            return;
        }
        let Some(name) = self.type_kind_name(&ty) else {
            return;
        };
        if !self.implements_boolean(&name) {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(cond),
                format!(
                    "`{name}` cannot be used directly as a condition; \
                     compare it explicitly (e.g. `== '1'`) or `impl Boolean for {name}`"
                ),
            );
        }
    }
}
