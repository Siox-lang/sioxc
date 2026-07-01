//! Entity specialization and elaboration for siox Phase 1 (spec Stage 5).
//!
//! Turns parameterized entities and instances into a concrete elaborated
//! hierarchy: parameter substitution, instance creation, port connection
//! resolution (including `.clk` shorthand), nested hierarchy, external entity
//! stubs, direction checking, and constant-expression evaluation for
//! parameters.
//!
//! Acceptance (spec Stage 5): all entity parameters known after elaboration;
//! all required ports connected or defaulted; direction violations reported;
//! bus modes expand to leaf permissions; external entities are black boxes;
//! the hierarchy can be printed as a tree (`siox tree`).
//!
//! Phase-1 scope of this pass: roots are `#[top]`/`#[test]` entities; instances
//! are top-level `let x = Entity<args> { ... }` constructs in an impl body.
//! Generated instances (loops/arrays), bus-mode leaf expansion, and full
//! direction analysis are noted as follow-ups.

use std::collections::{HashMap, HashSet};
use std::fmt;

use siox_diag::{codes, Diagnostic, DiagnosticSink, Span};
use siox_syntax::ast::*;
use siox_syntax::Module;
use siox_types::Typed;

/// Index into [`Hierarchy::instances`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InstanceId(pub u32);

/// A resolved parameter value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamValue {
    Int(i64),
    /// Could not be evaluated to a constant (e.g. an unbound top-level param).
    Unknown,
}

/// An elaborated type with concrete widths substituted in. A width of `None`
/// means "not yet known" (an unbound parameter). Bus/mode and generic types
/// that don't carry a simple width are kept as a rendered `Other`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EType {
    Bit,
    Logic,
    Bool,
    Clock,
    UInt(Option<u32>),
    Int(Option<u32>),
    Named(String),
    Array { elem: Box<EType>, len: Option<u32> },
    Other(String),
}

impl EType {
    /// The bit width when it is meaningful and known (integers, vectors).
    /// `Bit`/`Logic`/`Bool`/`Clock` return `None` so the width check only
    /// compares actual bit-vector widths, not single-bit kinds.
    pub fn width(&self) -> Option<u32> {
        match self {
            EType::UInt(w) | EType::Int(w) => *w,
            EType::Array { len, .. } => *len,
            _ => None,
        }
    }
}

impl fmt::Display for EType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EType::Bit => write!(f, "Bit"),
            EType::Logic => write!(f, "Logic"),
            EType::Bool => write!(f, "Bool"),
            EType::Clock => write!(f, "Clock"),
            EType::UInt(None) => write!(f, "uint"),
            EType::UInt(Some(w)) => write!(f, "uint[{w}]"),
            EType::Int(None) => write!(f, "int"),
            EType::Int(Some(w)) => write!(f, "int[{w}]"),
            EType::Named(n) => write!(f, "{n}"),
            EType::Array { elem, len: Some(l) } => write!(f, "{elem}[{l}]"),
            EType::Array { elem, len: None } => write!(f, "{elem}[]"),
            EType::Other(s) => write!(f, "{s}"),
        }
    }
}

/// One resolved port connection: `port` of the instance is driven by / drives
/// the local `signal` in the parent. `ty` is the port's type after parameter
/// substitution (e.g. `uint[W]` with `W=8` becomes `uint[8]`).
#[derive(Clone, Debug)]
pub struct Connection {
    pub port: String,
    pub signal: String,
    pub ty: EType,
}

/// One node in the elaborated instance tree.
#[derive(Clone, Debug)]
pub struct Instance {
    /// Instance name (the `let` binding; equals the entity name for a root).
    pub name: String,
    /// Entity type being instantiated.
    pub entity: String,
    pub params: Vec<(String, ParamValue)>,
    /// How this instance's ports connect to the parent's signals (empty for a
    /// root, which has no parent).
    pub connections: Vec<Connection>,
    pub children: Vec<InstanceId>,
    pub is_extern: bool,
}

/// A concrete elaborated design: a forest of instance trees rooted at each
/// `#[top]` / `#[test]` entity.
#[derive(Default)]
pub struct Hierarchy {
    pub roots: Vec<InstanceId>,
    pub instances: Vec<Instance>,
}

impl Hierarchy {
    pub fn instance(&self, id: InstanceId) -> &Instance {
        &self.instances[id.0 as usize]
    }

    /// Render the instance tree (backs `siox tree`).
    pub fn to_tree_string(&self) -> String {
        let mut out = String::new();
        for &root in &self.roots {
            self.write_instance(&mut out, root, 0, true);
        }
        out
    }

    fn write_instance(&self, out: &mut String, id: InstanceId, depth: usize, is_root: bool) {
        let inst = self.instance(id);
        let pad = "  ".repeat(depth);
        let params = format_params(&inst.params);
        let tag = if inst.is_extern { " [extern]" } else { "" };
        if is_root {
            out.push_str(&format!("{pad}{}{params}{tag}\n", inst.entity));
        } else {
            out.push_str(&format!("{pad}{}: {}{params}{tag}\n", inst.name, inst.entity));
        }
        for c in &inst.connections {
            out.push_str(&format!("{pad}  .{}: {} <- {}\n", c.port, c.ty, c.signal));
        }
        for &child in &inst.children {
            self.write_instance(out, child, depth + 1, false);
        }
    }
}

/// Elaborate starting from every `#[top]` / `#[test]` entity.
pub fn elaborate(modules: &[Module], _typed: &Typed, sink: &mut DiagnosticSink) -> Hierarchy {
    let mut e = Elaborator {
        sink,
        entities: HashMap::new(),
        impls: HashMap::new(),
        out: Hierarchy::default(),
    };
    e.collect(modules);

    let mut stack = Vec::new();
    for m in modules {
        for item in &m.items {
            if let Item::Entity(ent) = item {
                if is_root(ent) {
                    let params = ent
                        .params
                        .params
                        .iter()
                        .map(|p| (p.name.text.clone(), ParamValue::Unknown))
                        .collect();
                    let id = e.build(&ent.name.text, &ent.name.text, params, Vec::new(), &mut stack);
                    e.out.roots.push(id);
                }
            }
        }
    }
    e.out
}

struct Elaborator<'a> {
    sink: &'a mut DiagnosticSink,
    entities: HashMap<String, &'a EntityDecl>,
    /// Entity name -> its inherent impls (where instances live).
    impls: HashMap<String, Vec<&'a ImplDecl>>,
    out: Hierarchy,
}

impl<'a> Elaborator<'a> {
    fn collect(&mut self, modules: &'a [Module]) {
        for m in modules {
            for item in &m.items {
                match item {
                    Item::Entity(e) => {
                        self.entities.insert(e.name.text.clone(), e);
                    }
                    Item::Impl(im) if im.trait_.is_none() => {
                        if let Some(name) = type_head_name(&im.target) {
                            self.impls.entry(name.to_string()).or_default().push(im);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn build(
        &mut self,
        inst_name: &str,
        entity_name: &str,
        params: Vec<(String, ParamValue)>,
        connections: Vec<Connection>,
        stack: &mut Vec<String>,
    ) -> InstanceId {
        // Cycle guard: an entity may not (transitively) instantiate itself.
        if stack.iter().any(|s| s == entity_name) {
            let span = self.entities.get(entity_name).map(|e| e.name.span);
            if let Some(span) = span {
                self.error(
                    codes::DUPLICATE_ITEM,
                    span,
                    format!("cyclic instantiation of entity `{entity_name}`"),
                );
            }
            return self.push(Instance {
                name: inst_name.to_string(),
                entity: entity_name.to_string(),
                params,
                connections,
                children: Vec::new(),
                is_extern: true,
            });
        }

        let is_extern = self.entities.get(entity_name).map(|e| e.is_extern).unwrap_or(true);
        let specs = self.gather_instances(entity_name, is_extern);
        let env = param_env(&params);
        // This instance's own signals (ports + impl lets), for width-checking the
        // connections of the children it instantiates.
        let parent_signals = self.entity_signals(entity_name, &env);

        stack.push(entity_name.to_string());
        let mut children = Vec::new();
        for spec in specs {
            let sub = type_head_name(spec.ty).unwrap_or("");
            // Only entity constructions are instances; struct/data constructs
            // are ignored here.
            if let Some(sub_decl) = self.entities.get(sub).copied() {
                // Args may reference this instance's params; ports substitute
                // the child's resolved params.
                let cparams = eval_params(sub_decl, spec.ty, &env);
                let child_env = param_env(&cparams);
                let cconns = self.resolve_connections(sub_decl, spec.args, spec.site, &child_env);
                self.check_widths(&parent_signals, &cconns, spec.site);
                let child = self.build(&spec.name, sub, cparams, cconns, stack);
                children.push(child);
            }
        }
        stack.pop();

        self.push(Instance {
            name: inst_name.to_string(),
            entity: entity_name.to_string(),
            params,
            connections,
            children,
            is_extern,
        })
    }

    /// Collect the instance-construction sites inside an entity's impl bodies.
    fn gather_instances(&self, entity_name: &str, is_extern: bool) -> Vec<InstanceSpec<'a>> {
        let mut specs = Vec::new();
        if is_extern {
            return specs;
        }
        if let Some(impls) = self.impls.get(entity_name) {
            for im in impls {
                for item in &im.items {
                    let let_decl = match item {
                        ImplItem::Let(l) => Some(l),
                        ImplItem::Stmt(Stmt::Let(l)) => Some(l),
                        _ => None,
                    };
                    if let Some(l) = let_decl {
                        // Only a *named* construct (`Counter<W=8> { ... }`) is an
                        // instance; a name-less `{ .f = v }` is a struct value.
                        if let Some(Expr::Construct { ty: Some(ty), args, span }) = &l.value {
                            specs.push(InstanceSpec {
                                name: l.name.text.as_str(),
                                ty,
                                args,
                                site: *span,
                            });
                        }
                    }
                }
            }
        }
        specs
    }

    /// Resolve `{ .clk, .count = c }` against the sub-entity's ports, reporting
    /// unknown ports and missing required connections.
    fn resolve_connections(
        &mut self,
        edecl: &EntityDecl,
        args: &[ConnectArg],
        site: Span,
        env: &HashMap<String, i64>,
    ) -> Vec<Connection> {
        let ports: HashMap<&str, &Type> =
            edecl.ports.iter().map(|p| (p.name.text.as_str(), &p.ty)).collect();
        let mut conns = Vec::new();
        let mut connected: HashSet<String> = HashSet::new();

        for arg in args {
            let port = arg.field.text.clone();
            let Some(port_ty) = ports.get(port.as_str()) else {
                self.error(
                    codes::UNKNOWN_NAME,
                    arg.span,
                    format!("`{}` has no port `{port}`", edecl.name.text),
                );
                continue;
            };
            let signal = match &arg.value {
                // `.clk` shorthand means `.clk = clk`.
                None => arg.field.text.clone(),
                Some(e) => render_signal(e),
            };
            let ty = concrete_ty(port_ty, env);
            connected.insert(port.clone());
            conns.push(Connection { port, signal, ty });
        }

        for p in &edecl.ports {
            if !connected.contains(&p.name.text) {
                self.sink.emit(
                    Diagnostic::error(format!(
                        "port `{}` of `{}` is not connected",
                        p.name.text, edecl.name.text
                    ))
                    .with_code(codes::MISSING_PORT_CONNECTION)
                    .at(site)
                    .help(format!("add `.{} = <signal>` to the connection", p.name.text)),
                );
            }
        }
        conns
    }

    /// The concrete types of an entity's own signals (ports + impl-level lets)
    /// with `env` substituted, used to width-check the connections made to its
    /// child instances.
    fn entity_signals(&self, entity_name: &str, env: &HashMap<String, i64>) -> HashMap<String, EType> {
        let mut sigs = HashMap::new();
        if let Some(edecl) = self.entities.get(entity_name) {
            for p in &edecl.ports {
                sigs.insert(p.name.text.clone(), concrete_ty(&p.ty, env));
            }
        }
        if let Some(impls) = self.impls.get(entity_name) {
            for im in impls {
                for item in &im.items {
                    if let ImplItem::Let(l) = item {
                        if let Some(t) = &l.ty {
                            sigs.insert(l.name.text.clone(), concrete_ty(t, env));
                        }
                    }
                }
            }
        }
        sigs
    }

    /// Report a width mismatch when a port and the local signal it connects to
    /// have different, both-known widths (spec 3.17 / 3.18).
    fn check_widths(
        &mut self,
        parent_signals: &HashMap<String, EType>,
        conns: &[Connection],
        site: Span,
    ) {
        for c in conns {
            let Some(sig) = parent_signals.get(&c.signal) else { continue };
            if let (Some(pw), Some(sw)) = (c.ty.width(), sig.width()) {
                if pw != sw {
                    self.error(
                        codes::TYPE_MISMATCH,
                        site,
                        format!(
                            "width mismatch on port `{}`: the port is `{}` but `{}` is `{}`",
                            c.port, c.ty, c.signal, sig
                        ),
                    );
                }
            }
        }
    }

    fn push(&mut self, inst: Instance) -> InstanceId {
        let id = InstanceId(self.out.instances.len() as u32);
        self.out.instances.push(inst);
        id
    }

    fn error(&mut self, code: &'static str, span: Span, msg: String) {
        self.sink.emit(Diagnostic::error(msg).with_code(code).at(span));
    }
}

/// An instance-construction site discovered in an impl body.
struct InstanceSpec<'a> {
    name: &'a str,
    ty: &'a Type,
    args: &'a [ConnectArg],
    site: Span,
}

fn is_root(e: &EntityDecl) -> bool {
    e.attrs.iter().any(|a| {
        matches!(a.name.segments.last().map(|s| s.text.as_str()), Some("top") | Some("test"))
    })
}

/// The `Int`-valued subset of a param list, as a substitution environment.
fn param_env(params: &[(String, ParamValue)]) -> HashMap<String, i64> {
    params
        .iter()
        .filter_map(|(n, v)| match v {
            ParamValue::Int(i) => Some((n.clone(), *i)),
            ParamValue::Unknown => None,
        })
        .collect()
}

/// Map the construct's generic arguments to the entity's parameter names,
/// evaluating each in `env` (the instantiating scope's parameters).
fn eval_params(edecl: &EntityDecl, ty: &Type, env: &HashMap<String, i64>) -> Vec<(String, ParamValue)> {
    let args: &[GenericArg] = match ty {
        Type::Generic { args, .. } => args,
        _ => &[],
    };
    let mut out = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        match arg {
            GenericArg::Named { name, value } => out.push((name.text.clone(), eval(value, env))),
            GenericArg::Positional(value) => {
                let name = edecl
                    .params
                    .params
                    .get(i)
                    .map(|p| p.name.text.clone())
                    .unwrap_or_else(|| format!("arg{i}"));
                out.push((name, eval(value, env)));
            }
        }
    }
    out
}

/// Constant-evaluate a parameter expression (spec 3.3 const exprs), resolving
/// bare identifiers against `env`.
fn eval(e: &Expr, env: &HashMap<String, i64>) -> ParamValue {
    use ParamValue::{Int, Unknown};
    match e {
        Expr::Int { text, .. } => parse_int(text).map(Int).unwrap_or(Unknown),
        Expr::Bool { value, .. } => Int(*value as i64),
        Expr::Path(p) if p.segments.len() == 1 => {
            env.get(&p.segments[0].text).copied().map(Int).unwrap_or(Unknown)
        }
        Expr::Unary { op, rhs, .. } => match (op, eval(rhs, env)) {
            (UnOp::Neg, Int(v)) => Int(-v),
            (UnOp::Not, Int(v)) => Int(!v),
            _ => Unknown,
        },
        Expr::Binary { op, lhs, rhs, .. } => match (eval(lhs, env), eval(rhs, env)) {
            (Int(a), Int(b)) => match op {
                BinOp::Add => Int(a + b),
                BinOp::Sub => Int(a - b),
                BinOp::Mul => Int(a * b),
                BinOp::Div if b != 0 => Int(a / b),
                BinOp::Shl => Int(a << b),
                BinOp::Shr => Int(a >> b),
                BinOp::And => Int(a & b),
                BinOp::Or => Int(a | b),
                _ => Unknown,
            },
            _ => Unknown,
        },
        _ => Unknown,
    }
}

/// Resolve a port/signal type to a structured [`EType`] with `env` substituted.
fn concrete_ty(t: &Type, env: &HashMap<String, i64>) -> EType {
    match t {
        Type::Path(p) => match p.segments.last().map(|s| s.text.as_str()) {
            Some("Bit") => EType::Bit,
            Some("Logic") => EType::Logic,
            Some("Bool") => EType::Bool,
            Some("Clock") => EType::Clock,
            Some("uint") | Some("integer") => EType::UInt(None),
            Some("int") => EType::Int(None),
            Some(other) => EType::Named(other.to_string()),
            None => EType::Other(String::new()),
        },
        Type::Indexed { base, index, .. } => {
            let len = index_width(index, env);
            match concrete_ty(base, env) {
                EType::UInt(_) => EType::UInt(len),
                EType::Int(_) => EType::Int(len),
                elem => EType::Array { elem: Box::new(elem), len },
            }
        }
        // Bus-mode and generic types don't carry a simple scalar width; keep a
        // rendered form for display and skip width checking on them.
        Type::Generic { .. } | Type::Mode { .. } => EType::Other(render_concrete(t, env)),
    }
}

/// The bit width implied by a type index: a single value is the width itself
/// (`uint[8]` -> 8); a descending/ascending range is its span (`[31..0]` -> 32).
fn index_width(index: &Expr, env: &HashMap<String, i64>) -> Option<u32> {
    if let Expr::Range { lo, hi, .. } = index {
        if let (ParamValue::Int(a), ParamValue::Int(b)) = (eval(lo, env), eval(hi, env)) {
            return Some((a - b).unsigned_abs() as u32 + 1);
        }
        return None;
    }
    match eval(index, env) {
        ParamValue::Int(v) if v >= 0 => Some(v as u32),
        _ => None,
    }
}

/// Render a port type with parameter widths substituted (`uint[W]` with `W=8`
/// becomes `uint[8]`; unresolved widths keep their symbolic form).
fn render_concrete(t: &Type, env: &HashMap<String, i64>) -> String {
    match t {
        Type::Path(p) => p.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("::"),
        Type::Indexed { base, index, .. } => {
            format!("{}[{}]", render_concrete(base, env), render_index(index, env))
        }
        Type::Generic { base, args, .. } => {
            let inner = args
                .iter()
                .map(|a| match a {
                    GenericArg::Positional(e) => render_index(e, env),
                    GenericArg::Named { name, value } => {
                        format!("{} = {}", name.text, render_index(value, env))
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}<{inner}>", render_concrete(base, env))
        }
        Type::Mode { dir, inner, mode, .. } => {
            let m = mode.as_ref().map(|n| format!("::{}", n.text)).unwrap_or_default();
            format!("{} {}{m}", dir_str(*dir), render_concrete(inner, env))
        }
    }
}

/// Render a type-index expression, substituting a constant value when known.
fn render_index(e: &Expr, env: &HashMap<String, i64>) -> String {
    match eval(e, env) {
        ParamValue::Int(v) => v.to_string(),
        ParamValue::Unknown => render_expr(e),
    }
}

fn render_expr(e: &Expr) -> String {
    match e {
        Expr::Path(p) => p.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("::"),
        Expr::Int { text, .. } => text.clone(),
        Expr::Range { lo, hi, .. } => format!("{}..{}", render_expr(lo), render_expr(hi)),
        Expr::Index { base, index, .. } => format!("{}[{}]", render_expr(base), render_expr(index)),
        _ => "?".to_string(),
    }
}

fn dir_str(d: Direction) -> &'static str {
    match d {
        Direction::In => "in",
        Direction::Out => "out",
        Direction::Inout => "inout",
    }
}

fn parse_int(text: &str) -> Option<i64> {
    let t = text.trim();
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        i64::from_str_radix(h, 16).ok()
    } else if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        i64::from_str_radix(b, 2).ok()
    } else {
        t.parse().ok()
    }
}

/// Render the local signal a port connects to. Bare paths render as their name;
/// other expressions render to a placeholder for the tree view.
fn render_signal(e: &Expr) -> String {
    match e {
        Expr::Path(p) => p.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("::"),
        Expr::Int { text, .. } => text.clone(),
        Expr::LogicLit { ch, .. } => format!("'{ch}'"),
        _ => "<expr>".to_string(),
    }
}

fn type_head_name(ty: &Type) -> Option<&str> {
    match ty {
        Type::Path(p) => p.segments.first().map(|s| s.text.as_str()),
        Type::Generic { base, .. } | Type::Indexed { base, .. } => type_head_name(base),
        Type::Mode { inner, .. } => type_head_name(inner),
    }
}

fn format_params(params: &[(String, ParamValue)]) -> String {
    if params.is_empty() {
        return String::new();
    }
    let inner = params
        .iter()
        .map(|(n, v)| match v {
            ParamValue::Int(i) => format!("{n}={i}"),
            ParamValue::Unknown => format!("{n}=?"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("<{inner}>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use siox_diag::FileId;

    fn elaborate_src(src: &str) -> (Hierarchy, usize) {
        let mut sink = DiagnosticSink::new();
        let module = siox_syntax::parse_module(FileId(0), src, &mut sink);
        assert_eq!(sink.error_count(), 0, "source failed to parse:\n{src}");
        let modules = std::slice::from_ref(&module);
        let resolved = siox_resolve::resolve(modules, &mut sink);
        let typed = siox_types::check(modules, &resolved, &mut sink);
        let before = sink.error_count();
        let hier = elaborate(modules, &typed, &mut sink);
        (hier, sink.error_count() - before)
    }

    const HARNESS: &str = "module m;\n\
        entity Counter<W: integer> {\n\
          in clk: Clock;\n\
          in rst: Logic;\n\
          out count: uint[W];\n\
        }\n\
        impl Counter<W: integer> {\n\
          let value: uint[W] = 0;\n\
          count = value;\n\
        }\n\
        #[top]\n\
        entity Harness {}\n\
        impl Harness {\n\
          let clk: Logic = '0';\n\
          let rst: Logic = '1';\n\
          let count: uint[8];\n\
          let dut = Counter<W = 8> {\n\
            .clk,\n\
            .rst,\n\
            .count = count,\n\
          };\n\
        }\n";

    #[test]
    fn builds_instance_tree_with_params_and_connections() {
        let (hier, errors) = elaborate_src(HARNESS);
        assert_eq!(errors, 0);
        assert_eq!(hier.roots.len(), 1);
        let root = hier.instance(hier.roots[0]);
        assert_eq!(root.entity, "Harness");
        assert_eq!(root.children.len(), 1);

        let dut = hier.instance(root.children[0]);
        assert_eq!(dut.name, "dut");
        assert_eq!(dut.entity, "Counter");
        assert_eq!(dut.params, vec![("W".to_string(), ParamValue::Int(8))]);
        // `.clk` shorthand resolves to signal `clk`; `.count = count` explicit.
        assert!(dut
            .connections
            .iter()
            .any(|c| c.port == "clk" && c.signal == "clk"));
        assert!(dut
            .connections
            .iter()
            .any(|c| c.port == "count" && c.signal == "count"));
    }

    #[test]
    fn tree_string_is_rendered() {
        let (hier, _) = elaborate_src(HARNESS);
        let tree = hier.to_tree_string();
        assert!(tree.contains("Harness"));
        assert!(tree.contains("dut: Counter<W=8>"));
        assert!(tree.contains(".clk: Clock <- clk"));
    }

    #[test]
    fn parameter_widths_are_substituted_into_port_types() {
        let (hier, _) = elaborate_src(HARNESS);
        let root = hier.instance(hier.roots[0]);
        let dut = hier.instance(root.children[0]);
        // `count: uint[W]` with W=8 becomes `uint[8]`.
        let count = dut.connections.iter().find(|c| c.port == "count").unwrap();
        assert_eq!(count.ty, EType::UInt(Some(8)));
    }

    #[test]
    fn connection_width_mismatch_is_reported() {
        // Port `a` is uint[8] (W=8) but the local signal `a` is uint[4].
        let src = "module m;\n\
            entity Sub<W: integer> { in a: uint[W]; out b: uint[W]; }\n\
            impl Sub<W: integer> { b = a; }\n\
            #[top]\n\
            entity Top {}\n\
            impl Top {\n\
              let a: uint[4];\n\
              let b: uint[8];\n\
              let dut = Sub<W = 8> { .a = a, .b = b };\n\
            }\n";
        let (_, errors) = elaborate_src(src);
        assert_eq!(errors, 1);
    }

    #[test]
    fn matching_widths_are_fine() {
        let src = "module m;\n\
            entity Sub<W: integer> { in a: uint[W]; out b: uint[W]; }\n\
            impl Sub<W: integer> { b = a; }\n\
            #[top]\n\
            entity Top {}\n\
            impl Top {\n\
              let a: uint[8];\n\
              let b: uint[8];\n\
              let dut = Sub<W = 8> { .a = a, .b = b };\n\
            }\n";
        let (_, errors) = elaborate_src(src);
        assert_eq!(errors, 0);
    }

    #[test]
    fn missing_connection_is_reported() {
        // `rst` is left unconnected.
        let src = "module m;\n\
            entity Counter<W: integer> { in clk: Clock; in rst: Logic; out count: uint[W]; }\n\
            impl Counter<W: integer> { count = 0; }\n\
            #[top]\n\
            entity H {}\n\
            impl H {\n\
              let clk: Logic = '0';\n\
              let count: uint[8];\n\
              let dut = Counter<W = 8> { .clk, .count };\n\
            }\n";
        let (_, errors) = elaborate_src(src);
        assert_eq!(errors, 1);
    }

    #[test]
    fn unknown_port_is_reported() {
        let src = "module m;\n\
            entity Counter { out count: uint[8]; }\n\
            impl Counter { count = 0; }\n\
            #[top]\n\
            entity H {}\n\
            impl H {\n\
              let count: uint[8];\n\
              let dut = Counter { .count, .nope = count };\n\
            }\n";
        let (_, errors) = elaborate_src(src);
        assert_eq!(errors, 1);
    }

    #[test]
    fn extern_entity_is_a_black_box() {
        let src = "module m;\n\
            extern entity Ram<W: integer> { in addr: uint[W]; out data: uint[8]; }\n\
            #[top]\n\
            entity H {}\n\
            impl H {\n\
              let addr: uint[4];\n\
              let data: uint[8];\n\
              let mem = Ram<W = 4> { .addr, .data };\n\
            }\n";
        let (hier, errors) = elaborate_src(src);
        assert_eq!(errors, 0);
        let root = hier.instance(hier.roots[0]);
        let mem = hier.instance(root.children[0]);
        assert!(mem.is_extern);
        assert!(mem.children.is_empty());
    }
}
