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
use siox_resolve::Resolved;
use siox_syntax::ast::*;
use siox_syntax::Module;

/// A checked, interned type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ty {
    Bit,
    Logic,
    Bool,
    /// `uint[N]` / `int[N]` with a resolved width.
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
/// This is an incremental Stage-4 starter. It already enforces two concrete
/// rules and is structured so the remaining checks (width conversions, method
/// resolution, `Logic`-as-condition, attribute targeting) slot in beside them:
/// - **Phase-2 rejection** (spec Stage 4): `x::ddt` and friends are analogue
///   syntax and produce [`codes::PHASE2_SYNTAX`].
/// - **Write to input port** (spec 3.18): assigning a bare `in` port inside its
///   entity's impl produces [`codes::WRITE_TO_INPUT_PORT`].
pub fn check(modules: &[Module], resolved: &Resolved, sink: &mut DiagnosticSink) -> Typed {
    let mut checker = Checker { sink, _resolved: resolved, entity_in_ports: HashMap::new() };
    checker.collect_entities(modules);
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

struct Checker<'a> {
    sink: &'a mut DiagnosticSink,
    _resolved: &'a Resolved,
    /// Entity name -> set of its `in` port names.
    entity_in_ports: HashMap<String, HashSet<String>>,
}

impl<'a> Checker<'a> {
    fn collect_entities(&mut self, modules: &[Module]) {
        for m in modules {
            for item in &m.items {
                if let Item::Entity(e) = item {
                    let ins = e
                        .ports
                        .iter()
                        .filter(|p| p.dir == Some(Direction::In))
                        .map(|p| p.name.text.clone())
                        .collect();
                    self.entity_in_ports.insert(e.name.text.clone(), ins);
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

    fn check_impl(&mut self, im: &ImplDecl) {
        // For an inherent impl of an entity, writes to its `in` ports are errors.
        let in_ports = if im.trait_.is_none() {
            type_head_name(&im.target)
                .and_then(|name| self.entity_in_ports.get(name))
                .cloned()
                .unwrap_or_default()
        } else {
            HashSet::new()
        };

        for item in &im.items {
            match item {
                ImplItem::Const(c) => self.check_expr(&c.value),
                ImplItem::Let(l) => {
                    if let Some(v) = &l.value {
                        self.check_expr(v);
                    }
                }
                ImplItem::Fn(f) => {
                    if let Some(b) = &f.body {
                        self.check_block(b);
                    }
                }
                ImplItem::ModeField { .. } => {}
                ImplItem::Stmt(s) => self.check_stmt(s, &in_ports),
            }
        }
    }

    fn check_block(&mut self, b: &Block) {
        for s in &b.stmts {
            self.check_stmt(s, &HashSet::new());
        }
    }

    fn check_stmt(&mut self, s: &Stmt, in_ports: &HashSet<String>) {
        match s {
            Stmt::Let(l) => {
                if let Some(v) = &l.value {
                    self.check_expr(v);
                }
            }
            Stmt::Assign { target, value, .. } => {
                self.check_write_target(target, in_ports);
                self.check_expr(target);
                self.check_expr(value);
            }
            Stmt::If(i) => self.check_if(i, in_ports),
            Stmt::Match(m) => {
                self.check_expr(&m.scrutinee);
                for arm in &m.arms {
                    for s in &arm.body.stmts {
                        self.check_stmt(s, in_ports);
                    }
                }
            }
            Stmt::For { range, body, .. } => {
                self.check_expr(range);
                for s in &body.stmts {
                    self.check_stmt(s, in_ports);
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

    fn check_if(&mut self, i: &IfStmt, in_ports: &HashSet<String>) {
        self.check_expr(&i.cond);
        for s in &i.then.stmts {
            self.check_stmt(s, in_ports);
        }
        match i.else_.as_deref() {
            Some(ElseBranch::Block(b)) => {
                for s in &b.stmts {
                    self.check_stmt(s, in_ports);
                }
            }
            Some(ElseBranch::If(inner)) => self.check_if(inner, in_ports),
            None => {}
        }
    }

    /// Flag `port = ...` where `port` is a bare `in` port of the entity (spec
    /// 3.18). Field/index writes (`bus.ready = ...`) are intentionally left for
    /// the fuller direction analysis.
    fn check_write_target(&mut self, target: &Expr, in_ports: &HashSet<String>) {
        if let Expr::Path(p) = target {
            if p.segments.len() == 1 {
                let name = &p.segments[0].text;
                if in_ports.contains(name) {
                    self.error(
                        codes::WRITE_TO_INPUT_PORT,
                        p.span,
                        format!("cannot assign to input port `{name}`"),
                    );
                }
            }
        }
    }

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
}
