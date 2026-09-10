//! Queries, validation, scheduling decomposition, and textual rendering.

use std::collections::HashSet;

use super::*;

/// Collect every signal an IR expression reads (`Current`/`Old`/`Event`
/// leaves) into `out`, in first-seen order.
pub fn read_set(e: &Expr, out: &mut Vec<SignalId>) {
    match e {
        // The operands are read through `inner` too; walking both would list
        // them twice and `dedup` is not applied everywhere this feeds.
        Expr::MetaCmp { inner, .. } => read_set(inner, out),
        Expr::Current(id) | Expr::Old(id) | Expr::Event(id) => out.push(*id),
        Expr::CCall { args, .. } => {
            for a in args {
                read_set(a, out);
            }
        }
        Expr::Unary { rhs, .. } => read_set(rhs, out),
        Expr::Binary { lhs, rhs, .. } => {
            read_set(lhs, out);
            read_set(rhs, out);
        }
        Expr::Slice { base, .. } => read_set(base, out),
        Expr::TableLookup { index, .. } => read_set(index, out),
        // `valid` is synthesized entirely from `index`, so walking both would
        // duplicate every sensitivity leaf.
        Expr::CheckedIndex { index, .. } => read_set(index, out),
        Expr::Select { cond, then, els } => {
            read_set(cond, out);
            read_set(then, out);
            read_set(els, out);
        }
        Expr::Const(_) | Expr::WideConst(_) | Expr::Real(_) | Expr::Logic(_) | Expr::Unknown => {}
    }
}

/// Sort and deduplicate a signal list in place.
fn dedup(v: &mut Vec<SignalId>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|id| seen.insert(*id));
}

/// Validation walk over an expression (see [`Design::validate`]).
fn check_expr(e: &Expr, n: u32, tables: &[LookupTable], issues: &mut Vec<String>, ctx: &str) {
    match e {
        // Every one is rewritten once companions are known; one surviving means
        // that pass did not reach it, and the backends have no meaning for it.
        Expr::MetaCmp { .. } => issues.push(format!(
            "{ctx}: contains an unresolved metavalue comparison"
        )),
        Expr::CCall { args, .. } => {
            for a in args {
                check_expr(a, n, tables, issues, ctx);
            }
        }
        Expr::Current(id) | Expr::Old(id) | Expr::Event(id) => {
            if id.0 >= n {
                issues.push(format!("{ctx}: signal id {} out of range (n={n})", id.0));
            }
        }
        Expr::Unknown => issues.push(format!("{ctx}: contains an Unknown (unlowered) expression")),
        Expr::Unary { rhs, .. } => check_expr(rhs, n, tables, issues, ctx),
        Expr::Binary { lhs, rhs, .. } => {
            check_expr(lhs, n, tables, issues, ctx);
            check_expr(rhs, n, tables, issues, ctx);
        }
        Expr::Slice { base, hi, lo } => {
            if lo > hi {
                issues.push(format!("{ctx}: slice bounds lo {lo} > hi {hi}"));
            }
            check_expr(base, n, tables, issues, ctx);
        }
        Expr::TableLookup { table, index } => {
            if table.0 >= tables.len() {
                issues.push(format!(
                    "{ctx}: lookup table id {} out of range (n={})",
                    table.0,
                    tables.len()
                ));
            }
            check_expr(index, n, tables, issues, ctx);
        }
        Expr::CheckedIndex { index, valid, .. } => {
            check_expr(index, n, tables, issues, ctx);
            check_expr(valid, n, tables, issues, ctx);
        }
        Expr::Select { cond, then, els } => {
            check_expr(cond, n, tables, issues, ctx);
            check_expr(then, n, tables, issues, ctx);
            check_expr(els, n, tables, issues, ctx);
        }
        Expr::Const(_) | Expr::WideConst(_) | Expr::Real(_) | Expr::Logic(_) => {}
    }
}

/// A unit of behaviour the scheduler dispatches, with its **sensitivity**
/// (the signals it reads) and **write set** (the signals it drives). This is
/// the process view the LLVM backend compiles and the interpreter dispatches
/// on (spec Stage 6 / the compiled-backend plan, B1).
#[derive(Clone, Debug)]
pub struct Process {
    /// What kind of scheduled process this is.
    pub kind: ProcessKind,
    /// Source labels of the contexts contributing to this scheduled process.
    /// A resolved signal may combine more than one named source process.
    pub labels: Vec<String>,
    /// Signals read by the process's conditions/expressions (sensitivity).
    pub reads: Vec<SignalId>,
    /// Signals the process drives.
    pub writes: Vec<SignalId>,
}

/// One source-level runtime indexing domain. Backends latch a one-based index
/// into [`Design::index_sites`] so the generated executable can report the
/// source location and the range without embedding compiler data in the LLVM
/// engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IndexSite {
    /// The access site, for the runtime failure report.
    pub span: crate::diag::Span,
    /// The declaration's written left bound.
    pub left: i64,
    /// The declaration's written right bound.
    pub right: i64,
}

/// How a scheduled process is driven.
#[derive(Clone, Debug)]
pub enum ProcessKind {
    /// A combinational target, resolved from the drivers that target it, in
    /// source order (spec 3.14 last-writer-wins). `drivers` indexes
    /// `Design::drivers`.
    Comb {
        /// The signal this process settles.
        target: SignalId,
        /// Indices into [`Design::drivers`], in source order.
        drivers: Vec<usize>,
    },
    /// A clocked event block. `block` indexes `Design::event_blocks`.
    Event {
        /// Index into [`Design::event_blocks`].
        block: usize,
    },
}

impl Design {
    /// The assignment sites a dynamic range failure (spec 3.26) can be blamed
    /// on: the distinct source spans of the drivers and next-state updates
    /// that write a ranged signal, in lowering order.
    ///
    /// The runtime latches an index into this table plus one, so `0` keeps its
    /// meaning of "no site" and the report falls back to the signal's
    /// declaration. Both engines call *this* to build the table rather than
    /// walking the design themselves: the index is the whole contract between
    /// the value the hardware latches and the string the harness prints, and
    /// two walks that disagree by one entry would misattribute every failure
    /// after the first divergence.
    pub fn range_sites(&self) -> Vec<crate::diag::Span> {
        fn process_place_signals(
            ir: &crate::ir::process::ProcessIr,
            value: crate::ir::process::ProcessValueId,
            signals: &mut Vec<SignalId>,
        ) {
            let Some(value) = ir.values.get(value.0 as usize) else {
                return;
            };
            match &value.kind {
                crate::ir::process::ProcessValueKind::Signal { signals: ids, .. } => {
                    signals.extend(ids.iter().copied());
                }
                crate::ir::process::ProcessValueKind::Storage(storage) => {
                    if let Some(storage) = ir.storages.get(storage.0 as usize) {
                        signals.extend(
                            storage
                                .bindings
                                .iter()
                                .filter(|binding| {
                                    matches!(
                                        binding.direction,
                                        LayoutDirection::In | LayoutDirection::InOut
                                    )
                                })
                                .map(|binding| binding.signal),
                        );
                    }
                }
                crate::ir::process::ProcessValueKind::Field { base, .. }
                | crate::ir::process::ProcessValueKind::Index { base, .. } => {
                    process_place_signals(ir, *base, signals);
                }
                crate::ir::process::ProcessValueKind::Concat(parts) => {
                    for part in parts {
                        process_place_signals(ir, *part, signals);
                    }
                }
                _ => {}
            }
        }

        let mut sites = Vec::new();
        let mut seen = HashSet::new();
        let mut add = |target: SignalId, span: Option<crate::diag::Span>| {
            let ranged = self
                .signals
                .get(target.0 as usize)
                .is_some_and(|s| s.range.is_some());
            if let (true, Some(span)) = (ranged, span) {
                if seen.insert(span) {
                    sites.push(span);
                }
            }
        };
        for driver in &self.drivers {
            add(driver.target, driver.span);
        }
        for block in &self.event_blocks {
            for update in &block.updates {
                add(update.target, update.span);
            }
        }
        // Process IR currently coexists with the normalized hardware graph,
        // so identical migrated writes deduplicate by span. Source-first
        // testbench writes that have no legacy driver still need a stable site
        // id for the same public range-failure ABI.
        for process in &self.process_ir.processes {
            for block in &process.blocks {
                for instruction in &block.instructions {
                    let (target, span) = match instruction {
                        crate::ir::process::ProcessInstruction::Assign { target, span, .. }
                        | crate::ir::process::ProcessInstruction::Schedule {
                            target, span, ..
                        } => (*target, *span),
                        crate::ir::process::ProcessInstruction::Declare { .. }
                        | crate::ir::process::ProcessInstruction::Runtime { .. } => continue,
                    };
                    let mut signals = Vec::new();
                    process_place_signals(&self.process_ir, target, &mut signals);
                    for signal in signals {
                        add(signal, Some(span));
                    }
                }
            }
        }
        sites
    }

    /// Runtime index domains referenced by this design, in stable lowering
    /// order. A checked index is commonly cloned into every arm of a flattened
    /// array mux/write; deduplication makes all those copies one diagnostic
    /// site and one runtime id.
    pub fn index_sites(&self) -> Vec<IndexSite> {
        /// Collect the distinct runtime index sites in an expression, so each
        /// reports once.
        fn collect(expr: &Expr, sites: &mut Vec<IndexSite>, seen: &mut HashSet<IndexSite>) {
            match expr {
                Expr::CheckedIndex {
                    index,
                    valid,
                    left,
                    right,
                    span,
                } => {
                    let site = IndexSite {
                        span: *span,
                        left: *left,
                        right: *right,
                    };
                    if seen.insert(site) {
                        sites.push(site);
                    }
                    collect(index, sites, seen);
                    collect(valid, sites, seen);
                }
                Expr::MetaCmp {
                    operands, inner, ..
                } => {
                    for operand in operands {
                        collect(operand, sites, seen);
                    }
                    collect(inner, sites, seen);
                }
                Expr::CCall { args, .. } => {
                    for argument in args {
                        collect(argument, sites, seen);
                    }
                }
                Expr::Unary { rhs, .. } | Expr::Slice { base: rhs, .. } => {
                    collect(rhs, sites, seen)
                }
                Expr::TableLookup { index, .. } => collect(index, sites, seen),
                Expr::Binary { lhs, rhs, .. } => {
                    collect(lhs, sites, seen);
                    collect(rhs, sites, seen);
                }
                Expr::Select { cond, then, els } => {
                    collect(cond, sites, seen);
                    collect(then, sites, seen);
                    collect(els, sites, seen);
                }
                Expr::Const(_)
                | Expr::WideConst(_)
                | Expr::Real(_)
                | Expr::Logic(_)
                | Expr::Current(_)
                | Expr::Old(_)
                | Expr::Event(_)
                | Expr::Unknown => {}
            }
        }

        let mut sites = Vec::new();
        let mut seen = HashSet::new();
        for driver in &self.drivers {
            if let Some(condition) = &driver.cond {
                collect(condition, &mut sites, &mut seen);
            }
            collect(&driver.expr, &mut sites, &mut seen);
        }
        for block in &self.event_blocks {
            collect(&block.condition, &mut sites, &mut seen);
            for update in &block.updates {
                if let Some(condition) = &update.cond {
                    collect(condition, &mut sites, &mut seen);
                }
                collect(&update.expr, &mut sites, &mut seen);
            }
        }
        // Process IR owns the source-level checked access once generated-C is
        // gone. Keep legacy expression sites first for ABI stability during
        // migration, then append process-only sites. Arena nodes are already
        // dependency ordered, and the set removes cloned accesses.
        for value in &self.process_ir.values {
            if let crate::ir::process::ProcessValueKind::CheckedIndex {
                left, right, span, ..
            } = value.kind
            {
                let site = IndexSite { span, left, right };
                if seen.insert(site) {
                    sites.push(site);
                }
            }
        }
        sites
    }

    /// Semantic width presented to backends for one flattened signal. When a
    /// persisted source layout exists it is the authority; hand-built IR used
    /// by backend tests remains valid without layout metadata.
    pub fn signal_width(&self, id: SignalId) -> Option<u32> {
        let signal = self.signals.get(id.0 as usize)?;
        match self.source_layouts.get(&signal.path) {
            Some(layout) => layout
                .bit_width()
                .and_then(|width| u32::try_from(width).ok())
                .or_else(|| (signal.width == 0).then_some(0)),
            None => Some(signal.width),
        }
    }

    /// Check the IR is well-formed enough for a backend to compile: signal
    /// ids in range, no `Unknown` (unlowered) expressions, concrete widths,
    /// and valid slice bounds. Returns a list of problems — empty means the
    /// design is safe to hand to codegen. Pure; callers decide how to react.
    pub fn validate(&self) -> Vec<String> {
        let n = self.signals.len() as u32;
        let mut issues = Vec::new();

        for (id, table) in self.lookup_tables.iter().enumerate() {
            if table.values.is_empty() {
                issues.push(format!("lookup table {id} is empty"));
            }
            if !(1..=64).contains(&table.element_width) {
                issues.push(format!(
                    "lookup table {id} has invalid element width {}",
                    table.element_width
                ));
            } else {
                let mask = if table.element_width == 64 {
                    u64::MAX
                } else {
                    (1u64 << table.element_width) - 1
                };
                if table.values.iter().any(|value| value & !mask != 0) {
                    issues.push(format!(
                        "lookup table {id} contains a value wider than {} bits",
                        table.element_width
                    ));
                }
            }
        }

        // Signals codegen actually touches (driven or read). An unreferenced
        // width-0 signal — e.g. an instance-binding `let` placeholder — is
        // harmless, so only flag unknown widths on referenced signals.
        let mut referenced: std::collections::HashSet<SignalId> = std::collections::HashSet::new();
        let collect = |e: &Expr| {
            let mut v = Vec::new();
            read_set(e, &mut v);
            v
        };
        for d in &self.drivers {
            referenced.insert(d.target);
            if let Some(c) = &d.cond {
                referenced.extend(collect(c));
            }
            referenced.extend(collect(&d.expr));
        }
        for eb in &self.event_blocks {
            referenced.extend(collect(&eb.condition));
            for u in &eb.updates {
                referenced.insert(u.target);
                if let Some(c) = &u.cond {
                    referenced.extend(collect(c));
                }
                referenced.extend(collect(&u.expr));
            }
        }
        for (i, s) in self.signals.iter().enumerate() {
            if s.width == 0 && referenced.contains(&SignalId(i as u32)) {
                issues.push(format!("signal `{}` has unknown width (0)", s.path));
            }
            let Some(layout) = self.source_layouts.get(&s.path) else {
                continue;
            };
            if matches!(
                &layout.kind,
                LayoutKind::Struct { .. } | LayoutKind::Array { .. }
            ) {
                issues.push(format!(
                    "signal `{}` still has an aggregate source layout instead of a flattened leaf",
                    s.path
                ));
                continue;
            }
            match layout
                .bit_width()
                .and_then(|width| u32::try_from(width).ok())
            {
                Some(width) if width != s.width => issues.push(format!(
                    "signal `{}` width {} disagrees with its source layout width {width}",
                    s.path, s.width
                )),
                None if s.width != 0 => issues.push(format!(
                    "signal `{}` has no concrete width in its source layout",
                    s.path
                )),
                _ => {}
            }
        }
        let target = |id: SignalId, what: &str, issues: &mut Vec<String>| {
            if id.0 >= n {
                issues.push(format!(
                    "{what}: target signal id {} out of range (n={n})",
                    id.0
                ));
            }
        };
        // A driver's position in this vector means nothing to the person who
        // wrote the design; the signal it drives is what they can go and look
        // at. "driver 0 expr: contains an Unknown" sent readers to an IR dump
        // to work out which line it meant.
        let name = |id: SignalId| -> String {
            self.signals
                .get(id.0 as usize)
                .map(|s| format!("`{}`", s.path))
                .unwrap_or_else(|| format!("signal id {}", id.0))
        };
        for d in &self.drivers {
            let ctx = format!("the driver for {}", name(d.target));
            target(d.target, &ctx, &mut issues);
            if d.meta.is_some() {
                issues.push(format!(
                    "{ctx}: still carries unexpanded metavalue metadata"
                ));
            }
            if let Some(c) = &d.cond {
                check_expr(
                    c,
                    n,
                    &self.lookup_tables,
                    &mut issues,
                    &format!("{ctx} (condition)"),
                );
            }
            check_expr(&d.expr, n, &self.lookup_tables, &mut issues, &ctx);
        }
        for (bi, eb) in self.event_blocks.iter().enumerate() {
            // An event block has no single target, so name it by what it
            // updates; the index is the fallback for an empty one.
            let block = match eb.updates.first() {
                Some(u) => format!("the event block updating {}", name(u.target)),
                None => format!("event block {bi}"),
            };
            check_expr(
                &eb.condition,
                n,
                &self.lookup_tables,
                &mut issues,
                &format!("{block} (condition)"),
            );
            for u in &eb.updates {
                let ctx = format!("{block}, update of {}", name(u.target));
                target(u.target, &ctx, &mut issues);
                if u.meta.is_some() {
                    issues.push(format!(
                        "{ctx}: still carries unexpanded metavalue metadata"
                    ));
                }
                if let Some(c) = &u.cond {
                    check_expr(
                        c,
                        n,
                        &self.lookup_tables,
                        &mut issues,
                        &format!("{ctx} (condition)"),
                    );
                }
                check_expr(&u.expr, n, &self.lookup_tables, &mut issues, &ctx);
            }
        }
        issues.extend(self.process_ir.validate(n));
        for (index, value) in self.process_ir.values.iter().enumerate() {
            if let ProcessValueKind::TableLookup { table, .. } = value.kind {
                if table.0 >= self.lookup_tables.len() {
                    issues.push(format!(
                        "process value {:?} references invalid lookup table {}",
                        ProcessValueId(index as u32),
                        table.0
                    ));
                }
            }
        }
        issues
    }

    /// The process decomposition: one combinational process per driven signal
    /// (grouping its source-ordered drivers) and one per event block, each
    /// with its sensitivity and write set. Combinational targets keep their
    /// first-seen order so source-order override is preserved.
    pub fn processes(&self) -> Vec<Process> {
        let mut procs = Vec::new();

        // Group combinational drivers by target, first-seen order.
        let mut order: Vec<SignalId> = Vec::new();
        let mut by_target: std::collections::HashMap<SignalId, Vec<usize>> =
            std::collections::HashMap::new();
        for (i, d) in self.drivers.iter().enumerate() {
            by_target.entry(d.target).or_insert_with(|| {
                order.push(d.target);
                Vec::new()
            });
            by_target.get_mut(&d.target).unwrap().push(i);
        }
        for target in order {
            let drivers = by_target.remove(&target).unwrap();
            let mut reads = Vec::new();
            let mut labels = self
                .resolved_process_labels
                .get(&target.0)
                .cloned()
                .unwrap_or_default();
            for &di in &drivers {
                let d = &self.drivers[di];
                if let Some(label) = self.process_labels.get(&d.ctx) {
                    labels.push(label.clone());
                }
                if let Some(c) = &d.cond {
                    read_set(c, &mut reads);
                }
                read_set(&d.expr, &mut reads);
            }
            dedup(&mut reads);
            labels.sort();
            labels.dedup();
            procs.push(Process {
                kind: ProcessKind::Comb { target, drivers },
                labels,
                reads,
                writes: vec![target],
            });
        }

        // One process per event block.
        for (bi, eb) in self.event_blocks.iter().enumerate() {
            let mut reads = Vec::new();
            read_set(&eb.condition, &mut reads);
            let mut writes = Vec::new();
            for u in &eb.updates {
                if let Some(c) = &u.cond {
                    read_set(c, &mut reads);
                }
                read_set(&u.expr, &mut reads);
                writes.push(u.target);
            }
            dedup(&mut reads);
            dedup(&mut writes);
            procs.push(Process {
                kind: ProcessKind::Event { block: bi },
                labels: self
                    .process_labels
                    .get(&eb.ctx)
                    .cloned()
                    .into_iter()
                    .collect(),
                reads,
                writes,
            });
        }
        procs
    }

    /// Render normalized IR (backs `siox ir`).
    pub fn to_ir_string(&self) -> String {
        let mut out = String::new();
        for (id, table) in self.lookup_tables.iter().enumerate() {
            let values = table
                .values
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "lookup#{id} : {} = [{values}]\n",
                table.element_width
            ));
        }
        for s in &self.signals {
            let w = if s.width == 0 {
                "?".to_string()
            } else {
                s.width.to_string()
            };
            out.push_str(&format!("signal {} : {w}\n", s.path));
        }
        for d in &self.drivers {
            let cond = match &d.cond {
                Some(c) => format!("  when {}", render(c, self)),
                None => String::new(),
            };
            let labels = self
                .resolved_process_labels
                .get(&d.target.0)
                .cloned()
                .or_else(|| {
                    self.process_labels
                        .get(&d.ctx)
                        .map(|name| vec![name.clone()])
                })
                .unwrap_or_default();
            let label = if labels.is_empty() {
                String::new()
            } else {
                format!(" [{}]", labels.join(", "))
            };
            out.push_str(&format!(
                "driver{label} {} = {}{cond}\n",
                self.signals[d.target.0 as usize].path,
                render(&d.expr, self)
            ));
        }
        for eb in &self.event_blocks {
            let label = self
                .process_labels
                .get(&eb.ctx)
                .map(|name| format!(" [{name}]"))
                .unwrap_or_default();
            out.push_str(&format!(
                "event{label} ({}):\n",
                render(&eb.condition, self)
            ));
            for u in &eb.updates {
                let cond = match &u.cond {
                    Some(c) => format!("  when {}", render(c, self)),
                    None => String::new(),
                };
                out.push_str(&format!(
                    "    next {} = {}{cond}\n",
                    self.signals[u.target.0 as usize].path,
                    render(&u.expr, self)
                ));
            }
        }
        out.push_str(&self.process_ir.to_ir_string());
        out
    }
}

// --- rendering --------------------------------------------------------------

/// Render an expression for the IR dump.
pub(super) fn render(e: &Expr, d: &Design) -> String {
    match e {
        Expr::MetaCmp { inner, .. } => format!("metacmp({})", render(inner, d)),
        Expr::CCall { name, args, .. } => {
            let a = args
                .iter()
                .map(|x| render(x, d))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{name}({a})")
        }
        Expr::Const(v) => v.to_string(),
        Expr::WideConst(words) => {
            let mut parts = words.iter().rev();
            let mut text = format!("0x{:x}", parts.next().copied().unwrap_or(0));
            for word in parts {
                text.push_str(&format!("{word:016x}"));
            }
            text
        }
        Expr::Real(x) => format!("{x}"),
        Expr::Logic(c) => format!("'{c}'"),
        Expr::Current(id) => d.signals[id.0 as usize].path.clone(),
        Expr::Old(id) => format!("Old({})", d.signals[id.0 as usize].path),
        Expr::Event(id) => format!("Event({})", d.signals[id.0 as usize].path),
        Expr::Unary { op, rhs } => format!("{}{}", un_sym(*op), paren(rhs, d)),
        Expr::Binary { op, lhs, rhs } => {
            format!("{} {} {}", paren(lhs, d), bin_sym(*op), paren(rhs, d))
        }
        Expr::Slice { base, hi, lo } => format!("{}[{hi}..{lo}]", paren(base, d)),
        Expr::TableLookup { table, index } => {
            format!("lookup#{}[{}]", table.0, render(index, d))
        }
        Expr::CheckedIndex {
            index, left, right, ..
        } => format!("checked({}, {left}..{right})", render(index, d)),
        Expr::Select { cond, then, els } => {
            format!(
                "{} ? {} : {}",
                paren(cond, d),
                paren(then, d),
                paren(els, d)
            )
        }
        Expr::Unknown => "?".to_string(),
    }
}

/// Render an expression parenthesized, for use as an operand.
fn paren(e: &Expr, d: &Design) -> String {
    match e {
        Expr::Binary { .. } | Expr::Unary { .. } => format!("({})", render(e, d)),
        _ => render(e, d),
    }
}

/// The dump symbol for a prefix operator.
fn un_sym(op: UnOp) -> &'static str {
    match op {
        UnOp::Not => "not ",
        UnOp::Neg => "-",
        UnOp::RealToInt => "integer",
    }
}

/// The dump symbol for an infix operator.
fn bin_sym(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::SAdd => "+",
        BinOp::SSub => "-",
        BinOp::SMul => "*",
        BinOp::SDiv => "/",
        BinOp::And => "and",
        BinOp::Or => "or",
        BinOp::Xor => "xor",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::AShr => ">>",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::SLt => "<",
        BinOp::SLe => "<=",
        BinOp::SGt => ">",
        BinOp::SGe => ">=",
        BinOp::FAdd => "+.",
        BinOp::FSub => "-.",
        BinOp::FMul => "*.",
        BinOp::FDiv => "/.",
        BinOp::FEq => "==.",
        BinOp::FNe => "!=.",
        BinOp::FLt => "<.",
        BinOp::FLe => "<=.",
        BinOp::FGt => ">.",
        BinOp::FGe => ">=.",
    }
}
