//! Source-defined logic encodings, signal construction, and metavalue planes.

use super::*;

impl<'a> Lowering<'a> {
    /// Elaborate the std-owned `LogicEncoding` behaviour and ordinary logic
    /// operator implementations into data. VHDL's `std_logic_1164` keeps its
    /// value normalization and truth tables in the package; SIOX follows that
    /// dependency direction instead of teaching IR passes the enum order.
    pub(super) fn compute_logic_encodings(&self) -> HashMap<String, LogicEncoding> {
        const CONTRACT: &str = "std::logic::LogicEncoding";
        let mut out = HashMap::new();
        for ((trait_name, ty), methods) in &self.op_impls {
            if trait_name != CONTRACT && trait_name != "LogicEncoding" {
                continue;
            }
            let Some(variants) = self.enum_variants.get(ty) else {
                continue;
            };
            let method = |name: &str| {
                methods
                    .iter()
                    .find_map(|(function, _)| (function.name.text == name).then_some(*function))
            };
            let (Some(to_bool), Some(is_binary), Some(is_high_impedance), Some(to_x01)) = (
                method("to_bool"),
                method("is_binary"),
                method("is_high_impedance"),
                method("to_x01"),
            ) else {
                continue;
            };
            let mut encoding = LogicEncoding::default();
            for &disc in variants.values() {
                let env = HashMap::from([("self".to_string(), disc)]);
                let Some(bit) = eval_logic_function(to_bool, &env, variants) else {
                    continue;
                };
                let Some(binary) = eval_logic_function(is_binary, &env, variants) else {
                    continue;
                };
                let Some(high_impedance) = eval_logic_function(is_high_impedance, &env, variants)
                else {
                    continue;
                };
                let Some(x01) = eval_logic_function(to_x01, &env, variants) else {
                    continue;
                };
                encoding.value_bits.insert(disc, bit != 0);
                if binary != 0 {
                    encoding.binary.insert(disc);
                }
                if high_impedance != 0 {
                    encoding.high_impedance.insert(disc);
                }
                encoding.x01.insert(disc, x01);
            }
            // A partial table is unsafe: packed lowering must be able to
            // classify every member of the enum. Do not publish malformed
            // metadata and let the ordinary unsupported-expression paths
            // reject uses that require it.
            if variants
                .values()
                .any(|disc| !encoding.value_bits.contains_key(disc))
                || encoding
                    .binary
                    .iter()
                    .filter(|disc| encoding.value_bits.get(disc) == Some(&false))
                    .count()
                    != 1
                || encoding
                    .binary
                    .iter()
                    .filter(|disc| encoding.value_bits.get(disc) == Some(&true))
                    .count()
                    != 1
            {
                continue;
            }
            encoding
                .unknown
                .extend(encoding.x01.iter().filter_map(|(&disc, normalized)| {
                    (!encoding.binary.contains(normalized)).then_some(disc)
                }));

            // Logical behaviour remains ordinary `Operator` source. Fold it
            // for every pair now, once, rather than rediscovering a second
            // truth table in each backend.
            for op in ["and", "or", "xor", "nand", "nor", "xnor"] {
                let Some((function, _)) = self
                    .op_impls
                    .get(&(op.to_string(), ty.clone()))
                    .and_then(|functions| functions.first())
                else {
                    continue;
                };
                let Some(rhs_name) = function
                    .params
                    .iter()
                    .find(|parameter| !parameter.is_self)
                    .and_then(|parameter| parameter.name.as_ref())
                    .map(|name| name.text.clone())
                else {
                    continue;
                };
                let mut table = HashMap::new();
                for &left in variants.values() {
                    for &right in variants.values() {
                        let env =
                            HashMap::from([("self".to_string(), left), (rhs_name.clone(), right)]);
                        if let Some(result) = eval_logic_function(function, &env, variants) {
                            table.insert((left, right), result);
                        }
                    }
                }
                encoding.binary_ops.insert(op.to_string(), table);
            }
            // Parallel-driver resolution is source-defined by the ordinary
            // `Resolve` trait too. Keep its complete enum table beside the
            // logical operator tables so repeated drivers lower to a compact
            // lookup instead of repeatedly cloning the implementation's
            // branch tree.
            if let Some((function, _)) = self
                .op_impls
                .get(&("Resolve".to_string(), ty.clone()))
                .and_then(|functions| functions.first())
            {
                if let Some(rhs_name) = function
                    .params
                    .iter()
                    .find(|parameter| !parameter.is_self)
                    .and_then(|parameter| parameter.name.as_ref())
                    .map(|name| name.text.clone())
                {
                    let mut table = HashMap::new();
                    for &left in variants.values() {
                        for &right in variants.values() {
                            let env = HashMap::from([
                                ("self".to_string(), left),
                                (rhs_name.clone(), right),
                            ]);
                            if let Some(result) = eval_logic_function(function, &env, variants) {
                                table.insert((left, right), result);
                            }
                        }
                    }
                    encoding.binary_ops.insert("resolve".to_string(), table);
                }
            }
            if let Some((function, _)) = self
                .op_impls
                .get(&("not".to_string(), ty.clone()))
                .and_then(|functions| functions.first())
            {
                let mut table = HashMap::new();
                for &disc in variants.values() {
                    let env = HashMap::from([("self".to_string(), disc)]);
                    if let Some(result) = eval_logic_function(function, &env, variants) {
                        table.insert(disc, result);
                    }
                }
                encoding.unary_ops.insert("not".to_string(), table);
            }

            // A derived enum has the same representation and variants. Make
            // the contract available under both `Logic` and its ULogic base so
            // literals, scalar signals, and packed family elements agree.
            out.insert(ty.clone(), encoding.clone());
            let mut current = ty.as_str();
            let mut seen = std::collections::HashSet::new();
            while seen.insert(current.to_string()) {
                let Some(base) = self.enum_bases.get(current) else {
                    break;
                };
                out.insert(base.clone(), encoding.clone());
                current = base;
            }
        }
        out
    }

    /// Allocate the next driver context. Writes within one context override;
    /// writes across contexts resolve.
    pub(super) fn next_ctx(&mut self) -> u32 {
        self.cur_ctx += 1;
        self.cur_ctx
    }

    /// A fresh driver context tied to the source site that created it, so a
    /// later conflict can point at the connection rather than just the signal.
    pub(super) fn next_ctx_at(&mut self, span: crate::diag::Span) -> u32 {
        let ctx = self.next_ctx();
        self.ctx_span.insert(ctx, span);
        ctx
    }

    /// Create a signal and return its id, recording its width, domain and
    /// declaration anchor.
    pub(super) fn add_signal(
        &mut self,
        entity: &str,
        name: &str,
        width: u32,
        declaration_span: crate::diag::Span,
    ) {
        let id = SignalId(self.out.signals.len() as u32);
        self.out.signals.push(Signal {
            path: format!("{entity}.{name}"),
            declaration_span,
            width,
            real: false,
            integer: false,
            char: false,
            range: None,
            init: vec![0],
            enum_type: None,
        });
        self.locals.insert(name.to_string(), id);
    }

    /// Ensure a `Logic`-vector signal has its metavalue companion — an extra
    /// signal, 4 bits per element, holding each element's full `std_ulogic`
    /// discriminant (nibble *i* = element *i*'s 9-value), so a read reconstructs
    /// the exact value (`'X'` vs `'Z'` vs …). The companion is a normal signal,
    /// so the engines store/reset it for free. Created only where a metavalue
    /// appears, so metavalue-free designs are untouched. Initial discriminants
    /// are arbitrary-width word vectors, so the companion scales with the
    /// source vector rather than stopping at one ABI word.
    pub(super) fn ensure_meta_companion(&mut self, id: SignalId, discs: Vec<u64>) {
        if let Some(&cid) = self.out.meta_of.get(&id.0) {
            self.out.signals[cid as usize].init = discs;
            return;
        }
        let sig = &self.out.signals[id.0 as usize];
        let companion = Signal {
            path: format!("{}$meta", sig.path),
            declaration_span: sig.declaration_span,
            width: sig.width * 4,
            real: false,
            integer: false,
            char: false,
            range: None,
            init: discs,
            enum_type: None,
        };
        let cid = self.out.signals.len() as u32;
        self.out.signals.push(companion);
        self.out.meta_of.insert(id.0, cid);
    }

    /// The metavalue companion for a *driven* vector: existing one, or a fresh
    /// all-clean (`init = 0`) companion. Never overwrites an init companion.
    pub(super) fn driven_companion(&mut self, id: SignalId) -> u32 {
        if let Some(&c) = self.out.meta_of.get(&id.0) {
            return c;
        }
        let sig = &self.out.signals[id.0 as usize];
        let companion = Signal {
            path: format!("{}$meta", sig.path),
            declaration_span: sig.declaration_span,
            width: sig.width * 4,
            real: false,
            integer: false,
            char: false,
            range: None,
            init: vec![0],
            enum_type: None,
        };
        let cid = self.out.signals.len() as u32;
        self.out.signals.push(companion);
        self.out.meta_of.insert(id.0, cid);
        cid
    }

    /// The metavalue disc-array a driver expression produces, or `None` if it is
    /// provably clean. `width` is the expression's result element count (for
    /// the poison pattern); recursive operands derive their own widths from the
    /// IR rather than inheriting a narrowed destination width. Covers: a signal
    /// read (its companion — copies and port
    /// connections carry the metavalue), `numeric_std` arithmetic (any metavalue
    /// operand poisons the whole result to `'X'`), and a mux (per branch).
    /// Logical/relational is a follow-on.
    pub(super) fn lower_meta_ir(
        &self,
        e: &Expr,
        width: u32,
        temps: &mut MetaTemps,
    ) -> Option<Expr> {
        match e {
            Expr::Current(id) => self
                .out
                .meta_of
                .get(&id.0)
                .map(|&c| Expr::Current(SignalId(c))),
            Expr::Old(id) => self.out.meta_of.get(&id.0).map(|&c| Expr::Old(SignalId(c))),
            Expr::Binary {
                op: BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div,
                lhs,
                rhs,
            } => {
                let lhs_width = self.meta_expr_width(lhs, width);
                let rhs_width = self.meta_expr_width(rhs, width);
                let cond = [
                    (self.lower_meta_ir(lhs, lhs_width, temps), lhs_width),
                    (self.lower_meta_ir(rhs, rhs_width, temps), rhs_width),
                ]
                .into_iter()
                .filter_map(|(meta, operand_width)| {
                    let encoding = self.logic_encoding(DEFAULT_LOGIC_TYPE)?;
                    // `any_unknown` reads the operand once per element, so an
                    // inline operand is deep-copied `operand_width` times.
                    // Bind it once and let the unroll read a leaf.
                    meta.map(|meta| {
                        let meta = materialize(meta, operand_width * 4, temps);
                        any_unknown(&meta, operand_width, encoding)
                    })
                })
                .reduce(|a, b| Expr::Binary {
                    op: BinOp::Or,
                    lhs: Box::new(a),
                    rhs: Box::new(b),
                })?;
                let mut all_x = vec![0u64; (width as usize).div_ceil(16)];
                for i in 0..width {
                    all_x[i as usize / 16] |= self.x_disc() << (4 * (i % 16));
                }
                Some(Expr::Select {
                    cond: Box::new(cond),
                    then: Box::new(words_const(all_x)),
                    els: Box::new(Expr::Const(0)),
                })
            }
            Expr::Select { cond, then, els } => {
                let (mt, me) = (
                    self.lower_meta_ir(then, width, temps),
                    self.lower_meta_ir(els, width, temps),
                );
                if mt.is_none() && me.is_none() {
                    return None;
                }
                Some(Expr::Select {
                    cond: cond.clone(),
                    then: Box::new(mt.unwrap_or(Expr::Const(0))),
                    els: Box::new(me.unwrap_or(Expr::Const(0))),
                })
            }
            Expr::Binary { op, lhs, rhs } if matches!(op, BinOp::And | BinOp::Or | BinOp::Xor) => {
                self.logical_meta(*op, lhs, rhs, width, temps)
            }
            // A slice selects elements, so it selects their discriminants: the
            // companion holds four bits per element, so the nibble range is
            // four times the element range. Without this a slice had no
            // companion and `"0000X100"[3..0]` came back `1100`, the metavalue
            // replaced by the value plane's bit for it.
            Expr::Slice { base, hi, lo } => {
                let base_width = self.meta_expr_width(base, width.max(hi + 1));
                let m = self.lower_meta_ir(base, base_width, temps)?;
                Some(Expr::Slice {
                    base: Box::new(m),
                    hi: hi * 4 + 3,
                    lo: lo * 4,
                })
            }
            // A shift moves elements, so it moves their discriminants with
            // them: the companion holds four bits per element, so it shifts by
            // four times as much. Without this the result had no companion at
            // all and `"0000X100" << 2` came back `00110000` -- the metavalue
            // silently replaced by the value plane's bit for it -- where VHDL
            // gives `00X10000`. Elements shifted in are `'0'`, which is a zero
            // nibble, exactly what shifting in zeroes produces.
            Expr::Binary {
                op: op @ (BinOp::Shl | BinOp::Shr),
                lhs,
                rhs,
            } => {
                let lhs_width = self.meta_expr_width(lhs, width);
                let m = self.lower_meta_ir(lhs, lhs_width, temps)?;
                Some(Expr::Binary {
                    op: *op,
                    lhs: Box::new(m),
                    rhs: Box::new(Expr::Binary {
                        op: BinOp::Mul,
                        lhs: Box::new(rhs.as_ref().clone()),
                        rhs: Box::new(Expr::Const(4)),
                    }),
                })
            }
            // `not` as an IR unary. A vector `not` no longer arrives here --
            // it lowers to `x xor all-ones` and is handled as an `Xor` above --
            // so nothing in the corpus or the nvc differential reaches this,
            // and its `'U'` handling below is consistent by construction rather
            // than by test. It is kept because the shape is still a valid one
            // for the IR to hold, and a handler that disagreed with the `Xor`
            // path would be worse than one that is merely unused.
            Expr::Unary { op: UnOp::Not, rhs } => {
                let rhs_width = self.meta_expr_width(rhs, width);
                let m = self.lower_meta_ir(rhs, rhs_width, temps)?;
                let encoding = self.logic_encoding(DEFAULT_LOGIC_TYPE)?;
                let table = encoding.unary_ops.get("not")?;
                // Both planes are read once per element below; bind each once
                // rather than deep-copying the operand `width` times.
                let m = materialize(m, rhs_width * 4, temps);
                let value = materialize(rhs.as_ref().clone(), rhs_width, temps);
                let mut acc = Expr::Const(0);
                for i in 0..width {
                    let input = logic_element_disc(&value, &m, i, encoding);
                    let result = logic_unary_table_result(input, table);
                    let meta = not1(logic_disc_in(result.clone(), &encoding.binary));
                    acc = or_expr(acc, meta_nibble(meta, i, result));
                }
                Some(acc)
            }
            _ => None,
        }
    }

    /// Best available element width for a lowered value expression. Constants
    /// inherit their contextual fallback; signal reads and slices are exact.
    /// Keeping this structural avoids baking type-family widths into the IR and
    /// is enough to prevent a narrow outer slice from hiding unknown elements
    /// in a wider computed operand.
    pub(super) fn meta_expr_width(&self, expr: &Expr, fallback: u32) -> u32 {
        match expr {
            Expr::Current(id) | Expr::Old(id) => self
                .out
                .signals
                .get(id.0 as usize)
                .map(|signal| signal.width)
                .unwrap_or(fallback),
            Expr::Slice { hi, lo, .. } => hi.saturating_sub(*lo) + 1,
            Expr::Select { then, els, .. } => self
                .meta_expr_width(then, fallback)
                .max(self.meta_expr_width(els, fallback)),
            Expr::Unary { rhs, .. } => self.meta_expr_width(rhs, fallback),
            Expr::Binary {
                op: BinOp::Shl | BinOp::Shr | BinOp::AShr,
                lhs,
                ..
            } => self.meta_expr_width(lhs, fallback),
            Expr::Binary { lhs, rhs, .. } => self
                .meta_expr_width(lhs, fallback)
                .max(self.meta_expr_width(rhs, fallback)),
            _ => fallback,
        }
    }

    /// The metavalue companion of a per-element logical op (`std_logic_1164`),
    /// unrolled per element: a result element is `'X'` when an operand is
    /// a metavalue *and* no operand forces the output — `0 and X = 0`,
    /// `1 or X = 1`, `X xor _ = X`.
    pub(super) fn logical_meta(
        &self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        width: u32,
        temps: &mut MetaTemps,
    ) -> Option<Expr> {
        let lhs_width = self.meta_expr_width(lhs, width);
        let rhs_width = self.meta_expr_width(rhs, width);
        let (ma, mb) = (
            self.lower_meta_ir(lhs, lhs_width, temps),
            self.lower_meta_ir(rhs, rhs_width, temps),
        );
        if ma.is_none() && mb.is_none() {
            return None;
        }
        let encoding = self.logic_encoding(DEFAULT_LOGIC_TYPE)?;
        let symbol = match op {
            BinOp::And => "and",
            BinOp::Or => "or",
            BinOp::Xor => "xor",
            _ => return None,
        };
        let table = encoding.binary_ops.get(symbol)?;
        // The unroll below reads both planes of both operands once per element.
        // Binding all four once turns `4 * width` deep copies of whole operand
        // subtrees into `4 * width` leaf reads -- the difference between
        // `width^depth` and `width * depth` growth for nested expressions.
        let ma = ma.map(|m| materialize(m, lhs_width * 4, temps));
        let mb = mb.map(|m| materialize(m, rhs_width * 4, temps));
        let lhs = materialize(lhs.clone(), lhs_width, temps);
        let rhs = materialize(rhs.clone(), rhs_width, temps);
        let mut acc = Expr::Const(0);
        for i in 0..width {
            let left_meta = ma.clone().unwrap_or(Expr::Const(0));
            let right_meta = mb.clone().unwrap_or(Expr::Const(0));
            let left = logic_element_disc(&lhs, &left_meta, i, encoding);
            let right = logic_element_disc(&rhs, &right_meta, i, encoding);
            let result = logic_binary_table_result(left, right, table);
            let meta = not1(logic_disc_in(result.clone(), &encoding.binary));
            acc = or_expr(acc, meta_nibble(meta, i, result));
        }
        Some(acc)
    }

    /// Propagate metavalues through operators: drive each vector target's
    /// companion from [`Self::lower_meta_ir`] of its value. Runs after drivers are
    /// lowered.
    pub(super) fn propagate_metavalues(&mut self) {
        // First discover the complete set of signals that need companions.
        // A newly discovered companion can make a downstream copy discoverable,
        // so this is a fixed point over the finite set of value signals. No
        // companion drivers are added during discovery: that keeps companion
        // expressions terminal and makes the bound explicit.
        loop {
            let companion_ids: std::collections::HashSet<u32> =
                self.out.meta_of.values().copied().collect();
            let mut discovered = Vec::new();
            for d in &self.out.drivers {
                if companion_ids.contains(&d.target.0) || self.out.meta_of.contains_key(&d.target.0)
                {
                    continue;
                }
                let n = self.out.signals[d.target.0 as usize].width;
                // Discovery only asks whether a companion expression exists, so
                // anything it materializes is thrown away with this sink.
                let mut probe = MetaTemps::new(
                    self.out.signals.len() as u32,
                    d.ctx,
                    self.out.signals[d.target.0 as usize].declaration_span,
                );
                if n != 0
                    && (d.meta.is_some() || self.lower_meta_ir(&d.expr, n, &mut probe).is_some())
                {
                    discovered.push(d.target);
                }
            }
            for block in &self.out.event_blocks {
                for update in &block.updates {
                    if companion_ids.contains(&update.target.0)
                        || self.out.meta_of.contains_key(&update.target.0)
                    {
                        continue;
                    }
                    let n = self.out.signals[update.target.0 as usize].width;
                    let mut probe = MetaTemps::new(
                        self.out.signals.len() as u32,
                        block.ctx,
                        self.out.signals[update.target.0 as usize].declaration_span,
                    );
                    if n != 0
                        && (update.meta.is_some()
                            || self.lower_meta_ir(&update.expr, n, &mut probe).is_some())
                    {
                        discovered.push(update.target);
                    }
                }
            }
            discovered.sort_by_key(|signal| signal.0);
            discovered.dedup();
            if discovered.is_empty() {
                break;
            }
            for target in discovered {
                self.driven_companion(target);
            }
        }

        // Then emit exactly one companion write beside every write of a signal
        // that has a companion. Clean writes deliberately emit zero: without
        // that write a later clean override changed the value plane while an
        // earlier `X`/`Z` remained stale in the discriminant plane.
        let companion_ids: std::collections::HashSet<u32> =
            self.out.meta_of.values().copied().collect();
        // Operands hoisted out of the per-element unrolls below. Ids continue
        // from the current signal count and the signals are appended, in
        // creation order, once both write kinds are lowered.
        // Seeded only so the sink is constructible; every lowering below sets
        // `ctx`/`anchor` from the write it is lowering before using it.
        let anchor = self
            .out
            .signals
            .first()
            .map(|signal| signal.declaration_span)
            .unwrap_or_else(|| crate::diag::Span::new(crate::diag::FileId(0), 0..0));
        let mut temps = MetaTemps::new(self.out.signals.len() as u32, 0, anchor);
        let mut drivers = Vec::with_capacity(self.out.drivers.len() * 2);
        for mut driver in std::mem::take(&mut self.out.drivers) {
            if companion_ids.contains(&driver.target.0) {
                driver.meta = None;
                drivers.push(driver);
                continue;
            }
            let companion = self.out.meta_of.get(&driver.target.0).copied();
            temps.ctx = driver.ctx;
            temps.anchor = self.out.signals[driver.target.0 as usize].declaration_span;
            let meta = companion.map(|_| {
                let width = self.out.signals[driver.target.0 as usize].width;
                driver
                    .meta
                    .take()
                    .or_else(|| self.lower_meta_ir(&driver.expr, width, &mut temps))
                    .unwrap_or(Expr::Const(0))
            });
            let cond = driver.cond.clone();
            let ctx = driver.ctx;
            let span = driver.span;
            drivers.push(driver);
            if let (Some(companion), Some(expr)) = (companion, meta) {
                drivers.push(Driver {
                    target: SignalId(companion),
                    cond,
                    expr,
                    meta: None,
                    ctx,
                    span,
                });
            }
        }
        self.out.drivers = drivers;

        for block_index in 0..self.out.event_blocks.len() {
            let block_ctx = self.out.event_blocks[block_index].ctx;
            let mut updates =
                Vec::with_capacity(self.out.event_blocks[block_index].updates.len() * 2);
            for mut update in std::mem::take(&mut self.out.event_blocks[block_index].updates) {
                if companion_ids.contains(&update.target.0) {
                    update.meta = None;
                    updates.push(update);
                    continue;
                }
                let companion = self.out.meta_of.get(&update.target.0).copied();
                temps.ctx = block_ctx;
                temps.anchor = self.out.signals[update.target.0 as usize].declaration_span;
                let meta = companion.map(|_| {
                    let width = self.out.signals[update.target.0 as usize].width;
                    update
                        .meta
                        .take()
                        .or_else(|| self.lower_meta_ir(&update.expr, width, &mut temps))
                        .unwrap_or(Expr::Const(0))
                });
                let cond = update.cond.clone();
                let span = update.span;
                updates.push(update);
                if let (Some(companion), Some(expr)) = (companion, meta) {
                    updates.push(NextUpdate {
                        target: SignalId(companion),
                        cond,
                        expr,
                        meta: None,
                        span,
                    });
                }
            }
            self.out.event_blocks[block_index].updates = updates;
        }

        self.materialize_meta_temps(&mut temps);
    }

    /// Turn everything a sink hoisted into ordinary combinational signals.
    ///
    /// Temporaries are created inner-to-outer and `topo_order` sorts drivers by
    /// dependency before emission, so appending them is enough. The ids were
    /// handed out from the signal count when the sink was armed, which is why
    /// nothing may create a signal between arming and draining.
    pub(super) fn materialize_meta_temps(&mut self, temps: &mut MetaTemps) {
        for temp in std::mem::take(&mut temps.made) {
            debug_assert_eq!(temp.id as usize, self.out.signals.len());
            self.out.signals.push(Signal {
                path: format!("$metatmp{}", temp.id),
                declaration_span: temp.anchor,
                width: temp.width,
                real: false,
                integer: false,
                char: false,
                range: None,
                init: vec![0],
                enum_type: None,
            });
            self.out.metavalue_temps.insert(temp.id);
            self.out.drivers.push(Driver {
                target: SignalId(temp.id),
                cond: None,
                expr: temp.expr,
                meta: None,
                ctx: temp.ctx,
                span: None,
            });
        }
    }

    /// Arm [`Lowering::meta_temps`] so the `&self` helpers hoist instead of
    /// inlining. Ids continue from the current signal count.
    pub(super) fn arm_meta_temps(&self, ctx: u32, anchor: crate::diag::Span) {
        *self.meta_temps.borrow_mut() = MetaTemps::new(self.out.signals.len() as u32, ctx, anchor);
    }

    /// Materialize whatever the armed sink collected and leave it non-hoisting
    /// again. Safe to call when nothing was armed or nothing hoisted.
    pub(super) fn flush_meta_temps(&mut self) {
        let mut temps =
            std::mem::replace(&mut *self.meta_temps.borrow_mut(), MetaTemps::inline_only());
        self.materialize_meta_temps(&mut temps);
    }

    /// Rewrite each single-element read of a metavalue vector into its 9-value
    /// reconstruction (companion nibble when a metavalue, else the value bit).
    /// A post-pass so it sees driven companions (created in propagation), not
    /// just init ones.
    pub(super) fn reconstruct_reads(&mut self) {
        let meta_of = self.out.meta_of.clone();
        // Companion id -> how many elements it describes, so the comparison
        // guard can ask its per-element question without the signal table.
        let elems: HashMap<u32, u32> = meta_of
            .iter()
            .map(|(&base, &companion)| (companion, self.out.signals[base as usize].width))
            .collect();
        let encodings: HashMap<u32, LogicEncoding> = meta_of
            .iter()
            .filter_map(|(&base, &companion)| {
                let element = self.out.array_element_enums.get(&base)?;
                self.logic_encodings
                    .get(element)
                    .cloned()
                    .map(|encoding| (companion, encoding))
            })
            .collect();
        for d in &mut self.out.drivers {
            if let Some(c) = &mut d.cond {
                reconstruct_expr(c, &meta_of, &elems, &encodings);
            }
            reconstruct_expr(&mut d.expr, &meta_of, &elems, &encodings);
        }
        for b in &mut self.out.event_blocks {
            reconstruct_expr(&mut b.condition, &meta_of, &elems, &encodings);
            for u in &mut b.updates {
                if let Some(c) = &mut u.cond {
                    reconstruct_expr(c, &meta_of, &elems, &encodings);
                }
                reconstruct_expr(&mut u.expr, &meta_of, &elems, &encodings);
            }
        }
    }
}
