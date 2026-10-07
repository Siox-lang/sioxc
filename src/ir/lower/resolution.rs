//! Constructor binding and post-lowering driver/vector resolution.

use super::*;

impl<'a> Lowering<'a> {
    /// Concrete parameter bindings written on an instance type
    /// (`Counter<W = 8>`, or positionally as `Counter<8>`).
    ///
    /// Positional args bind the declaration's parameters in order, matching
    /// `construct_type_params` — each takes the ones it owns, value params
    /// here and bare type params there. Dropping the positional form left the
    /// parameter unbound, which surfaced far downstream as "signal has unknown
    /// width (0)" rather than anything pointing at the instance.
    pub(super) fn construct_params(
        &self,
        ty: &ast::Type,
        entity_id: DefId,
        env: &HashMap<String, i64>,
    ) -> HashMap<String, i64> {
        let mut out = HashMap::new();
        let ast::Type::Generic { args, .. } = ty else {
            return out;
        };
        let decl = self.entities.get(&entity_id);
        for (i, a) in args.iter().enumerate() {
            match a {
                ast::GenericArg::Named { name, value } => {
                    if let Some(v) = self.eval_const(value, env) {
                        out.insert(name.text.clone(), v);
                    }
                }
                ast::GenericArg::Positional(e) => {
                    let Some(p) = decl.and_then(|d| d.params.params.get(i)) else {
                        continue;
                    };
                    // A bare type param is `construct_type_params`' business.
                    if p.bound.is_none() {
                        continue;
                    }
                    if let Some(v) = self.eval_const(e, env) {
                        out.insert(p.name.text.clone(), v);
                    }
                }
                ast::GenericArg::PositionalType(_) | ast::GenericArg::NamedType { .. } => {}
            }
        }
        out
    }

    /// Type-parameter bindings for a generic entity instance (`Buf<unsigned[8]>` ->
    /// `T -> unsigned[8]`): the entity's bare type params (bound `None`), matched to
    /// the construct's generic args positionally or by name.
    pub(super) fn construct_type_params(
        &self,
        ty: &ast::Type,
        entity_id: DefId,
    ) -> HashMap<String, ast::Type> {
        let mut out = HashMap::new();
        let (Some(decl), ast::Type::Generic { args, .. }) = (self.entities.get(&entity_id), ty)
        else {
            return out;
        };
        let type_params: Vec<&ast::Param> = decl
            .params
            .params
            .iter()
            .filter(|p| p.bound.is_none())
            .collect();
        for (i, a) in args.iter().enumerate() {
            match a {
                ast::GenericArg::Named { name, value } => {
                    if type_params.iter().any(|p| p.name.text == name.text) {
                        if let Some(t) = expr_to_type(value) {
                            out.insert(name.text.clone(), t);
                        }
                    }
                }
                ast::GenericArg::NamedType { name, ty } => {
                    if type_params.iter().any(|p| p.name.text == name.text) {
                        out.insert(name.text.clone(), ty.clone());
                    }
                }
                ast::GenericArg::Positional(e) => {
                    if let (Some(p), Some(t)) = (decl.params.params.get(i), expr_to_type(e)) {
                        if p.bound.is_none() {
                            out.insert(p.name.text.clone(), t);
                        }
                    }
                }
                ast::GenericArg::PositionalType(ty) => {
                    if let Some(p) = decl.params.params.get(i) {
                        if p.bound.is_none() {
                            out.insert(p.name.text.clone(), ty.clone());
                        }
                    }
                }
            }
        }
        out
    }

    /// Spec 3.14 + Resolve: a signal driven from several contexts folds each
    /// context's contribution (its override chain over a 'Z' base) through
    /// the type's `Resolve` impl; a type without one is unresolved, and
    /// parallel drivers are an elaboration error.
    pub(super) fn resolve_driver_contexts(&mut self) {
        use std::collections::BTreeMap;
        // target -> ctx -> ordered driver indices
        let mut by_target: BTreeMap<u32, BTreeMap<u32, Vec<usize>>> = BTreeMap::new();
        for (i, d) in self.hardware.drivers.iter().enumerate() {
            by_target
                .entry(d.target.0)
                .or_default()
                .entry(d.ctx)
                .or_default()
                .push(i);
        }
        let mut replaced = Vec::new();
        for (t, ctxs) in &by_target {
            // Metavalue companions are an implementation plane of their parent
            // signal. The parent's element-wise Resolve replaces their drivers
            // together; diagnosing the temporary per-context companion drivers
            // as an independent unresolved net would be a false conflict.
            if self.out.meta_of.values().any(|companion| companion == t) {
                continue;
            }
            if ctxs.len() < 2 {
                continue;
            }
            let ty = self.sig_type.get(t).cloned().unwrap_or_default();
            let direct_resolve = self.op_impls.contains("Resolve", &ty);
            let element_resolve = self
                .out
                .array_element_enums
                .get(t)
                .filter(|element| {
                    self.blanket_array_impls
                        .get("Resolve")
                        .is_some_and(|requirement| self.op_impls.contains(requirement, element))
                })
                .cloned();
            let has_resolve = direct_resolve || element_resolve.is_some();
            let path = self.out.signals[*t as usize].path.clone();
            let declaration_span = self.out.signals[*t as usize].declaration_span;
            let mut labels: Vec<String> = ctxs
                .keys()
                .filter_map(|context| self.out.process_labels.get(context).cloned())
                .collect();
            labels.sort();
            labels.dedup();
            if !has_resolve {
                // Lead with the mistake (several sources driving one signal),
                // not its symptom (a missing `Resolve` impl) — the usual cause
                // is a miswired bus, e.g. two producers on one net. Point at
                // each contributing connection when we know where it came from.
                // Each source is named by its process or assignment label
                // when it has one: `conflicting source 2 (`sum`)`.
                let sites: Vec<(crate::diag::Span, String)> = ctxs
                    .keys()
                    .filter_map(|c| {
                        let span = self.ctx_span.get(c).copied()?;
                        let name = self
                            .out
                            .process_labels
                            .get(c)
                            .and_then(|label| label.rsplit("::").next())
                            .map(|label| format!(" (`{label}`)"))
                            .unwrap_or_default();
                        Some((span, name))
                    })
                    .collect();
                let mut d = crate::diag::Diagnostic::error(format!(
                    "`{path}` is driven by {} conflicting sources",
                    ctxs.len()
                ))
                .with_code(crate::diag::codes::CONFLICTING_DRIVERS);
                if let Some(((first, first_name), rest)) = sites.split_first() {
                    d = d.at(*first);
                    if !first_name.is_empty() {
                        d = d.label(*first, format!("conflicting source 1{first_name}"));
                    }
                    for (i, (s, name)) in rest.iter().enumerate() {
                        d = d.label(*s, format!("conflicting source {}{name}", i + 2));
                    }
                    d = d.label(declaration_span, "signal declared here");
                } else {
                    d = d.at(declaration_span);
                }
                self.sink.emit(d.help(format!(
                    "only one source may drive `{path}`; a bus needs converse \
                     endpoints (one side driving each leaf). To have several \
                     drivers fold instead, `{ty}` needs an `impl Resolve` (as \
                     `Logic` has)"
                )));
                continue;
            }
            // Each resolved target is an independent implicit source context.
            // A shared synthetic context zero would merge unrelated targets
            // (even from different test roots) into one canonical CFG.
            self.cur_ctx += 1;
            let resolved_ctx = self.cur_ctx;
            if let Some(owner) = ctxs
                .keys()
                .find_map(|ctx| self.hardware.context_paths.get(ctx))
                .cloned()
            {
                self.hardware.context_paths.insert(resolved_ctx, owner);
            }
            // A forwarded array Resolve operates per element and preserves the
            // separate value/discriminant planes.
            if let Some(element) = element_resolve {
                let width = self.out.signals[*t as usize].width;
                // Folding unrolls per element, so an operand it repeats is
                // hoisted rather than deep-copied `width` times. Nothing
                // between the arm and the flush creates a signal.
                self.arm_meta_temps(resolved_ctx, declaration_span, None);
                let folded = self.resolve_vector_contexts(ctxs, width, &element);
                self.flush_meta_temps();
                if let Some((value, meta)) = folded {
                    replaced.push((*t, value, Some(meta), labels, resolved_ctx));
                } else {
                    self.sink.emit(
                        crate::diag::Diagnostic::error(format!(
                            "could not instantiate element-wise `impl Resolve for {element}[]` folding `{path}`"
                        ))
                        .with_code(crate::diag::codes::UNSUPPORTED_EXPR)
                        .at(declaration_span)
                        .help(
                            "the element Resolve implementation must have a finite hardware form",
                        ),
                    );
                }
                continue;
            }

            // Each context: fold its drivers (later overrides) over a 'Z' base.
            let neutral = self
                .logic_encoding(&ty)
                .and_then(LogicEncoding::high_impedance_value)
                .or_else(|| self.new_defaults.get(&ty).copied())
                .unwrap_or(0);
            let mut contributions = Vec::new();
            for idxs in ctxs.values() {
                let mut acc = Expr::Const(neutral);
                for &i in idxs {
                    let d = &self.hardware.drivers[i];
                    acc = match &d.cond {
                        None => d.expr.clone(),
                        Some(c) => {
                            self.source_select(c, &d.expr, &acc, d.span.unwrap_or(declaration_span))
                        }
                    };
                }
                contributions.push(acc);
            }
            // Pairwise resolve through a compact source-derived table for a
            // logic enum, or through the ordinary inlined impl for any other
            // user type.
            let mut it = contributions.into_iter();
            let mut folded = it.next().unwrap();
            for c in it {
                let resolved = self
                    .logic_encoding(&ty)
                    .and_then(|encoding| encoding.binary_ops.get("resolve"))
                    .map(|table| {
                        self.source_values.borrow_mut().binary_table(
                            &folded,
                            &c,
                            table,
                            declaration_span,
                        )
                    })
                    .or_else(|| self.inline_resolve(&ty, folded.clone(), c));
                match resolved {
                    Some(r) => folded = r,
                    None => {
                        self.sink.emit(
                            crate::diag::Diagnostic::error(format!(
                                "could not inline `impl Resolve for {ty}` folding `{path}`"
                            ))
                            .with_code(crate::diag::codes::UNSUPPORTED_EXPR)
                            .at(declaration_span)
                            .help("the Resolve implementation must have a finite hardware form"),
                        );
                        break;
                    }
                }
            }
            replaced.push((*t, folded, None, labels, resolved_ctx));
        }
        for (t, expr, meta, labels, ctx) in replaced {
            self.hardware.drivers.retain(|d| d.target.0 != t);
            if !labels.is_empty() {
                self.out.resolved_process_labels.insert(t, labels);
            }
            self.hardware.drivers.push(Driver {
                span: self.cur_span,
                target: SignalId(t),
                cond: None,
                expr,
                meta,
                ctx,
            });
        }
    }

    /// Fold the drivers contributing to one signal through its type's `Resolve`
    /// impl, per element, rather than as one whole-vector expression.
    pub(super) fn resolve_vector_contexts(
        &self,
        contexts: &std::collections::BTreeMap<u32, Vec<usize>>,
        width: u32,
        element: &str,
    ) -> Option<(Expr, Expr)> {
        let encoding = self.logic_encoding(element)?;
        let z = encoding.high_impedance_value()?;
        let span = self.meta_temps.borrow().anchor;
        let mut contributions = Vec::new();
        for indices in contexts.values() {
            let mut value = Expr::Const(encoding.value_bit(z)?);
            let mut meta = Expr::Const(if encoding.binary.contains(&z) { 0 } else { z });
            value = self
                .source_values
                .borrow_mut()
                .repeat_plane(&value, width, 1, span);
            meta = self
                .source_values
                .borrow_mut()
                .repeat_plane(&meta, width, 4, span);
            for &index in indices {
                let driver = &self.hardware.drivers[index];
                let next_value = driver.expr.clone();
                let next_meta = driver
                    .meta
                    .clone()
                    .or_else(|| {
                        let mut temps = self.meta_temps.borrow_mut();
                        let outer = temps.cond.clone();
                        let outer_guard = temps.guard;
                        self.set_meta_guard(&mut temps, driver.cond.clone());
                        let metadata = self.lower_meta_ir(&driver.expr, width, &mut temps);
                        temps.cond = outer;
                        temps.guard = outer_guard;
                        metadata
                    })
                    .unwrap_or(Expr::Const(0));
                match &driver.cond {
                    None => {
                        value = next_value;
                        meta = next_meta;
                    }
                    Some(condition) => {
                        let span = driver.span.unwrap_or(span);
                        value = self.source_select(condition, &next_value, &value, span);
                        meta = self.source_select(condition, &next_meta, &meta, span);
                    }
                }
            }
            contributions.push((value, meta));
        }

        // The per-element loop below reads every contribution's value and
        // discriminant plane once per element, so an inline contribution is
        // deep-copied `width` times -- which is what made a resolved
        // multi-driver signal grow as `width^2` even after the operand metas
        // were hoisted. Bind each plane once and let the unroll read a leaf.
        let contributions: Vec<(Expr, Expr)> = {
            let mut temps = self.meta_temps.borrow_mut();
            contributions
                .into_iter()
                .map(|(value, meta)| {
                    (
                        materialize(value, width, &mut temps),
                        materialize(meta, width * 4, &mut temps),
                    )
                })
                .collect()
        };

        let table = encoding.binary_ops.get("resolve");
        let mut value = Expr::Const(0);
        let mut meta = Expr::Const(0);
        // Resolve one element all the way across the contexts before packing
        // it back into the two vector planes. Repacking after every pair and
        // slicing that expression apart for the next context duplicated the
        // whole accumulated vector once per element and grew exponentially.
        for index in 0..width {
            let mut incoming = contributions.iter();
            let (first_value, first_meta) = incoming.next()?;
            let mut result = self.source_values.borrow_mut().element_disc(
                first_value,
                first_meta,
                index,
                encoding,
                span,
            );
            for (incoming_value, incoming_meta) in incoming {
                let right = self.source_values.borrow_mut().element_disc(
                    incoming_value,
                    incoming_meta,
                    index,
                    encoding,
                    span,
                );
                result = match table {
                    Some(table) => self
                        .source_values
                        .borrow_mut()
                        .binary_table(&result, &right, table, span),
                    None => self.inline_resolve(element, result, right)?,
                };
            }
            let arena = &mut *self.source_values.borrow_mut();
            let value_bit = arena.value_bit(&result, encoding, span);
            let shifted = arena.binary(BinOp::Shl, &value_bit, &Expr::Const(index as u64), span);
            value = arena.binary(BinOp::Or, &value, &shifted, span);
            let binary = arena.disc_in(&result, &encoding.binary, span);
            let is_meta = arena.binary(BinOp::Eq, &binary, &Expr::Const(0), span);
            let nibble = arena.select(&is_meta, &result, &Expr::Const(0), span);
            let shifted = arena.binary(BinOp::Shl, &nibble, &Expr::Const((4 * index) as u64), span);
            meta = arena.binary(BinOp::Or, &meta, &shifted, span);
        }
        Some((value, meta))
    }

    /// Inline `impl Resolve for <ty>` over two already-lowered expressions.
    pub(super) fn inline_resolve(&self, ty: &str, a: Expr, b: Expr) -> Option<Expr> {
        let fns = self.op_impls.get("Resolve", ty)?;
        let (f, _) = fns.first()?;
        let body = f.body.as_ref()?;
        let mut env: HashMap<String, Val> = HashMap::new();
        env.insert("self".to_string(), Val::Scalar(a));
        if let Some(p) = f.params.iter().find(|p| !p.is_self) {
            if let Some(n) = &p.name {
                env.insert(n.text.clone(), Val::Scalar(b));
            }
        }
        match self.inline_block(&body.stmts, &env)? {
            Val::Scalar(e) => Some(e),
            _ => None,
        }
    }
}
