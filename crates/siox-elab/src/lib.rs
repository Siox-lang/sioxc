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

/// One resolved port connection: `port` of the instance is driven by / drives
/// the local `signal` in the parent.
#[derive(Clone, Debug)]
pub struct Connection {
    pub port: String,
    pub signal: String,
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
            out.push_str(&format!("{pad}  .{} <- {}\n", c.port, c.signal));
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

        stack.push(entity_name.to_string());
        let mut children = Vec::new();
        for spec in specs {
            let sub = type_head_name(spec.ty).unwrap_or("");
            // Only entity constructions are instances; struct/data constructs
            // are ignored here.
            if let Some(sub_decl) = self.entities.get(sub).copied() {
                let cparams = eval_params(sub_decl, spec.ty);
                let cconns = self.resolve_connections(sub_decl, spec.args, spec.site);
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
                        if let Some(Expr::Construct { ty, args, span }) = &l.value {
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
    ) -> Vec<Connection> {
        let ports: HashSet<&str> = edecl.ports.iter().map(|p| p.name.text.as_str()).collect();
        let mut conns = Vec::new();
        let mut connected: HashSet<String> = HashSet::new();

        for arg in args {
            let port = arg.field.text.clone();
            if !ports.contains(port.as_str()) {
                self.error(
                    codes::UNKNOWN_NAME,
                    arg.span,
                    format!("`{}` has no port `{port}`", edecl.name.text),
                );
                continue;
            }
            let signal = match &arg.value {
                // `.clk` shorthand means `.clk = clk`.
                None => arg.field.text.clone(),
                Some(e) => render_signal(e),
            };
            connected.insert(port.clone());
            conns.push(Connection { port, signal });
        }

        for p in &edecl.ports {
            if !connected.contains(&p.name.text) {
                self.error(
                    codes::MISSING_PORT_CONNECTION,
                    site,
                    format!("port `{}` of `{}` is not connected", p.name.text, edecl.name.text),
                );
            }
        }
        conns
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

/// Map the construct's generic arguments to the entity's parameter names.
fn eval_params(edecl: &EntityDecl, ty: &Type) -> Vec<(String, ParamValue)> {
    let args: &[GenericArg] = match ty {
        Type::Generic { args, .. } => args,
        _ => &[],
    };
    let mut out = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        match arg {
            GenericArg::Named { name, value } => out.push((name.text.clone(), eval(value))),
            GenericArg::Positional(value) => {
                let name = edecl
                    .params
                    .params
                    .get(i)
                    .map(|p| p.name.text.clone())
                    .unwrap_or_else(|| format!("arg{i}"));
                out.push((name, eval(value)));
            }
        }
    }
    out
}

/// Constant-evaluate a parameter expression (spec 3.3 const exprs).
fn eval(e: &Expr) -> ParamValue {
    use ParamValue::{Int, Unknown};
    match e {
        Expr::Int { text, .. } => parse_int(text).map(Int).unwrap_or(Unknown),
        Expr::Bool { value, .. } => Int(*value as i64),
        Expr::Unary { op, rhs, .. } => match (op, eval(rhs)) {
            (UnOp::Neg, Int(v)) => Int(-v),
            (UnOp::Not, Int(v)) => Int(!v),
            _ => Unknown,
        },
        Expr::Binary { op, lhs, rhs, .. } => match (eval(lhs), eval(rhs)) {
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
        entity Counter<W: usize> {\n\
          in clk: Clock;\n\
          in rst: Logic;\n\
          out count: uint[W];\n\
        }\n\
        impl Counter<W: usize> {\n\
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
        assert!(tree.contains(".clk <- clk"));
    }

    #[test]
    fn missing_connection_is_reported() {
        // `rst` is left unconnected.
        let src = "module m;\n\
            entity Counter<W: usize> { in clk: Clock; in rst: Logic; out count: uint[W]; }\n\
            impl Counter<W: usize> { count = 0; }\n\
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
            extern entity Ram<W: usize> { in addr: uint[W]; out data: uint[8]; }\n\
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
