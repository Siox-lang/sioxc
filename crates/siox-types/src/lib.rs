//! Type system and kind checking for siox Phase 1 (spec Stage 4).
//!
//! Checks primitive digital types (`Bit`, `Logic`, `Bool`), integer widths
//! (`uint[N]`, `int[N]`), structs, enums, arrays/vectors, entity types,
//! directional views and bus modes, function/method signatures, trait bounds,
//! attribute value typing, and pattern typing.
//!
//! Key Phase 1 rules to enforce:
//! - system attributes `::event`/`::old` exist on every digital value
//!   (spec 3.9), and range attributes `::width/::range/::high/::low/::left/
//!   ::right/::direction` on range-like values (spec 3.23)
//! - `::ddt` is rejected as Phase-2 analogue syntax (spec Stage 4)
//! - no implicit broad conversions (spec 3.17): `uint[8]` !-> `uint[16]`
//! - cannot write to `in` ports inside an entity (spec 3.18 / code E-P004)
//! - `Logic` is not a bare condition without comparison (spec 3.16)

use std::collections::{HashMap, HashSet};

use siox_diag::{codes, Diagnostic, DiagnosticSink, Span};
use siox_resolve::{DefKind, Resolved};
use siox_syntax::ast::*;
use siox_syntax::Module;

/// A checked, interned type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ty {
    Bit,
    Logic,
    Bool,
    /// `uint[N]` / `int[N]`. Width `0` means "not yet known" (parametric, e.g.
    /// `uint[W]`); the concrete width is resolved during elaboration.
    UInt(u32),
    Int(u32),
    /// Named struct / enum / entity, keyed by its definition.
    Named(siox_resolve::DefId),
    /// `T[range]` array/vector of a digital element type.
    Array { elem: Box<Ty>, len: u32 },
    /// Placeholder for an as-yet-unresolved/error type.
    Error,
}

impl Ty {
    /// Whether `::event` / `::old` apply (spec 3.9). True for all digital and
    /// discrete values, structs of digital fields, arrays, and enums.
    pub fn is_digital(&self) -> bool {
        // TODO(stage-4): recurse into Named structs to confirm all-digital.
        !matches!(self, Ty::Error)
    }
}

/// Outcome of type checking: a type for every expression/signal, ready for the
/// elaborator and IR lowering.
#[derive(Default)]
pub struct Typed {
    // TODO(stage-4): expr -> Ty map, signal/port types, method resolution.
}

/// Type-check resolved modules.
///
/// Incremental Stage-4 checker. It builds a light type-inference core (resolve
/// type annotations to [`Ty`], a per-impl symbol table, and `type_of` for
/// expressions) and enforces the digital rules that do not need elaboration:
/// - **Phase-2 guard** (spec Stage 4): `::ddt` -> [`codes::PHASE2_SYNTAX`].
/// - **Write to input port** (spec 3.18): bare `in` port on an assignment LHS
///   -> [`codes::WRITE_TO_INPUT_PORT`].
/// - **`Logic` as a bare condition** (spec 3.16): a condition of type `Logic`
///   that is not an explicit comparison -> [`codes::TYPE_MISMATCH`].
/// - **Attribute target** (spec 3.5): an attribute applied to a target its
///   declaration does not allow -> [`codes::INVALID_ATTR_TARGET`].
///
/// Deferred to elaboration, where the needed information exists: width-level
/// conversions (`uint[8]` !-> `uint[16]`) and method-call resolution.
pub fn check(modules: &[Module], resolved: &Resolved, sink: &mut DiagnosticSink) -> Typed {
    let mut checker = Checker::new(sink, resolved);
    checker.collect(modules);
    for m in modules {
        for item in &m.items {
            checker.check_item(item);
        }
    }
    Typed::default()
}

/// Analogue (Phase-2) system attributes that must error rather than be silently
/// accepted in Phase 1 (spec Stage 4). The full analogue set is a Phase-2
/// concern; `::ddt` is kept here only as the guard the spec calls out.
const PHASE2_ATTRS: &[&str] = &["ddt"];

/// A port as seen by the checker: its name, resolved type, and direction.
struct PortInfo {
    name: String,
    ty: Ty,
    dir: Option<Direction>,
}

/// The value type an attribute declaration expects (spec 3.5).
#[derive(Clone, Copy, PartialEq, Eq)]
enum AttrValueTy {
    Bool,
    Str,
    Other,
}

struct Checker<'a> {
    sink: &'a mut DiagnosticSink,
    resolved: &'a Resolved,
    /// Entity name -> its ports.
    entities: HashMap<String, Vec<PortInfo>>,
    /// Attribute name -> the target keywords it may be applied to.
    attr_targets: HashMap<String, Vec<String>>,
    /// Attribute name -> the value type it expects.
    attr_value_kinds: HashMap<String, AttrValueTy>,
}

impl<'a> Checker<'a> {
    fn new(sink: &'a mut DiagnosticSink, resolved: &'a Resolved) -> Self {
        // Seed the std::attrs targets so the standard attributes validate while
        // `std/` is still empty (mirrors the builtins seeded in siox-resolve).
        let mut attr_targets = HashMap::new();
        for (name, targets) in [
            ("top", &["entity"][..]),
            ("test", &["entity"]),
            ("keep", &["let", "port"]),
            ("library", &["entity"]),
            ("name", &["entity"]),
        ] {
            attr_targets.insert(name.to_string(), targets.iter().map(|s| s.to_string()).collect());
        }
        let mut attr_value_kinds = HashMap::new();
        for (name, ty) in [
            ("top", AttrValueTy::Bool),
            ("test", AttrValueTy::Bool),
            ("keep", AttrValueTy::Bool),
            ("library", AttrValueTy::Str),
            ("name", AttrValueTy::Str),
        ] {
            attr_value_kinds.insert(name.to_string(), ty);
        }
        Checker { sink, resolved, entities: HashMap::new(), attr_targets, attr_value_kinds }
    }

    /// First pass: record entity port types and declared attribute targets.
    fn collect(&mut self, modules: &[Module]) {
        for m in modules {
            for item in &m.items {
                match item {
                    Item::Entity(e) => {
                        let ports = e
                            .ports
                            .iter()
                            .map(|p| PortInfo {
                                name: p.name.text.clone(),
                                ty: self.ast_ty(&p.ty),
                                dir: p.dir,
                            })
                            .collect();
                        self.entities.insert(e.name.text.clone(), ports);
                    }
                    Item::AttrDecl(a) => {
                        let targets = a.targets.iter().map(|t| t.text.clone()).collect();
                        self.attr_targets.insert(a.name.text.clone(), targets);
                        let kind = match type_head_name(&a.ty) {
                            Some("Bool") => AttrValueTy::Bool,
                            Some("string") | Some("str") => AttrValueTy::Str,
                            _ => AttrValueTy::Other,
                        };
                        self.attr_value_kinds.insert(a.name.text.clone(), kind);
                    }
                    _ => {}
                }
            }
        }
    }

    fn check_item(&mut self, item: &Item) {
        match item {
            Item::Const(c) => self.check_expr(&c.value),
            Item::Enum(e) => {
                for v in &e.variants {
                    if let Some(val) = &v.value {
                        self.check_expr(val);
                    }
                }
            }
            Item::Entity(e) => {
                for a in &e.attrs {
                    self.check_attr_target(a);
                    self.check_attr_value(a);
                    if let Some(v) = &a.value {
                        self.check_expr(v);
                    }
                }
            }
            Item::Impl(im) => self.check_impl(im),
            Item::Trait(t) => {
                for f in &t.items {
                    if let Some(b) = &f.body {
                        self.check_block(b);
                    }
                }
            }
            Item::Using(_) | Item::Struct(_) | Item::AttrDecl(_) => {}
        }
    }

    /// Spec 3.5: an attribute may only be applied to a target its declaration
    /// allows. Attributes currently attach to entities only, so the target is
    /// always `entity`. Unknown attribute names are reported by name resolution.
    fn check_attr_target(&mut self, a: &Attr) {
        let name = a.name.segments.last().map(|s| s.text.as_str()).unwrap_or("");
        let verdict = self
            .attr_targets
            .get(name)
            .map(|targets| (targets.iter().any(|t| t == "entity"), targets.join(", ")));
        if let Some((false, allowed)) = verdict {
            self.error(
                codes::INVALID_ATTR_TARGET,
                a.name.span,
                format!("attribute `{name}` cannot be applied to an entity (allowed: {allowed})"),
            );
        }
    }

    fn check_impl(&mut self, im: &ImplDecl) {
        let (in_ports, sym) = self.impl_env(im);
        for item in &im.items {
            match item {
                ImplItem::Const(c) => self.check_expr(&c.value),
                ImplItem::Let(l) => {
                    if let Some(v) = &l.value {
                        self.check_init(l.ty.as_ref(), v, &sym);
                        self.check_expr(v);
                    }
                }
                ImplItem::Fn(f) => {
                    if let Some(b) = &f.body {
                        self.check_block(b);
                    }
                }
                ImplItem::ModeField { .. } => {}
                ImplItem::Stmt(s) => self.check_stmt(s, &in_ports, &sym),
            }
        }
    }

    /// Build the value environment for an impl body: the `in` ports (for the
    /// write check) and a name -> type table (ports + impl-level lets/consts).
    fn impl_env(&self, im: &ImplDecl) -> (HashSet<String>, HashMap<String, Ty>) {
        let mut in_ports = HashSet::new();
        let mut sym = HashMap::new();
        if im.trait_.is_none() {
            if let Some(ports) = type_head_name(&im.target).and_then(|n| self.entities.get(n)) {
                for p in ports {
                    sym.insert(p.name.clone(), p.ty.clone());
                    if p.dir == Some(Direction::In) {
                        in_ports.insert(p.name.clone());
                    }
                }
            }
        }
        for it in &im.items {
            match it {
                ImplItem::Let(l) => {
                    let ty = l.ty.as_ref().map(|t| self.ast_ty(t)).unwrap_or(Ty::Error);
                    sym.insert(l.name.text.clone(), ty);
                }
                ImplItem::Const(c) => {
                    sym.insert(c.name.text.clone(), self.ast_ty(&c.ty));
                }
                _ => {}
            }
        }
        (in_ports, sym)
    }

    fn check_block(&mut self, b: &Block) {
        let (in_ports, sym) = (HashSet::new(), HashMap::new());
        for s in &b.stmts {
            self.check_stmt(s, &in_ports, &sym);
        }
    }

    fn check_stmt(&mut self, s: &Stmt, in_ports: &HashSet<String>, sym: &HashMap<String, Ty>) {
        match s {
            Stmt::Let(l) => {
                if let Some(v) = &l.value {
                    self.check_init(l.ty.as_ref(), v, sym);
                    self.check_expr(v);
                }
            }
            Stmt::Assign { target, value, .. } => {
                self.check_write_target(target, in_ports);
                self.check_assignment(target, value, sym);
                self.check_expr(target);
                self.check_expr(value);
            }
            Stmt::If(i) => self.check_if(i, in_ports, sym),
            Stmt::Match(m) => {
                self.check_expr(&m.scrutinee);
                for arm in &m.arms {
                    for s in &arm.body.stmts {
                        self.check_stmt(s, in_ports, sym);
                    }
                }
            }
            Stmt::For { range, body, .. } => {
                self.check_expr(range);
                for s in &body.stmts {
                    self.check_stmt(s, in_ports, sym);
                }
            }
            Stmt::Expr(e) => self.check_expr(e),
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    self.check_expr(v);
                }
            }
        }
    }

    fn check_if(&mut self, i: &IfStmt, in_ports: &HashSet<String>, sym: &HashMap<String, Ty>) {
        self.check_condition(&i.cond, sym);
        self.check_expr(&i.cond);
        for s in &i.then.stmts {
            self.check_stmt(s, in_ports, sym);
        }
        match i.else_.as_deref() {
            Some(ElseBranch::Block(b)) => {
                for s in &b.stmts {
                    self.check_stmt(s, in_ports, sym);
                }
            }
            Some(ElseBranch::If(inner)) => self.check_if(inner, in_ports, sym),
            None => {}
        }
    }

    /// Spec 3.16: a `Logic` value cannot be a condition on its own; it must be
    /// compared explicitly (e.g. `== '1'`). `Bit` and `Bool` are fine.
    fn check_condition(&mut self, cond: &Expr, sym: &HashMap<String, Ty>) {
        if self.type_of(cond, sym) == Ty::Logic {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(cond),
                "`Logic` cannot be used directly as a condition; compare it explicitly, e.g. `== '1'`"
                    .to_string(),
            );
        }
    }

    /// Spec 3.18: flag `port = ...` where `port` is a bare `in` port. Field /
    /// index writes (`bus.ready = ...`) are left for fuller direction analysis.
    fn check_write_target(&mut self, target: &Expr, in_ports: &HashSet<String>) {
        if let Expr::Path(p) = target {
            if p.segments.len() == 1 && in_ports.contains(&p.segments[0].text) {
                self.error(
                    codes::WRITE_TO_INPUT_PORT,
                    p.span,
                    format!("cannot assign to input port `{}`", p.segments[0].text),
                );
            }
        }
    }

    /// Spec 3.5: an attribute's value must match the type its declaration gives.
    fn check_attr_value(&mut self, a: &Attr) {
        let Some(value) = &a.value else { return };
        let name = a.name.segments.last().map(|s| s.text.as_str()).unwrap_or("");
        let expected = self.attr_value_kinds.get(name).copied();
        let ok = match expected {
            Some(AttrValueTy::Bool) => matches!(value, Expr::Bool { .. }),
            Some(AttrValueTy::Str) => matches!(value, Expr::StrLit { .. }),
            // Unknown attribute (reported by resolve) or an `Other`-typed one.
            _ => true,
        };
        if !ok {
            let want = match expected {
                Some(AttrValueTy::Bool) => "a Bool",
                Some(AttrValueTy::Str) => "a string",
                _ => "a different",
            };
            self.error(
                codes::INVALID_ATTR_VALUE_TYPE,
                expr_span(value),
                format!("attribute `{name}` expects {want} value"),
            );
        }
    }

    /// Spec 3.17: a `let name: T = e` initializer must be assignable to `T`.
    fn check_init(&mut self, decl_ty: Option<&Type>, value: &Expr, sym: &HashMap<String, Ty>) {
        let Some(t) = decl_ty else { return };
        let lhs = self.ast_ty(t);
        if !matches!(lhs, Ty::Error) && !self.assignable(&lhs, value, sym) {
            let rhs = self.type_of(value, sym);
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(value),
                format!(
                    "cannot initialize {} with {} without an explicit conversion",
                    ty_name(&lhs),
                    ty_name(&rhs)
                ),
            );
        }
    }

    /// Spec 3.17: the right-hand side of `target = value` must be assignable to
    /// the target's type. Only fires when the target type is known.
    fn check_assignment(&mut self, target: &Expr, value: &Expr, sym: &HashMap<String, Ty>) {
        let lhs = self.type_of(target, sym);
        if !matches!(lhs, Ty::Error) && !self.assignable(&lhs, value, sym) {
            let rhs = self.type_of(value, sym);
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(value),
                format!(
                    "cannot assign {} to {} without an explicit conversion",
                    ty_name(&rhs),
                    ty_name(&lhs)
                ),
            );
        }
    }

    /// Whether `value` may be assigned to a target of type `lhs` without an
    /// explicit conversion. Integer and logic *literals* are polymorphic; an
    /// `Error` type on either side suppresses the check.
    fn assignable(&self, lhs: &Ty, value: &Expr, sym: &HashMap<String, Ty>) -> bool {
        match value {
            Expr::Int { .. } => matches!(lhs, Ty::UInt(_) | Ty::Int(_) | Ty::Error),
            Expr::LogicLit { .. } => matches!(lhs, Ty::Bit | Ty::Logic | Ty::Error),
            _ => compatible(lhs, &self.type_of(value, sym)),
        }
    }

    /// Walk an expression for the Phase-2 `::ddt` guard (the only expression-
    /// local check so far).
    fn check_expr(&mut self, e: &Expr) {
        match e {
            Expr::SysAttr { base, attr, span } => {
                if PHASE2_ATTRS.contains(&attr.text.as_str()) {
                    self.error(
                        codes::PHASE2_SYNTAX,
                        *span,
                        format!("`::{}` is Phase-2 analogue syntax, not available in Phase 1", attr.text),
                    );
                }
                self.check_expr(base);
            }
            Expr::Field { base, .. } => self.check_expr(base),
            Expr::Index { base, index, .. } => {
                self.check_expr(base);
                self.check_expr(index);
            }
            Expr::Range { lo, hi, .. } => {
                self.check_expr(lo);
                self.check_expr(hi);
            }
            Expr::Unary { rhs, .. } => self.check_expr(rhs),
            Expr::Binary { lhs, rhs, .. } => {
                self.check_expr(lhs);
                self.check_expr(rhs);
            }
            Expr::Call { callee, args, .. } => {
                self.check_expr(callee);
                for a in args {
                    self.check_expr(a);
                }
            }
            Expr::Construct { args, .. } => {
                for c in args {
                    if let Some(v) = &c.value {
                        self.check_expr(v);
                    }
                }
            }
            Expr::Int { .. }
            | Expr::LogicLit { .. }
            | Expr::StrLit { .. }
            | Expr::Bool { .. }
            | Expr::Path(_) => {}
        }
    }

    // --- type inference core ------------------------------------------------

    /// Best-effort type of an expression given the in-scope value table. Unknown
    /// or unsupported cases yield [`Ty::Error`], which suppresses dependent
    /// checks rather than producing a false positive.
    fn type_of(&self, e: &Expr, sym: &HashMap<String, Ty>) -> Ty {
        match e {
            Expr::Int { .. } => Ty::UInt(0),
            Expr::LogicLit { .. } => Ty::Logic,
            Expr::Bool { .. } => Ty::Bool,
            Expr::StrLit { .. } => Ty::Error,
            Expr::Path(p) => {
                if p.segments.len() == 1 {
                    sym.get(&p.segments[0].text).cloned().unwrap_or(Ty::Error)
                } else {
                    // `Enum::Variant` has the enum's type, not the variant's.
                    match self.resolved.resolved(p.span).and_then(|id| self.resolved.def(id)) {
                        Some(d) if d.kind == DefKind::EnumVariant => {
                            d.parent.map(Ty::Named).unwrap_or(Ty::Error)
                        }
                        _ => self.resolved.resolved(p.span).map(Ty::Named).unwrap_or(Ty::Error),
                    }
                }
            }
            Expr::SysAttr { base, attr, .. } => match attr.text.as_str() {
                "event" | "rising" | "falling" | "edge" => Ty::Bool,
                "old" => self.type_of(base, sym),
                "width" | "high" | "low" | "left" | "right" => Ty::UInt(0),
                _ => Ty::Error,
            },
            Expr::Binary { op, lhs, .. } => {
                if is_comparison(*op) {
                    Ty::Bool
                } else {
                    self.type_of(lhs, sym)
                }
            }
            Expr::Unary { rhs, .. } => self.type_of(rhs, sym),
            Expr::Construct { ty, .. } => self.ast_ty(ty),
            Expr::Field { .. } | Expr::Index { .. } | Expr::Call { .. } | Expr::Range { .. } => {
                Ty::Error
            }
        }
    }

    /// Resolve a type annotation to a [`Ty`]. Parametric widths (`uint[W]`)
    /// become `UInt(0)` until elaboration fills them in.
    fn ast_ty(&self, t: &Type) -> Ty {
        match t {
            Type::Path(p) => self.path_ty(p),
            Type::Indexed { base, index, .. } => {
                let width = width_of(index);
                match self.ast_ty(base) {
                    Ty::UInt(_) => Ty::UInt(width),
                    Ty::Int(_) => Ty::Int(width),
                    other => Ty::Array { elem: Box::new(other), len: width },
                }
            }
            Type::Generic { base, .. } => self.ast_ty(base),
            Type::Mode { inner, .. } => self.ast_ty(inner),
        }
    }

    fn path_ty(&self, p: &Path) -> Ty {
        if p.segments.len() == 1 {
            match p.segments[0].text.as_str() {
                "Bit" => Ty::Bit,
                "Logic" => Ty::Logic,
                "Bool" => Ty::Bool,
                // A clock is a single-bit signal; treat it as Bit for checking
                // (clock-as-condition correctness is a separate, later concern).
                "Clock" => Ty::Bit,
                "uint" | "usize" => Ty::UInt(0),
                "int" => Ty::Int(0),
                _ => self.resolved.resolved(p.span).map(Ty::Named).unwrap_or(Ty::Error),
            }
        } else {
            self.resolved.resolved(p.span).map(Ty::Named).unwrap_or(Ty::Error)
        }
    }

    fn error(&mut self, code: &'static str, span: Span, msg: String) {
        self.sink.emit(Diagnostic::error(msg).with_code(code).at(span));
    }
}

/// The base name of a type (`Counter<W>` -> `Counter`, `out S::Source` -> `S`).
fn type_head_name(ty: &Type) -> Option<&str> {
    match ty {
        Type::Path(p) => p.segments.first().map(|s| s.text.as_str()),
        Type::Generic { base, .. } | Type::Indexed { base, .. } => type_head_name(base),
        Type::Mode { inner, .. } => type_head_name(inner),
    }
}

/// Width of a bracketed type index when it is a literal (`uint[8]` -> 8);
/// otherwise `0`, meaning "parametric / not yet known".
fn width_of(index: &Expr) -> u32 {
    match index {
        Expr::Int { text, .. } => text.parse().unwrap_or(0),
        _ => 0,
    }
}

fn is_comparison(op: BinOp) -> bool {
    matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
}

/// Whether a value of type `rhs` may be assigned to `lhs` with no conversion.
/// A width of `0` is "not yet known" (parametric) and assumed compatible — the
/// concrete width check happens after elaboration.
fn compatible(lhs: &Ty, rhs: &Ty) -> bool {
    use Ty::*;
    if matches!(lhs, Error) || matches!(rhs, Error) {
        return true;
    }
    match (lhs, rhs) {
        (Bit, Bit) | (Logic, Logic) | (Bool, Bool) => true,
        (UInt(a), UInt(b)) | (Int(a), Int(b)) => *a == 0 || *b == 0 || a == b,
        (Named(a), Named(b)) => a == b,
        _ => false,
    }
}

fn ty_name(t: &Ty) -> String {
    match t {
        Ty::Bit => "Bit".to_string(),
        Ty::Logic => "Logic".to_string(),
        Ty::Bool => "Bool".to_string(),
        Ty::UInt(0) => "uint".to_string(),
        Ty::UInt(w) => format!("uint[{w}]"),
        Ty::Int(0) => "int".to_string(),
        Ty::Int(w) => format!("int[{w}]"),
        Ty::Named(_) => "a named type".to_string(),
        Ty::Array { .. } => "an array".to_string(),
        Ty::Error => "<unknown>".to_string(),
    }
}

fn expr_span(e: &Expr) -> Span {
    match e {
        Expr::Int { span, .. }
        | Expr::LogicLit { span, .. }
        | Expr::StrLit { span, .. }
        | Expr::Bool { span, .. }
        | Expr::Field { span, .. }
        | Expr::SysAttr { span, .. }
        | Expr::Index { span, .. }
        | Expr::Range { span, .. }
        | Expr::Unary { span, .. }
        | Expr::Binary { span, .. }
        | Expr::Call { span, .. }
        | Expr::Construct { span, .. } => *span,
        Expr::Path(p) => p.span,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use siox_diag::FileId;

    fn check_src(src: &str) -> usize {
        let mut sink = DiagnosticSink::new();
        let module = siox_syntax::parse_module(FileId(0), src, &mut sink);
        assert_eq!(sink.error_count(), 0, "source failed to parse:\n{src}");
        let resolved = siox_resolve::resolve(std::slice::from_ref(&module), &mut sink);
        let parse_resolve_errors = sink.error_count();
        check(std::slice::from_ref(&module), &resolved, &mut sink);
        sink.error_count() - parse_resolve_errors
    }

    #[test]
    fn rejects_phase2_ddt() {
        let errors = check_src("module m;\nentity E { out y: Bit; }\nimpl E {\n  y = x::ddt;\n}\n");
        assert_eq!(errors, 1);
    }

    #[test]
    fn accepts_digital_sysattrs() {
        let errors = check_src(
            "module m;\nentity E { in clk: Clock; out q: Bit; }\nimpl E {\n  if clk::rising {\n    q = clk::old;\n  }\n}\n",
        );
        assert_eq!(errors, 0);
    }

    #[test]
    fn rejects_write_to_input_port() {
        let errors = check_src(
            "module m;\nentity E { in en: Bit; out y: Bit; }\nimpl E {\n  en = '1';\n  y = en;\n}\n",
        );
        assert_eq!(errors, 1);
    }

    #[test]
    fn writing_output_is_fine() {
        let errors = check_src(
            "module m;\nentity E { in en: Bit; out y: Bit; }\nimpl E {\n  y = en;\n}\n",
        );
        assert_eq!(errors, 0);
    }

    #[test]
    fn bare_logic_condition_is_rejected() {
        let errors = check_src(
            "module m;\nentity E { in rst: Logic; out y: Bit; }\nimpl E {\n  if rst {\n    y = '0';\n  }\n}\n",
        );
        assert_eq!(errors, 1);
    }

    #[test]
    fn compared_logic_and_bit_conditions_are_fine() {
        // `rst == '1'` is a comparison (-> Bool); `en` is a Bit. Both valid.
        let errors = check_src(
            "module m;\nentity E { in rst: Logic; in en: Bit; out y: Bit; }\nimpl E {\n  if rst == '1' {\n    y = '0';\n  }\n  if en {\n    y = '1';\n  }\n}\n",
        );
        assert_eq!(errors, 0);
    }

    #[test]
    fn attribute_on_wrong_target_is_rejected() {
        // `keep` is declared for `let, port`, not `entity`.
        let errors = check_src("module m;\n#[keep]\nentity E { out y: Bit; }\n");
        assert_eq!(errors, 1);
    }

    #[test]
    fn attribute_on_right_target_is_fine() {
        let errors = check_src("module m;\n#[top]\nentity E { out y: Bit; }\n");
        assert_eq!(errors, 0);
    }

    #[test]
    fn assigning_bool_to_a_bit_port_is_rejected() {
        let errors = check_src(
            "module m;\nentity E { in en: Bit; out y: Bit; }\nimpl E {\n  y = en == en;\n}\n",
        );
        // `en == en` is Bool; `y` is Bit.
        assert_eq!(errors, 1);
    }

    #[test]
    fn integer_and_logic_literals_are_polymorphic() {
        // int literal -> any uint; '1' -> Bit or Logic. No conversions needed.
        let errors = check_src(
            "module m;\nentity E { out count: uint[8]; out q: Bit; out clk: Logic; }\nimpl E {\n  let value: uint[8] = 0;\n  count = value;\n  q = '1';\n  clk = '0';\n}\n",
        );
        assert_eq!(errors, 0);
    }

    #[test]
    fn enum_assignment_uses_the_enum_type() {
        let errors = check_src(
            "module m;\nenum State { Idle, Run }\nentity E { out s: State; }\nimpl E {\n  s = State::Idle;\n}\n",
        );
        assert_eq!(errors, 0);
    }

    #[test]
    fn bad_initializer_type_is_rejected() {
        let errors = check_src(
            "module m;\nentity E { out y: Bit; }\nimpl E {\n  let flag: Bool = 5;\n  y = '0';\n}\n",
        );
        assert_eq!(errors, 1);
    }

    #[test]
    fn attribute_value_type_is_checked() {
        // `name` expects a string; giving it an int is an error.
        let bad = check_src("module m;\n#[name = 5]\nentity E { out y: Bit; }\n");
        assert_eq!(bad, 1);
        let good = check_src("module m;\n#[name = \"dut\"]\nentity E { out y: Bit; }\n");
        assert_eq!(good, 0);
    }
}
