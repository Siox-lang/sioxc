//! Digital simulation IR for siox Phase 1 (spec Stage 6).
//!
//! Lowers the typed, elaborated design into a simulator-friendly form where
//! event dependencies and combinational dependencies are explicit, and
//! sequential next-state updates are separated from immediate local
//! assignments. `::event` and `::old` become explicit IR operations.
//!
//! Spec IR distinction:
//! ```text
//! Driver(signal, expression, condition)              // combinational
//! OnEvent(event_condition): next(signal) = expression // sequential
//! ```
//! and `Rising(clk)` lowers to
//! `Event(clk) && Old(clk) == '0' && Current(clk) == '1'`.
//!
//! The IR data types are deliberately **language-neutral** — they use their own
//! `BinOp`/`UnOp` and never reference the siox AST — so that other HDL frontends
//! could target the same IR. Only `lower` (the siox frontend lowering) consumes
//! the siox AST.
//!
//! Phase-1 scope: lowers the behaviour of each non-extern entity in the design,
//! with the entity's declared (possibly parametric) widths. Per-instance width
//! specialization and cross-instance flattening/connection are follow-ups.

use std::collections::HashMap;

use siox_diag::DiagnosticSink;
use siox_elab::Hierarchy;
use siox_syntax::ast::{self, BinOp as AstBinOp, UnOp as AstUnOp};
use siox_syntax::Module;

/// A design ready to simulate: signals, combinational drivers, and event blocks.
#[derive(Default)]
pub struct Design {
    pub signals: Vec<Signal>,
    pub drivers: Vec<Driver>,
    pub event_blocks: Vec<EventBlock>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SignalId(pub u32);

#[derive(Clone, Debug)]
pub struct Signal {
    /// Hierarchical path, e.g. `Counter.count`.
    pub path: String,
    /// Bit width; `0` means "not yet known" (a parametric width).
    pub width: u32,
}

/// A combinational driver: `signal = expr` under `cond` (spec 3.14 source-order
/// override is resolved during lowering into a priority chain).
#[derive(Clone, Debug)]
pub struct Driver {
    pub target: SignalId,
    pub cond: Option<Expr>,
    pub expr: Expr,
}

/// An event-controlled block: on `condition`, queue `next(target) = expr`
/// (spec 3.13 next-state semantics).
#[derive(Clone, Debug)]
pub struct EventBlock {
    pub condition: Expr,
    pub updates: Vec<NextUpdate>,
}

#[derive(Clone, Debug)]
pub struct NextUpdate {
    pub target: SignalId,
    pub cond: Option<Expr>,
    pub expr: Expr,
}

/// IR expression. `::event`/`::old` are first-class so the scheduler can read
/// them directly; `clk::rising` lowers into `Event`/`Old`/`Current`.
#[derive(Clone, Debug)]
pub enum Expr {
    Const(u64),
    Logic(char),
    Current(SignalId),
    Old(SignalId),
    Event(SignalId),
    Unary { op: UnOp, rhs: Box<Expr> },
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr> },
    /// Bit slice `base[hi..lo]` (inclusive), value `(base >> lo) & mask(hi-lo+1)`.
    Slice { base: Box<Expr>, hi: u32, lo: u32 },
    /// A reference that could not be lowered (unknown signal, unsupported form).
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    And,
    Nand,
    Or,
    Nor,
    Xor,
    Xnor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Lower the elaborated design into simulation IR.
pub fn lower(modules: &[Module], hier: &Hierarchy, sink: &mut DiagnosticSink) -> Design {
    let mut l = Lowering::new(sink);
    l.collect(modules);
    l.enum_variants = enum_discriminants(modules);

    // The entity types that appear in the elaborated hierarchy, in first-seen
    // order, deduplicated. Each entity's parameters are taken from its first
    // instance, so `uint[W]` lowers with the instance's concrete `W`.
    let mut seen = Vec::new();
    for inst in &hier.instances {
        if !seen.contains(&inst.entity) {
            seen.push(inst.entity.clone());
            l.entity_params.entry(inst.entity.clone()).or_insert_with(|| {
                inst.params
                    .iter()
                    .filter_map(|(n, v)| match v {
                        siox_elab::ParamValue::Int(i) => Some((n.clone(), *i)),
                        siox_elab::ParamValue::Unknown => None,
                    })
                    .collect()
            });
        }
    }
    for name in &seen {
        l.lower_entity(name);
    }
    l.out
}

struct Lowering<'a> {
    #[allow(dead_code)]
    sink: &'a mut DiagnosticSink,
    entities: HashMap<String, &'a ast::EntityDecl>,
    impls: HashMap<String, Vec<&'a ast::ImplDecl>>,
    /// Entity name -> its instance's concrete parameter values.
    entity_params: HashMap<String, HashMap<String, i64>>,
    /// Enum name -> variant name -> discriminant value.
    enum_variants: HashMap<String, HashMap<String, u64>>,
    /// Struct name -> its declaration (for flattening struct signals).
    structs: HashMap<String, &'a ast::StructDecl>,
    out: Design,
    /// Signal name -> id, valid while lowering a single entity.
    locals: HashMap<String, SignalId>,
}

impl<'a> Lowering<'a> {
    fn new(sink: &'a mut DiagnosticSink) -> Self {
        Lowering {
            sink,
            entities: HashMap::new(),
            impls: HashMap::new(),
            entity_params: HashMap::new(),
            enum_variants: HashMap::new(),
            structs: HashMap::new(),
            out: Design::default(),
            locals: HashMap::new(),
        }
    }

    fn collect(&mut self, modules: &'a [Module]) {
        for m in modules {
            for item in &m.items {
                match item {
                    ast::Item::Entity(e) => {
                        self.entities.insert(e.name.text.clone(), e);
                    }
                    ast::Item::Struct(s) => {
                        self.structs.insert(s.name.text.clone(), s);
                    }
                    ast::Item::Impl(im) if im.trait_.is_none() => {
                        if let Some(name) = type_head_name(&im.target) {
                            self.impls.entry(name.to_string()).or_default().push(im);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn lower_entity(&mut self, name: &str) {
        let Some(edecl) = self.entities.get(name).copied() else { return };
        // Extern entities are black boxes; `#[test]` entities are testbenches
        // (stimulus, not hardware) and are run by the Stage-8 test runner.
        if edecl.is_extern || has_attr(edecl, "test") {
            return;
        }

        // Signals: ports, then impl-level `let` state. Build the local name map.
        // Struct-typed signals flatten into one scalar signal per leaf field.
        let env = self.entity_params.get(name).cloned().unwrap_or_default();
        self.locals.clear();
        for p in &edecl.ports {
            self.add_typed_signal(name, &p.name.text, &p.ty, &env);
        }
        let impls: Vec<&ast::ImplDecl> = self.impls.get(name).cloned().unwrap_or_default();
        for im in &impls {
            for item in &im.items {
                if let ast::ImplItem::Let(l) = item {
                    if let Some(ty) = &l.ty {
                        self.add_typed_signal(name, &l.name.text, ty, &env);
                    } else {
                        self.add_signal(name, &l.name.text, 0);
                    }
                }
            }
        }

        // Behaviour: each bare statement is a driver or an event block.
        for im in &impls {
            for item in &im.items {
                if let ast::ImplItem::Stmt(stmt) = item {
                    self.lower_stmt(stmt, None);
                }
            }
        }
    }

    fn add_signal(&mut self, entity: &str, name: &str, width: u32) {
        let id = SignalId(self.out.signals.len() as u32);
        self.out.signals.push(Signal { path: format!("{entity}.{name}"), width });
        self.locals.insert(name.to_string(), id);
    }

    /// Add a signal for `name: ty`, flattening composites into scalar leaves: a
    /// struct into one signal per field (`s.valid`), an array into one per
    /// element (`a[0]`). Nested composites recurse. An integer vector
    /// (`uint[8]`) stays a single scalar signal.
    fn add_typed_signal(&mut self, entity: &str, name: &str, ty: &ast::Type, env: &HashMap<String, i64>) {
        if let Some(fields) = self.struct_fields(ty) {
            for (fname, fty) in fields {
                self.add_typed_signal(entity, &format!("{name}.{fname}"), &fty, env);
            }
        } else if let Some((elem, len)) = array_of(ty, env) {
            for i in 0..len {
                self.add_typed_signal(entity, &format!("{name}[{i}]"), elem, env);
            }
        } else {
            self.add_signal(entity, name, type_width(ty, env));
        }
    }

    /// The `(field name, field type)` list if `ty` names a known struct.
    fn struct_fields(&self, ty: &ast::Type) -> Option<Vec<(String, ast::Type)>> {
        if let ast::Type::Path(p) = ty {
            if p.segments.len() == 1 {
                if let Some(s) = self.structs.get(&p.segments[0].text) {
                    return Some(s.fields.iter().map(|f| (f.name.text.clone(), f.ty.clone())).collect());
                }
            }
        }
        None
    }

    /// Lower a top-level (combinational-context) statement. `cond` accumulates
    /// the enclosing combinational conditions.
    fn lower_stmt(&mut self, stmt: &ast::Stmt, cond: Option<Expr>) {
        match stmt {
            ast::Stmt::Assign { target, value, .. } => {
                if let Some(target) = self.target_signal(target) {
                    let expr = self.lower_expr(value);
                    self.out.drivers.push(Driver { target, cond, expr });
                }
            }
            ast::Stmt::If(iff) => {
                if expr_is_event(&iff.cond) {
                    // Event-controlled block (spec 3.11): the body's assignments
                    // become next-state updates (spec 3.13).
                    let condition = self.lower_expr(&iff.cond);
                    let mut updates = Vec::new();
                    self.lower_event_block(&iff.then, None, &mut updates);
                    // An `else` on an event block is unusual; lower it under the
                    // negated event for completeness.
                    if let Some(eb) = iff.else_.as_deref() {
                        let neg = Some(not(self.lower_expr(&iff.cond)));
                        self.lower_event_else(eb, neg, &mut updates);
                    }
                    self.out.event_blocks.push(EventBlock { condition, updates });
                } else {
                    // Combinational conditional: assignments become conditional
                    // drivers; the `else` adds the negated condition.
                    let c = self.lower_expr(&iff.cond);
                    let then_cond = Some(and(cond.clone(), c.clone()));
                    for s in &iff.then.stmts {
                        self.lower_stmt(s, then_cond.clone());
                    }
                    if let Some(eb) = iff.else_.as_deref() {
                        let else_cond = Some(and(cond, not(c)));
                        self.lower_combinational_else(eb, else_cond);
                    }
                }
            }
            ast::Stmt::Match(m) => {
                // Combinational match: each arm becomes conditional drivers
                // guarded by `scrutinee == variant` with first-match priority.
                let scrut = self.lower_expr(&m.scrutinee);
                let mut remaining = cond;
                for arm in &m.arms {
                    let mc = self.arm_match_cond(&arm.pattern, &scrut);
                    let fire = match &mc {
                        Some(c) => Some(and(remaining.clone(), c.clone())),
                        None => remaining.clone(),
                    };
                    for s in &arm.body.stmts {
                        self.lower_stmt(s, fire.clone());
                    }
                    remaining = match mc {
                        Some(c) => Some(and(remaining, not(c))),
                        None => Some(Expr::Const(0)),
                    };
                }
            }
            // Other statement forms (for, let, expr, return) are not lowered yet.
            _ => {}
        }
    }

    /// The condition under which a match arm fires: `scrut == <variant value>`
    /// for an enum path, or always (`None`) for a wildcard.
    fn arm_match_cond(&self, pattern: &ast::Pattern, scrut: &Expr) -> Option<Expr> {
        match pattern {
            ast::Pattern::Path(p) if p.segments.len() >= 2 => {
                let disc = self
                    .enum_variants
                    .get(&p.segments[0].text)
                    .and_then(|m| m.get(&p.segments[1].text))
                    .copied()
                    .unwrap_or(0);
                Some(eq(scrut.clone(), Expr::Const(disc)))
            }
            // Wildcard and (for now) bit patterns match anything.
            _ => None,
        }
    }

    fn lower_combinational_else(&mut self, eb: &ast::ElseBranch, cond: Option<Expr>) {
        match eb {
            ast::ElseBranch::Block(b) => {
                for s in &b.stmts {
                    self.lower_stmt(s, cond.clone());
                }
            }
            ast::ElseBranch::If(inner) => {
                self.lower_stmt(&ast::Stmt::If(inner.clone()), cond);
            }
        }
    }

    /// Lower the body of an event-controlled block into next-state updates,
    /// accumulating the priority condition through nested `if`/`else`.
    fn lower_event_block(&mut self, block: &ast::Block, cond: Option<Expr>, out: &mut Vec<NextUpdate>) {
        for s in &block.stmts {
            match s {
                ast::Stmt::Assign { target, value, .. } => {
                    if let Some(target) = self.target_signal(target) {
                        let expr = self.lower_expr(value);
                        out.push(NextUpdate { target, cond: cond.clone(), expr });
                    }
                }
                ast::Stmt::If(iff) => {
                    let c = self.lower_expr(&iff.cond);
                    self.lower_event_block(&iff.then, Some(and(cond.clone(), c.clone())), out);
                    if let Some(eb) = iff.else_.as_deref() {
                        let neg = Some(and(cond.clone(), not(c)));
                        self.lower_event_else(eb, neg, out);
                    }
                }
                ast::Stmt::Match(m) => {
                    let scrut = self.lower_expr(&m.scrutinee);
                    let mut remaining = cond.clone();
                    for arm in &m.arms {
                        let mc = self.arm_match_cond(&arm.pattern, &scrut);
                        let fire = match &mc {
                            Some(c) => Some(and(remaining.clone(), c.clone())),
                            None => remaining.clone(),
                        };
                        self.lower_event_block(&arm.body, fire, out);
                        remaining = match mc {
                            Some(c) => Some(and(remaining, not(c))),
                            None => Some(Expr::Const(0)),
                        };
                    }
                }
                _ => {}
            }
        }
    }

    fn lower_event_else(&mut self, eb: &ast::ElseBranch, cond: Option<Expr>, out: &mut Vec<NextUpdate>) {
        match eb {
            ast::ElseBranch::Block(b) => self.lower_event_block(b, cond, out),
            ast::ElseBranch::If(inner) => {
                let c = self.lower_expr(&inner.cond);
                self.lower_event_block(&inner.then, Some(and(cond.clone(), c.clone())), out);
                if let Some(eb) = inner.else_.as_deref() {
                    self.lower_event_else(eb, Some(and(cond, not(c))), out);
                }
            }
        }
    }

    /// The signal an assignment target refers to — a bare name or a struct-field
    /// path (`s.data`).
    fn target_signal(&self, target: &ast::Expr) -> Option<SignalId> {
        expr_path(target).and_then(|p| self.locals.get(&p).copied())
    }

    fn lower_expr(&self, e: &ast::Expr) -> Expr {
        match e {
            ast::Expr::Int { text, .. } => Expr::Const(parse_int(text).unwrap_or(0)),
            ast::Expr::Bool { value, .. } => Expr::Const(*value as u64),
            ast::Expr::LogicLit { ch, .. } => Expr::Logic(*ch),
            ast::Expr::Path(p) if p.segments.len() == 1 => self
                .locals
                .get(&p.segments[0].text)
                .map(|id| Expr::Current(*id))
                .unwrap_or(Expr::Unknown),
            // `Enum::Variant` lowers to its discriminant constant.
            ast::Expr::Path(p) if p.segments.len() >= 2 => self
                .enum_variants
                .get(&p.segments[0].text)
                .and_then(|m| m.get(&p.segments[1].text))
                .map(|&d| Expr::Const(d))
                .unwrap_or(Expr::Unknown),
            // A bit slice `base[hi..lo]` (constant bounds).
            ast::Expr::Index { base, index, .. }
                if matches!(index.as_ref(), ast::Expr::Range { .. }) =>
            {
                if let ast::Expr::Range { lo, hi, .. } = index.as_ref() {
                    match (int_lit(lo), int_lit(hi)) {
                        (Some(a), Some(b)) => Expr::Slice {
                            base: Box::new(self.lower_expr(base)),
                            hi: a.max(b),
                            lo: a.min(b),
                        },
                        _ => Expr::Unknown, // dynamic slice bounds: unsupported
                    }
                } else {
                    Expr::Unknown
                }
            }
            // A struct-field (`s.data`) or constant array-element (`a[2]`) access
            // resolves to its flattened signal.
            ast::Expr::Field { .. } | ast::Expr::Index { .. } => expr_path(e)
                .and_then(|p| self.locals.get(&p).copied())
                .map(Expr::Current)
                .unwrap_or(Expr::Unknown),
            ast::Expr::SysAttr { base, attr, .. } => self.lower_sysattr(base, &attr.text),
            ast::Expr::Unary { op, rhs, .. } => {
                Expr::Unary { op: lower_unop(*op), rhs: Box::new(self.lower_expr(rhs)) }
            }
            ast::Expr::Binary { op, lhs, rhs, .. } => Expr::Binary {
                op: lower_binop(*op),
                lhs: Box::new(self.lower_expr(lhs)),
                rhs: Box::new(self.lower_expr(rhs)),
            },
            _ => Expr::Unknown,
        }
    }

    /// Lower a system attribute. `clk::rising`/`falling`/`edge` expand into
    /// `Event`/`Old`/`Current` so the scheduler needs no special knowledge.
    fn lower_sysattr(&self, base: &ast::Expr, attr: &str) -> Expr {
        let Some(sig) = self.base_signal(base) else { return Expr::Unknown };
        match attr {
            "event" | "edge" => Expr::Event(sig),
            "old" => Expr::Old(sig),
            // rising: Event(clk) && Old(clk) == '0' && Current(clk) == '1'
            "rising" => and3(
                Expr::Event(sig),
                eq(Expr::Old(sig), Expr::Logic('0')),
                eq(Expr::Current(sig), Expr::Logic('1')),
            ),
            // falling: Event(clk) && Old(clk) == '1' && Current(clk) == '0'
            "falling" => and3(
                Expr::Event(sig),
                eq(Expr::Old(sig), Expr::Logic('1')),
                eq(Expr::Current(sig), Expr::Logic('0')),
            ),
            _ => Expr::Unknown,
        }
    }

    fn base_signal(&self, base: &ast::Expr) -> Option<SignalId> {
        if let ast::Expr::Path(p) = base {
            if p.segments.len() == 1 {
                return self.locals.get(&p.segments[0].text).copied();
            }
        }
        None
    }
}

impl Design {
    /// Render normalized IR (backs `siox ir`).
    pub fn to_ir_string(&self) -> String {
        let mut out = String::new();
        for s in &self.signals {
            let w = if s.width == 0 { "?".to_string() } else { s.width.to_string() };
            out.push_str(&format!("signal {} : {w}\n", s.path));
        }
        for d in &self.drivers {
            let cond = match &d.cond {
                Some(c) => format!("  when {}", render(c, self)),
                None => String::new(),
            };
            out.push_str(&format!(
                "driver {} = {}{cond}\n",
                self.signals[d.target.0 as usize].path,
                render(&d.expr, self)
            ));
        }
        for eb in &self.event_blocks {
            out.push_str(&format!("event ({}):\n", render(&eb.condition, self)));
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
        out
    }
}

// --- expression builders ----------------------------------------------------

fn not(e: Expr) -> Expr {
    Expr::Unary { op: UnOp::Not, rhs: Box::new(e) }
}

fn eq(lhs: Expr, rhs: Expr) -> Expr {
    Expr::Binary { op: BinOp::Eq, lhs: Box::new(lhs), rhs: Box::new(rhs) }
}

fn and3(a: Expr, b: Expr, c: Expr) -> Expr {
    Expr::Binary {
        op: BinOp::And,
        lhs: Box::new(Expr::Binary { op: BinOp::And, lhs: Box::new(a), rhs: Box::new(b) }),
        rhs: Box::new(c),
    }
}

/// `and` of an optional accumulated condition with a new one.
fn and(acc: Option<Expr>, c: Expr) -> Expr {
    match acc {
        Some(a) => Expr::Binary { op: BinOp::And, lhs: Box::new(a), rhs: Box::new(c) },
        None => c,
    }
}

// --- rendering --------------------------------------------------------------

fn render(e: &Expr, d: &Design) -> String {
    match e {
        Expr::Const(v) => v.to_string(),
        Expr::Logic(c) => format!("'{c}'"),
        Expr::Current(id) => d.signals[id.0 as usize].path.clone(),
        Expr::Old(id) => format!("Old({})", d.signals[id.0 as usize].path),
        Expr::Event(id) => format!("Event({})", d.signals[id.0 as usize].path),
        Expr::Unary { op, rhs } => format!("{}{}", un_sym(*op), paren(rhs, d)),
        Expr::Binary { op, lhs, rhs } => {
            format!("{} {} {}", paren(lhs, d), bin_sym(*op), paren(rhs, d))
        }
        Expr::Slice { base, hi, lo } => format!("{}[{hi}..{lo}]", paren(base, d)),
        Expr::Unknown => "?".to_string(),
    }
}

fn paren(e: &Expr, d: &Design) -> String {
    match e {
        Expr::Binary { .. } | Expr::Unary { .. } => format!("({})", render(e, d)),
        _ => render(e, d),
    }
}

fn un_sym(op: UnOp) -> &'static str {
    match op {
        UnOp::Not => "not ",
        UnOp::Neg => "-",
    }
}

fn bin_sym(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::And => "and",
        BinOp::Nand => "nand",
        BinOp::Or => "or",
        BinOp::Nor => "nor",
        BinOp::Xor => "xor",
        BinOp::Xnor => "xnor",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
    }
}

// --- helpers ----------------------------------------------------------------

/// Whether an expression depends on a `::event`-family system attribute, which
/// makes an enclosing `if` an event-controlled block (spec 3.11).
fn expr_is_event(e: &ast::Expr) -> bool {
    match e {
        ast::Expr::SysAttr { base, attr, .. } => {
            matches!(attr.text.as_str(), "event" | "rising" | "falling" | "edge")
                || expr_is_event(base)
        }
        ast::Expr::Unary { rhs, .. } => expr_is_event(rhs),
        ast::Expr::Binary { lhs, rhs, .. } => expr_is_event(lhs) || expr_is_event(rhs),
        ast::Expr::Field { base, .. } | ast::Expr::Index { base, .. } => expr_is_event(base),
        _ => false,
    }
}

fn lower_unop(op: AstUnOp) -> UnOp {
    match op {
        AstUnOp::Not => UnOp::Not,
        AstUnOp::Neg => UnOp::Neg,
    }
}

fn lower_binop(op: AstBinOp) -> BinOp {
    match op {
        AstBinOp::Add => BinOp::Add,
        AstBinOp::Sub => BinOp::Sub,
        AstBinOp::Mul => BinOp::Mul,
        AstBinOp::Div => BinOp::Div,
        AstBinOp::And => BinOp::And,
        AstBinOp::Nand => BinOp::Nand,
        AstBinOp::Or => BinOp::Or,
        AstBinOp::Nor => BinOp::Nor,
        AstBinOp::Xor => BinOp::Xor,
        AstBinOp::Xnor => BinOp::Xnor,
        AstBinOp::Shl => BinOp::Shl,
        AstBinOp::Shr => BinOp::Shr,
        AstBinOp::Eq => BinOp::Eq,
        AstBinOp::Ne => BinOp::Ne,
        AstBinOp::Lt => BinOp::Lt,
        AstBinOp::Le => BinOp::Le,
        AstBinOp::Gt => BinOp::Gt,
        AstBinOp::Ge => BinOp::Ge,
    }
}

fn parse_int(text: &str) -> Option<u64> {
    let t = text.trim();
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).ok()
    } else if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        u64::from_str_radix(b, 2).ok()
    } else {
        t.parse().ok()
    }
}

/// Bit width from a type annotation, substituting parameters from `env` (so
/// `uint[W]` with `W=8` is width 8). `0` means parametric / not yet known.
fn type_width(t: &ast::Type, env: &HashMap<String, i64>) -> u32 {
    match t {
        ast::Type::Path(p) => match p.segments.last().map(|s| s.text.as_str()) {
            Some("Bit") | Some("Logic") | Some("Clock") | Some("Bool") => 1,
            _ => 0,
        },
        // For `uint[8]` the index is the width; for `Logic[31..0]` it is the span.
        ast::Type::Indexed { index, .. } => match index.as_ref() {
            ast::Expr::Range { lo, hi, .. } => match (eval_const(lo, env), eval_const(hi, env)) {
                (Some(a), Some(b)) => (a - b).unsigned_abs() as u32 + 1,
                _ => 0,
            },
            e => eval_const(e, env).map(|v| v.max(0) as u32).unwrap_or(0),
        },
        ast::Type::Generic { base, .. } | ast::Type::Mode { inner: base, .. } => {
            type_width(base, env)
        }
    }
}

/// Const-evaluate a width expression against a parameter environment.
fn eval_const(e: &ast::Expr, env: &HashMap<String, i64>) -> Option<i64> {
    match e {
        ast::Expr::Int { text, .. } => parse_int(text).map(|v| v as i64),
        ast::Expr::Path(p) if p.segments.len() == 1 => env.get(&p.segments[0].text).copied(),
        ast::Expr::Unary { op, rhs, .. } => {
            let v = eval_const(rhs, env)?;
            Some(match op {
                ast::UnOp::Neg => -v,
                ast::UnOp::Not => !v,
            })
        }
        ast::Expr::Binary { op, lhs, rhs, .. } => {
            let (a, b) = (eval_const(lhs, env)?, eval_const(rhs, env)?);
            Some(match op {
                ast::BinOp::Add => a + b,
                ast::BinOp::Sub => a - b,
                ast::BinOp::Mul => a * b,
                ast::BinOp::Div if b != 0 => a / b,
                ast::BinOp::Shl => a << b,
                ast::BinOp::Shr => a >> b,
                _ => return None,
            })
        }
        _ => None,
    }
}

/// Build `enum name -> variant name -> discriminant`. Explicit `= n` values are
/// honoured; unspecified variants continue from the previous discriminant + 1.
fn enum_discriminants(modules: &[Module]) -> HashMap<String, HashMap<String, u64>> {
    let mut out = HashMap::new();
    for m in modules {
        for item in &m.items {
            if let ast::Item::Enum(e) = item {
                let mut vars = HashMap::new();
                let mut next = 0u64;
                for v in &e.variants {
                    let disc = match &v.value {
                        Some(ast::Expr::Int { text, .. }) => parse_int(text).unwrap_or(next),
                        _ => next,
                    };
                    vars.insert(v.name.text.clone(), disc);
                    next = disc + 1;
                }
                out.insert(e.name.text.clone(), vars);
            }
        }
    }
    out
}

/// The dotted signal path of a name, struct-field, or constant-index access:
/// `s` -> `"s"`, `s.data` -> `"s.data"`, `a[2]` -> `"a[2]"`. A dynamic index or
/// anything else (calls, slices) yields `None`.
fn expr_path(e: &ast::Expr) -> Option<String> {
    match e {
        ast::Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
        ast::Expr::Field { base, field, .. } => {
            Some(format!("{}.{}", expr_path(base)?, field.text))
        }
        ast::Expr::Index { base, index, .. } => match index.as_ref() {
            ast::Expr::Int { text, .. } => Some(format!("{}[{}]", expr_path(base)?, parse_int(text)?)),
            _ => None,
        },
        _ => None,
    }
}

/// The `(element type, length)` if `ty` is an array — an `Indexed` type whose
/// base is *not* an integer (`Bit[4]`), as opposed to a vector (`uint[8]`).
fn array_of<'t>(ty: &'t ast::Type, env: &HashMap<String, i64>) -> Option<(&'t ast::Type, u32)> {
    if let ast::Type::Indexed { base, index, .. } = ty {
        if !is_int_type(base) {
            return Some((base, eval_const(index, env).unwrap_or(0).max(0) as u32));
        }
    }
    None
}

/// The value of an integer-literal expression, if `e` is one.
fn int_lit(e: &ast::Expr) -> Option<u32> {
    match e {
        ast::Expr::Int { text, .. } => parse_int(text).map(|v| v as u32),
        _ => None,
    }
}

fn is_int_type(ty: &ast::Type) -> bool {
    matches!(ty, ast::Type::Path(p)
        if matches!(p.segments.last().map(|s| s.text.as_str()), Some("uint" | "int" | "usize")))
}

fn has_attr(e: &ast::EntityDecl, name: &str) -> bool {
    e.attrs
        .iter()
        .any(|a| a.name.segments.last().map(|s| s.text.as_str()) == Some(name))
}

fn type_head_name(t: &ast::Type) -> Option<&str> {
    match t {
        ast::Type::Path(p) => p.segments.first().map(|s| s.text.as_str()),
        ast::Type::Generic { base, .. } | ast::Type::Indexed { base, .. } => type_head_name(base),
        ast::Type::Mode { inner, .. } => type_head_name(inner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use siox_diag::FileId;

    fn lower_src(src: &str) -> Design {
        let mut sink = DiagnosticSink::new();
        let module = siox_syntax::parse_module(FileId(0), src, &mut sink);
        assert_eq!(sink.error_count(), 0, "parse errors:\n{src}");
        let modules = std::slice::from_ref(&module);
        let resolved = siox_resolve::resolve(modules, &mut sink);
        let typed = siox_types::check(modules, &resolved, &mut sink);
        let hier = siox_elab::elaborate(modules, &typed, &mut sink);
        lower(modules, &hier, &mut sink)
    }

    const COUNTER: &str = "module m;\n\
        entity Counter<W: usize> {\n\
          in clk: Clock;\n\
          in rst: Logic;\n\
          in en: Bit;\n\
          out count: uint[W];\n\
        }\n\
        impl Counter<W: usize> {\n\
          let value: uint[W] = 0;\n\
          if clk::rising {\n\
            if rst == '1' {\n\
              value = 0;\n\
            } else if en {\n\
              value = value + 1;\n\
            }\n\
          }\n\
          count = value;\n\
        }\n\
        #[top]\n\
        entity H {}\n\
        impl H {\n\
          let clk: Logic = '0';\n\
          let rst: Logic = '1';\n\
          let en: Bit = '1';\n\
          let count: uint[8];\n\
          let dut = Counter<W = 8> { .clk, .rst, .en, .count };\n\
        }\n";

    #[test]
    fn lowers_signals_driver_and_event_block() {
        let d = lower_src(COUNTER);
        // Counter signals: clk, rst, en, count, value. The instance's `W = 8`
        // makes the parametric `uint[W]` widths concrete.
        let count = d.signals.iter().find(|s| s.path == "Counter.count").unwrap();
        assert_eq!(count.width, 8);
        assert!(d.signals.iter().any(|s| s.path == "Counter.value"));
        // One combinational driver: count = value.
        assert_eq!(d.drivers.len(), 1);
        // One event block (clk::rising) with two next-state updates.
        assert_eq!(d.event_blocks.len(), 1);
        assert_eq!(d.event_blocks[0].updates.len(), 2);
    }

    #[test]
    fn rising_lowers_to_event_old_current() {
        let d = lower_src(COUNTER);
        let rendered = d.to_ir_string();
        // clk::rising expands into the explicit Event/Old/Current form.
        assert!(rendered.contains("Event(Counter.clk)"));
        assert!(rendered.contains("Old(Counter.clk) == '0'"));
        assert!(rendered.contains("Counter.clk == '1'"));
        // The combinational driver and the next-state updates are present.
        assert!(rendered.contains("driver Counter.count = Counter.value"));
        assert!(rendered.contains("next Counter.value = 0"));
    }

    #[test]
    fn priority_conditions_accumulate() {
        let d = lower_src(COUNTER);
        let u = &d.event_blocks[0].updates;
        // First update guarded by rst == '1'.
        assert!(matches!(&u[0].cond, Some(Expr::Binary { op: BinOp::Eq, .. })));
        // Second guarded by the negation AND en.
        assert!(matches!(&u[1].cond, Some(Expr::Binary { op: BinOp::And, .. })));
    }
}
