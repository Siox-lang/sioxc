//! Entity specialization and elaboration for siox Phase 1 (spec Stage 5).
//!
//! Turns parameterized entities and instances into a concrete elaborated
//! hierarchy: parameter substitution, instance creation, port connection
//! resolution (explicit `.port = signal` and positional forms), nested
//! hierarchy, external entity stubs, direction checking, and
//! constant-expression evaluation for parameters.
//!
//! Acceptance (spec Stage 5): all entity parameters known after elaboration;
//! all required ports connected or defaulted; direction violations reported;
//! bus modes expand to leaf permissions; external entities are black boxes;
//! the hierarchy can be printed as a tree (`siox tree`).
//!
//! Phase-1 scope of this pass: roots are `#[top]`/`#[test]` entities; instances
//! are top-level `let x: Entity<args> = { ... }` constructs in an impl body.
//! Generated instances (loops/arrays), applied-view leaf expansion, and full
//! direction analysis are noted as follow-ups.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::diag::{codes, Diagnostic, DiagnosticSink, Span};
use crate::syntax::ast::*;
use crate::syntax::Module;
use crate::types::{Ty, Typed};

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
    /// Any named type: an enum scalar, a struct, or a
    /// bare bit-vector family (`unsigned`). No bit-vector width, so the width check
    /// skips it.
    Named(String),
    /// A sized array. A bit vector is just an array of bits: `unsigned[8]` is
    /// `Array { elem: Named("unsigned"), len: 8 }` (element names the family so it
    /// renders as `unsigned[8]`), the same encoding as `Bit[8]` or `Point[4]`.
    /// Signedness/behaviour lives in the family's operator impls, not here.
    Array {
        elem: Box<EType>,
        len: Option<u32>,
    },
    Other(String),
}

impl EType {
    /// The width the connection check compares: an array's length (a bit
    /// vector's bit count). A named scalar has none, so the check skips it.
    pub fn width(&self) -> Option<u32> {
        match self {
            EType::Array { len, .. } => *len,
            _ => None,
        }
    }
}

impl fmt::Display for EType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EType::Named(n) => write!(f, "{n}"),
            EType::Array { elem, len: Some(l) } => write!(f, "{elem}[{l}]"),
            EType::Array { elem, len: None } => write!(f, "{elem}[]"),
            EType::Other(s) => write!(f, "{s}"),
        }
    }
}

/// One resolved port connection: `port` of the instance is driven by / drives
/// the local `signal` in the parent. `ty` is the port's type after parameter
/// substitution (e.g. `unsigned[W]` with `W=8` becomes `unsigned[8]`).
#[derive(Clone, Debug)]
pub struct Connection {
    pub port: String,
    pub signal: String,
    pub ty: EType,
    /// The connection site (`.bus = wire`), for diagnostics.
    pub span: Span,
}

/// One node in the elaborated instance tree.
#[derive(Clone, Debug)]
pub struct Instance {
    /// Instance name (the `let` binding; equals the entity name for a root).
    pub name: String,
    /// Metadata attributes from the instance `let` (`#[external_clock = true]
    /// let p: Pll = { .. };`) — (name, pretty-printed value). Preserved for
    /// external tools (netlist/constraint emission, spec 3.5).
    pub attrs: Vec<(String, Option<String>)>,
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
    pub(crate) expr_types: HashMap<Span, Ty>,
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
        let attrs = inst
            .attrs
            .iter()
            .map(|(n, v)| match v {
                Some(v) => format!(" #[{n} = {v}]"),
                None => format!(" #[{n}]"),
            })
            .collect::<String>();
        if is_root {
            out.push_str(&format!("{pad}{}{params}{tag}{attrs}\n", inst.entity));
        } else {
            out.push_str(&format!(
                "{pad}{}: {}{params}{tag}{attrs}\n",
                inst.name, inst.entity
            ));
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
pub fn elaborate(modules: &[Module], typed: &Typed, sink: &mut DiagnosticSink) -> Hierarchy {
    elaborate_roots(modules, typed, sink, is_root)
}

/// Elaborate for `check`: the usual roots, plus every entity that nothing
/// instantiates.
///
/// Structural analysis — unknown ports, undriven signals, combinational loops,
/// unresolved names — only sees what elaboration reaches, so a library entity
/// written before its first use was never analysed at all. A body with a
/// misspelled port, an undriven signal and an unresolved name in it reported
/// `check ok`. An entity that *is* instantiated still arrives through its
/// parent, so it is not rooted twice.
pub fn elaborate_for_check(
    modules: &[Module],
    typed: &Typed,
    sink: &mut DiagnosticSink,
) -> Hierarchy {
    let instantiated = instantiated_entities(modules);
    elaborate_roots(modules, typed, sink, |ent| {
        is_root(ent) || !instantiated.contains(&ent.name.text)
    })
}

/// Entity names used as the type of an instance `let` anywhere. Such a name is
/// reached through its parent, so `check` need not root it itself.
fn instantiated_entities(modules: &[Module]) -> HashSet<String> {
    let declared: HashSet<&str> = modules
        .iter()
        .flat_map(|m| &m.items)
        .filter_map(|item| match item {
            Item::Entity(e) => Some(e.name.text.as_str()),
            _ => None,
        })
        .collect();
    let mut out = HashSet::new();
    for m in modules {
        for item in &m.items {
            let Item::Impl(im) = item else { continue };
            for it in &im.items {
                let ImplItem::Let(l) = it else { continue };
                let head = l.ty.as_ref().and_then(type_head_name);
                if let Some(head) = head.filter(|h| declared.contains(h)) {
                    out.insert(head.to_string());
                }
            }
        }
    }
    out
}

/// Elaborate rooted at a single named entity — for `sioxc build`, which builds
/// one top-level module setup (not the testbenches). Lowering only lowers
/// entities that appear in the hierarchy, so this yields just the top and its
/// instantiated children. `roots` is empty if the entity isn't found.
pub fn elaborate_top(
    modules: &[Module],
    typed: &Typed,
    sink: &mut DiagnosticSink,
    top: &str,
) -> Hierarchy {
    elaborate_roots(modules, typed, sink, |ent| ent.name.text == top)
}

fn elaborate_roots(
    modules: &[Module],
    typed: &Typed,
    sink: &mut DiagnosticSink,
    is_selected: impl Fn(&EntityDecl) -> bool,
) -> Hierarchy {
    let mut e = Elaborator {
        sink,
        misplaced: std::cell::RefCell::new(Vec::new()),
        entities: HashMap::new(),
        impls: HashMap::new(),
        families: HashSet::new(),
        out: Hierarchy::default(),
    };
    e.out.expr_types = typed.expr_types().clone();
    e.collect(modules);

    let mut stack = Vec::new();
    for m in modules {
        for item in &m.items {
            if let Item::Entity(ent) = item {
                if is_selected(ent) {
                    let params = ent
                        .params
                        .params
                        .iter()
                        .map(|p| (p.name.text.clone(), ParamValue::Unknown))
                        .collect();
                    let id = e.build(
                        &ent.name.text,
                        &ent.name.text,
                        params,
                        Vec::new(),
                        Vec::new(),
                        &mut stack,
                    );
                    e.out.roots.push(id);
                }
            }
        }
    }
    e.report_misplaced();
    e.out
}

struct Elaborator<'a> {
    sink: &'a mut DiagnosticSink,
    /// Instances found where structural elaboration cannot reach them: inside
    /// a behavioural `if`, whose condition is not a constant and so is a
    /// process, not a generate. Recorded during gathering (which borrows
    /// `&self`) and reported once, deduplicated by span — one entity is
    /// elaborated once per instantiation of it.
    misplaced: std::cell::RefCell<Vec<(String, Span)>>,
    entities: HashMap<String, &'a EntityDecl>,
    /// Entity name -> its inherent impls (where instances live).
    impls: HashMap<String, Vec<&'a ImplDecl>>,
    /// Bit-vector families (`struct F : Logic[]`), for width-typing vectors.
    families: HashSet<String>,
    out: Hierarchy,
}

impl<'a> Elaborator<'a> {
    fn collect(&mut self, modules: &'a [Module]) {
        self.families = crate::ir::vector_families(modules);
        for m in modules {
            for item in &m.items {
                match item {
                    Item::Entity(e) => {
                        self.entities.insert(e.name.text.clone(), e);
                    }
                    Item::Struct(_) => {}
                    Item::View(_) => {}
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
        attrs: Vec<(String, Option<String>)>,
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
                attrs,
                entity: entity_name.to_string(),
                params,
                connections,
                children: Vec::new(),
                is_extern: true,
            });
        }

        let is_extern = self
            .entities
            .get(entity_name)
            .map(|e| e.is_extern)
            .unwrap_or(true);
        let env = self.with_impl_binders(entity_name, param_env(&params));
        let specs = self.gather_instances(entity_name, is_extern, &env);
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
                self.check_generic_arg_names(sub_decl, spec.ty, spec.site);
                let cparams = eval_params(sub_decl, spec.ty, &env);
                self.check_params_bound(sub_decl, &cparams, spec.site);
                let child_env = param_env(&cparams);
                // Ports this instance drives post-declaration (`inst.p = x;`)
                // count as connected for the missing-connection check.
                let driven = self.post_decl_driven(entity_name, &spec.name);
                let cconns = self.resolve_connections(
                    sub_decl,
                    &spec.args,
                    spec.site,
                    &child_env,
                    &spec.loop_env,
                    &driven,
                );
                self.check_widths(&parent_signals, &cconns, spec.site);
                let child_attrs = spec
                    .attrs
                    .iter()
                    .map(|a| {
                        let name = a
                            .name
                            .segments
                            .last()
                            .map(|s| s.text.clone())
                            .unwrap_or_default();
                        (
                            name,
                            a.value.as_ref().map(crate::syntax::pretty::expr_string),
                        )
                    })
                    .collect();
                let child = self.build(&spec.name, sub, cparams, cconns, child_attrs, stack);
                children.push(child);
            }
        }
        stack.pop();

        self.push(Instance {
            name: inst_name.to_string(),
            attrs,
            entity: entity_name.to_string(),
            params,
            connections,
            children,
            is_extern,
        })
    }

    /// Collect the instance-construction sites inside an entity's impl bodies.
    /// A generic implementation binds its own names for the entity's
    /// parameters (`impl<M: integer> Outer<M>`), matched by position. The env
    /// here is keyed by the *entity's* names, and everything downstream reads
    /// it — including the arguments an instance passes to a child, so
    /// `let child: Inner<K = M> = {}` could not evaluate `M` and reported the
    /// child's parameter as unbound. Add each binder's names alongside.
    fn with_impl_binders(
        &self,
        entity_name: &str,
        mut env: HashMap<String, i64>,
    ) -> HashMap<String, i64> {
        let (Some(edecl), Some(impls)) = (
            self.entities.get(entity_name).copied(),
            self.impls.get(entity_name),
        ) else {
            return env;
        };
        for im in impls {
            let Type::Generic { args, .. } = &im.target else {
                continue;
            };
            for (i, arg) in args.iter().enumerate() {
                let GenericArg::Positional(Expr::Path(path)) = arg else {
                    continue;
                };
                let ([seg], Some(param)) = (path.segments.as_slice(), edecl.params.params.get(i))
                else {
                    continue;
                };
                if seg.text == param.name.text {
                    continue;
                }
                if let Some(&value) = env.get(&param.name.text) {
                    env.insert(seg.text.clone(), value);
                }
            }
        }
        env
    }

    fn gather_instances(
        &self,
        entity_name: &str,
        is_extern: bool,
        env: &HashMap<String, i64>,
    ) -> Vec<InstanceSpec<'a>> {
        let mut specs = Vec::new();
        if is_extern {
            return specs;
        }
        // This entity's bare type parameters (`Buf<T>`): a `let s: T` names data
        // whose type is the bound argument (`unsigned[8]`), never an instance — even
        // when an entity happens to be named `T`.
        let tparams: HashSet<String> = self
            .entities
            .get(entity_name)
            .map(|e| {
                e.params
                    .params
                    .iter()
                    .filter(|p| p.bound.is_none())
                    .map(|p| p.name.text.clone())
                    .collect()
            })
            .unwrap_or_default();
        if let Some(impls) = self.impls.get(entity_name) {
            for im in impls {
                for item in &im.items {
                    match item {
                        ImplItem::Let(l) => self.gather_let(l, env, &tparams, &mut specs),
                        ImplItem::Stmt(s) => self.gather_stmt(s, env, &tparams, &mut specs),
                        _ => {}
                    }
                }
            }
        }
        specs
    }

    /// An instance `let`, in either form, as `(instance type, connections,
    /// site span)`:
    /// - `let x: Entity = { .. }` — the type is on the construct.
    /// - `let x: Entity = { .. }` — the type is the annotation; the value is a
    ///   name-less construct (`{ .a = a }`, dotted) or, since a positional/empty
    ///   `{ .. }` lexes as a concatenation, a concat whose parts are positional
    ///   connections.
    /// - `let x: Entity;` — the type is the annotation; no connections (ports
    ///   wired post-declaration).
    fn instance_let(
        &self,
        l: &'a LetDecl,
        tparams: &HashSet<String>,
    ) -> Option<(&'a Type, Vec<ConnectArg>, Span)> {
        // Old form: `= Entity { .. }`.
        if let Some(Expr::Construct {
            ty: Some(ty),
            args,
            span,
            ..
        }) = &l.value
        {
            return Some((ty, args.clone(), *span));
        }
        // New forms need a bare entity-typed annotation. An *array* of an
        // entity (`let stage: Inc[N]`) is an instance array, built element-wise
        // by `stage[i] = Inc { .. }` assignments — not a single instance here.
        let ann = l.ty.as_ref()?;
        if matches!(ann, Type::Indexed { .. }) {
            return None;
        }
        // A bare type parameter (`let s: T` in `impl Buf<T>`) is data, not an
        // instance, even when an entity is named `T`.
        if type_head_name(ann).is_some_and(|n| tparams.contains(n)) {
            return None;
        }
        if !type_head_name(ann).is_some_and(|n| self.entities.contains_key(n)) {
            return None;
        }
        match &l.value {
            // `let x: Entity = { .a = a }` — dotted name-less construct.
            Some(Expr::Construct { ty: None, args, .. }) => Some((ann, args.clone(), l.span)),
            // `let x: Entity = { a, b }` / `= {}` — a positional/empty block
            // lexes as a concat; its parts are positional connections.
            Some(Expr::Concat { parts, .. }) => {
                let args = parts
                    .iter()
                    .map(|p| ConnectArg {
                        field: None,
                        value: Some(p.clone()),
                        span: l.span,
                    })
                    .collect();
                Some((ann, args, l.span))
            }
            // `let x: Entity;` — no connections.
            None => Some((ann, Vec::new(), l.span)),
            _ => None,
        }
    }

    /// One instance `let` -> an instance spec (with the current loop bindings
    /// for its connection rendering).
    fn gather_let(
        &self,
        l: &'a LetDecl,
        env: &HashMap<String, i64>,
        tparams: &HashSet<String>,
        out: &mut Vec<InstanceSpec<'a>>,
    ) {
        if let Some((ty, args, span)) = self.instance_let(l, tparams) {
            // A generated instance gets the loop index appended for a unique
            // name; a plain one keeps its declared name.
            let name = if env.is_empty() {
                l.name.text.clone()
            } else {
                let idx: Vec<String> = env.values().map(|v| v.to_string()).collect();
                format!("{}_{}", l.name.text, idx.join("_"))
            };
            out.push(InstanceSpec {
                name,
                ty,
                args,
                attrs: &l.attrs,
                site: span,
                loop_env: env.clone(),
            });
        }
    }

    /// A statement inside an impl body / loop: `let` instances and `for` loops
    /// (unrolled over a static range, binding the loop variable).
    fn gather_stmt(
        &self,
        s: &'a Stmt,
        env: &HashMap<String, i64>,
        tparams: &HashSet<String>,
        out: &mut Vec<InstanceSpec<'a>>,
    ) {
        match s {
            Stmt::Let(l) => self.gather_let(l, env, tparams, out),
            // Instance-array element construction: `stage[i] = Sub { .. }`. The
            // target renders to the element name (`stage[1]`) with the loop
            // index evaluated, so `stage[i].port` reads resolve to it.
            Stmt::Assign {
                target,
                value:
                    Expr::Construct {
                        ty: Some(ty),
                        args,
                        span,
                        ..
                    },
                ..
            } => {
                out.push(InstanceSpec {
                    name: render_signal(target, env),
                    ty,
                    args: args.clone(),
                    attrs: &[],
                    site: *span,
                    loop_env: env.clone(),
                });
            }
            Stmt::For {
                var,
                range: Expr::Range { lo, hi, .. },
                body,
                ..
            } => {
                if let (ParamValue::Int(a), ParamValue::Int(b)) = (eval(lo, env), eval(hi, env)) {
                    // Inclusive, directional range (`0..2` -> 0,1,2;
                    // `2..0` -> 2,1,0), matching slices/array ranges.
                    for i in loop_range(a, b) {
                        let mut e = env.clone();
                        e.insert(var.text.clone(), i);
                        for st in &body.stmts {
                            self.gather_stmt(st, &e, tparams, out);
                        }
                    }
                }
            }
            // `if <const> { .. } else { .. }`: a generate-if. The condition is
            // constant-folded; only the taken branch's instances are gathered.
            // A non-constant condition is a behavioral `if`, not a generate-if.
            Stmt::If(iff) => self.gather_if(iff, env, tparams, out),
            _ => {}
        }
    }

    /// Emit one error per instantiation elaboration could not reach. An
    /// entity is elaborated once per instantiation of *it*, so the same
    /// source line can be recorded several times.
    fn report_misplaced(&mut self) {
        let mut seen: Vec<(String, Span)> = self.misplaced.borrow().clone();
        seen.sort_by_key(|(_, span)| (span.file.0, span.start));
        seen.dedup_by_key(|(_, span)| (span.file.0, span.start));
        for (name, span) in seen {
            self.sink.emit(
                Diagnostic::error(format!(
                    "an entity cannot be instantiated in a process: `{name}`"
                ))
                .with_code(codes::INSTANCE_PLACEMENT)
                .at(span)
                .help(
                    "this `if` tests a signal, so it is a process, not a \
                     generate. Instantiate at the top of the entity body, or \
                     inside a generate `if` whose condition folds to a \
                     constant, and drive the instance's ports from the process",
                ),
            );
        }
    }

    /// Record every entity instantiation inside a block that elaboration will
    /// not walk, so it can be reported rather than silently dropped. Nested
    /// `if`/`for`/`match` bodies count too: none of them is reachable once the
    /// enclosing `if` is behavioural.
    fn note_misplaced(&self, b: &Block, tparams: &HashSet<String>) {
        for st in &b.stmts {
            match st {
                Stmt::Let(l) => {
                    if let Some(head) = l.ty.as_ref().and_then(type_head_name) {
                        // A bare type parameter names data, not an instance,
                        // even when an entity happens to share its name — the
                        // same exclusion `gather_instances` makes.
                        if self.entities.contains_key(head) && !tparams.contains(head) {
                            self.misplaced
                                .borrow_mut()
                                .push((l.name.text.clone(), l.span));
                        }
                    }
                }
                Stmt::If(inner) => self.note_misplaced_if(inner, tparams),
                Stmt::For { body, .. } => self.note_misplaced(body, tparams),
                Stmt::Match(m) => {
                    for arm in &m.arms {
                        self.note_misplaced(&arm.body, tparams);
                    }
                }
                _ => {}
            }
        }
    }

    fn note_misplaced_if(&self, iff: &IfStmt, tparams: &HashSet<String>) {
        self.note_misplaced(&iff.then, tparams);
        match iff.else_.as_deref() {
            Some(ElseBranch::Block(b)) => self.note_misplaced(b, tparams),
            Some(ElseBranch::If(inner)) => self.note_misplaced_if(inner, tparams),
            None => {}
        }
    }

    fn gather_if(
        &self,
        iff: &'a IfStmt,
        env: &HashMap<String, i64>,
        tparams: &HashSet<String>,
        out: &mut Vec<InstanceSpec<'a>>,
    ) {
        match eval(&iff.cond, env) {
            ParamValue::Int(0) => match iff.else_.as_deref() {
                Some(ElseBranch::Block(b)) => {
                    for st in &b.stmts {
                        self.gather_stmt(st, env, tparams, out);
                    }
                }
                Some(ElseBranch::If(inner)) => self.gather_if(inner, env, tparams, out),
                None => {}
            },
            ParamValue::Int(_) => {
                for st in &iff.then.stmts {
                    self.gather_stmt(st, env, tparams, out);
                }
            }
            // A non-constant condition is behavioural — a process, not a
            // generate. Instances here were dropped without a word, so the
            // design ran as though they had never been written.
            ParamValue::Unknown => {
                self.note_misplaced(&iff.then, tparams);
                match iff.else_.as_deref() {
                    Some(ElseBranch::Block(b)) => self.note_misplaced(b, tparams),
                    Some(ElseBranch::If(inner)) => self.note_misplaced_if(inner, tparams),
                    None => {}
                }
            }
        }
    }

    /// Resolve `{ .clk = clk, .count = c }` against the sub-entity's ports, reporting
    /// unknown ports and missing required connections.
    /// The ports of instance `inst` (inside entity `entity_name`'s impls) that
    /// are driven post-declaration by `inst.port = ...` statements — the third
    /// struct-style connection form (`let dut: E; dut.a = a;`).
    fn post_decl_driven(&self, entity_name: &str, inst: &str) -> HashSet<String> {
        let mut out = HashSet::new();
        if let Some(impls) = self.impls.get(entity_name) {
            for im in impls {
                for item in &im.items {
                    if let ImplItem::Stmt(s) = item {
                        collect_field_assign_ports(s, inst, &mut out);
                    }
                }
            }
        }
        out
    }

    /// Spec Stage 5 requires every entity parameter to be known after
    /// elaboration. An instantiation that leaves one unbound produced a signal
    /// of unknown width (`E.d.y : ?`) with no diagnostic, failing much later at
    /// the engine. Roots are exempt — a top-level entity is never instantiated,
    /// so its parameters are legitimately open.
    fn check_params_bound(
        &mut self,
        edecl: &EntityDecl,
        params: &[(String, ParamValue)],
        site: Span,
    ) {
        let bound: HashMap<&str, ParamValue> =
            params.iter().map(|(n, v)| (n.as_str(), *v)).collect();
        let missing: Vec<String> = edecl
            .params
            .params
            .iter()
            // Only *value* parameters (`W: integer`) need a number. A type
            // parameter (`Buf<T>`) is bound to a type, which has no
            // `ParamValue`, so requiring one would reject every generic.
            .filter(|p| {
                p.bound
                    .as_ref()
                    .and_then(|t| type_head_name(t))
                    .is_some_and(|b| b == "integer")
            })
            .map(|p| p.name.text.as_str())
            .filter(|n| !matches!(bound.get(n), Some(ParamValue::Int(_))))
            .map(|n| format!("`{n}`"))
            .collect();
        if !missing.is_empty() {
            self.error(
                codes::UNKNOWN_NAME,
                site,
                format!(
                    "`{}` needs a value for {} — an entity parameter must be known at elaboration",
                    edecl.name.text,
                    missing.join(", ")
                ),
            );
        }
    }

    /// A named generic argument must name a parameter the entity declares.
    /// `S<W = 8, Z = 3>` used to bind `Z` into the instance's parameter list
    /// and carry on, so a typo'd parameter silently did nothing.
    fn check_generic_arg_names(&mut self, edecl: &EntityDecl, ty: &Type, site: Span) {
        let Type::Generic { args, .. } = ty else {
            return;
        };
        for arg in args {
            let GenericArg::Named { name, .. } = arg else {
                continue;
            };
            if !edecl.params.params.iter().any(|p| p.name.text == name.text) {
                let known: Vec<&str> = edecl
                    .params
                    .params
                    .iter()
                    .map(|p| p.name.text.as_str())
                    .collect();
                let mut d = Diagnostic::error(format!(
                    "`{}` has no parameter `{}`",
                    edecl.name.text, name.text
                ))
                .with_code(codes::UNKNOWN_NAME)
                .at(site);
                if !known.is_empty() {
                    d = d.help(format!("it declares: {}", known.join(", ")));
                }
                self.sink.emit(d);
            }
        }
    }

    fn resolve_connections(
        &mut self,
        edecl: &EntityDecl,
        args: &[ConnectArg],
        site: Span,
        env: &HashMap<String, i64>,
        render_env: &HashMap<String, i64>,
        driven: &HashSet<String>,
    ) -> Vec<Connection> {
        let ports: HashMap<&str, &Type> = edecl
            .ports
            .iter()
            .map(|p| (p.name.text.as_str(), &p.ty))
            .collect();
        let mut conns = Vec::new();
        let mut connected: HashSet<String> = HashSet::new();

        for (i, arg) in args.iter().enumerate() {
            // Positional args (`Inv { a, b }`) bind by declaration order; named
            // args (`.clk` / `.clk = sig`) bind by name.
            let port = match &arg.field {
                Some(f) => f.text.clone(),
                None => match edecl.ports.get(i) {
                    Some(p) => p.name.text.clone(),
                    None => {
                        self.error(
                            codes::UNKNOWN_NAME,
                            arg.span,
                            format!(
                                "`{}` has {} port(s); positional connection {} is out of range",
                                edecl.name.text,
                                edecl.ports.len(),
                                i + 1
                            ),
                        );
                        continue;
                    }
                },
            };
            let Some(port_ty) = ports.get(port.as_str()) else {
                self.error(
                    codes::UNKNOWN_NAME,
                    arg.span,
                    format!("`{}` has no port `{port}`", edecl.name.text),
                );
                continue;
            };
            // Every arg carries a value — `.port = signal` or positional
            // `signal`. A value-less arg only reaches here on parser recovery
            // (already diagnosed), so skip it.
            let Some(e) = &arg.value else { continue };
            // An `out`/`inout` port *drives* whatever it is connected to, so
            // that has to name something assignable. `.y = 9` produced a
            // connection to a signal literally named "9", which then flowed
            // on in silence — an `in` port takes a value quite legitimately,
            // and nothing distinguished the two.
            if let Some(port_decl) = edecl.ports.iter().find(|p| p.name.text == port) {
                let dir = port_decl.dir;
                if matches!(dir, Some(Direction::Out) | Some(Direction::Inout))
                    && !is_connection_target(e)
                {
                    let dir = if dir == Some(Direction::Out) {
                        "out"
                    } else {
                        "inout"
                    };
                    self.error(
                        codes::INVALID_ASSIGN_TARGET,
                        arg.span,
                        format!(
                            "`{port}` is an `{dir}` port, so it must be connected to a \
                             signal it can drive, not to a value"
                        ),
                    );
                    continue;
                }
            }
            let signal = render_signal(e, render_env);
            let ty = concrete_ty(port_ty, env, &self.families);
            connected.insert(port.clone());
            conns.push(Connection {
                port,
                signal,
                ty,
                span: arg.span,
            });
        }

        for p in &edecl.ports {
            // An `in` port must be driven; an `out`/`inout` port may be left
            // open — its value is still readable as `<instance>.<port>`. A port
            // driven post-declaration (`dut.p = x;`) counts as connected.
            if !connected.contains(&p.name.text)
                && !driven.contains(&p.name.text)
                && p.dir == Some(Direction::In)
            {
                // An unconnected input isn't an error — it holds its default
                // value (§3.29; "always initialized, may be undriven"). Warn so
                // a forgotten connection is still surfaced.
                self.sink.emit(
                    Diagnostic::warning(format!(
                        "input port `{}` of `{}` is not connected; it holds its default value",
                        p.name.text, edecl.name.text
                    ))
                    .with_code(codes::UNCONNECTED_INPUT)
                    .at(site)
                    .help(format!(
                        "connect it with `.{} = <signal>`, or leave it if the default is intended",
                        p.name.text
                    )),
                );
            }
        }
        conns
    }

    /// The concrete types of an entity's own signals (ports + impl-level lets)
    /// with `env` substituted, used to width-check the connections made to its
    /// child instances.
    fn entity_signals(
        &self,
        entity_name: &str,
        env: &HashMap<String, i64>,
    ) -> HashMap<String, EType> {
        let families = &self.families;
        let mut sigs = HashMap::new();
        if let Some(edecl) = self.entities.get(entity_name) {
            for p in &edecl.ports {
                sigs.insert(p.name.text.clone(), concrete_ty(&p.ty, env, families));
            }
        }
        if let Some(impls) = self.impls.get(entity_name) {
            for im in impls {
                for item in &im.items {
                    if let ImplItem::Let(l) = item {
                        if let Some(t) = &l.ty {
                            sigs.insert(l.name.text.clone(), concrete_ty(t, env, families));
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
            let Some(sig) = parent_signals.get(&c.signal) else {
                continue;
            };
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
        self.sink
            .emit(Diagnostic::error(msg).with_code(code).at(span));
    }
}

/// An instance-construction site discovered in an impl body.
struct InstanceSpec<'a> {
    name: String,
    ty: &'a Type,
    args: Vec<ConnectArg>,
    attrs: &'a [Attr],
    site: Span,
    /// Loop-variable bindings for a generated instance (`for i in 0..N`),
    /// substituted into the connection signal names (`wires[i]`).
    loop_env: HashMap<String, i64>,
}

fn is_root(e: &EntityDecl) -> bool {
    e.attrs.iter().any(|a| {
        matches!(
            a.name.segments.last().map(|s| s.text.as_str()),
            Some("top") | Some("test")
        )
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
fn eval_params(
    edecl: &EntityDecl,
    ty: &Type,
    env: &HashMap<String, i64>,
) -> Vec<(String, ParamValue)> {
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

/// The values a `for i in left..right` loop visits. Endpoints are **inclusive and
/// directional**, matching bit slices and array ranges: `0..2` -> 0,1,2 and
/// `2..0` -> 2,1,0. (Kept in sync with `crate::ir::loop_range`, which owns the
/// canonical definition; the crate layering forbids depending on it here.)
fn loop_range(a: i64, b: i64) -> Vec<i64> {
    if a <= b {
        (a..=b).collect()
    } else {
        (b..=a).rev().collect()
    }
}

/// Constant-evaluate a parameter expression (spec 3.3 const exprs), resolving
/// bare identifiers against `env`.
fn eval(e: &Expr, env: &HashMap<String, i64>) -> ParamValue {
    use ParamValue::{Int, Unknown};
    match e {
        Expr::Int { text, .. } => parse_int(text).map(Int).unwrap_or(Unknown),
        Expr::Path(p) if p.segments.len() == 1 => env
            .get(&p.segments[0].text)
            .copied()
            .map(Int)
            .unwrap_or(Unknown),
        Expr::Unary { op, rhs, .. } => match (op, eval(rhs, env)) {
            (UnOp::Neg, Int(v)) => v.checked_neg().map(Int).unwrap_or(Unknown),
            (UnOp::Not, Int(v)) => Int(!v),
            _ => Unknown,
        },
        Expr::Binary { op, lhs, rhs, .. } => match (eval(lhs, env), eval(rhs, env)) {
            (Int(a), Int(b)) => match op {
                BinOp::Add => a.checked_add(b).map(Int).unwrap_or(Unknown),
                BinOp::Sub => a.checked_sub(b).map(Int).unwrap_or(Unknown),
                BinOp::Mul => a.checked_mul(b).map(Int).unwrap_or(Unknown),
                BinOp::Div => a.checked_div(b).map(Int).unwrap_or(Unknown),
                BinOp::Shl => u32::try_from(b)
                    .ok()
                    .and_then(|shift| a.checked_shl(shift))
                    .map(Int)
                    .unwrap_or(Unknown),
                BinOp::Shr => u32::try_from(b)
                    .ok()
                    .and_then(|shift| a.checked_shr(shift))
                    .map(Int)
                    .unwrap_or(Unknown),
                BinOp::And => Int(a & b),
                BinOp::Or => Int(a | b),
                // Comparisons yield 1/0, for `if`-generate conditions.
                BinOp::Eq => Int((a == b) as i64),
                BinOp::Ne => Int((a != b) as i64),
                BinOp::Lt => Int((a < b) as i64),
                BinOp::Le => Int((a <= b) as i64),
                BinOp::Gt => Int((a > b) as i64),
                BinOp::Ge => Int((a >= b) as i64),
                _ => Unknown,
            },
            _ => Unknown,
        },
        _ => Unknown,
    }
}

/// Resolve a port/signal type to a structured [`EType`] with `env` substituted.
fn concrete_ty(t: &Type, env: &HashMap<String, i64>, families: &HashSet<String>) -> EType {
    match t {
        // A bare type name — `integer`, a bit-vector family (`unsigned`), a scalar
        // enum (`Bit`), or a struct — is just its name here (no width; the
        // width check skips it).
        Type::Path(p) => match p.segments.last().map(|s| s.text.as_str()) {
            Some(name) => EType::Named(name.to_string()),
            None => EType::Other(String::new()),
        },
        Type::Indexed { base, index, .. } => {
            let len = index.as_deref().and_then(|i| index_width(i, env));
            // `unsigned[8]` — a bit-vector family indexed *directly* — is a packed
            // array of that many bits, whose element names the family so it
            // renders as `unsigned[8]`. Everything else (`Bit[8]`, `Point[4]`, or a
            // nested `unsigned[8][4]`) is an array of its element type.
            if let Type::Path(p) = base.as_ref() {
                if let Some(name) = p.segments.last().map(|s| s.text.as_str()) {
                    if families.contains(name) {
                        return EType::Array {
                            elem: Box::new(EType::Named(name.to_string())),
                            len,
                        };
                    }
                }
            }
            EType::Array {
                elem: Box::new(concrete_ty(base, env, families)),
                len,
            }
        }
        // Bus-mode and generic types don't carry a simple scalar width; keep a
        // rendered form for display and skip width checking on them.
        Type::Generic { .. } | Type::View { .. } => EType::Other(render_concrete(t, env)),
    }
}

/// The bit width implied by a type index: a single value is the width itself
/// (`unsigned[8]` -> 8); a descending/ascending range is its span (`[31..0]` -> 32).
fn index_width(index: &Expr, env: &HashMap<String, i64>) -> Option<u32> {
    if let Expr::Range { lo, hi, .. } = index {
        if let (ParamValue::Int(a), ParamValue::Int(b)) = (eval(lo, env), eval(hi, env)) {
            return u32::try_from((i128::from(a) - i128::from(b)).unsigned_abs())
                .ok()?
                .checked_add(1);
        }
        return None;
    }
    match eval(index, env) {
        ParamValue::Int(v) if v >= 0 => u32::try_from(v).ok(),
        _ => None,
    }
}

/// Render a port type with parameter widths substituted (`unsigned[W]` with `W=8`
/// becomes `unsigned[8]`; unresolved widths keep their symbolic form).
fn render_concrete(t: &Type, env: &HashMap<String, i64>) -> String {
    match t {
        Type::Path(p) => p
            .segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("::"),
        Type::Indexed { base, index, .. } => match index {
            Some(index) => {
                format!(
                    "{}[{}]",
                    render_concrete(base, env),
                    render_index(index, env)
                )
            }
            None => format!("{}[]", render_concrete(base, env)),
        },
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
        Type::View { view, target, .. } => format!(
            "{} {}",
            view.segments
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join("::"),
            render_concrete(target, env)
        ),
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
        Expr::Path(p) => p
            .segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("::"),
        Expr::Int { text, .. } => text.clone(),
        Expr::Range { lo, hi, .. } => format!("{}..{}", render_expr(lo), render_expr(hi)),
        Expr::Index { base, index, .. } => format!("{}[{}]", render_expr(base), render_expr(index)),
        _ => "?".to_string(),
    }
}

fn parse_int(text: &str) -> Option<i64> {
    let normalized = text.trim().replace('_', "");
    let t = normalized.as_str();
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
/// Collect ports of instance `inst` assigned as `inst.port = ...` anywhere in
/// a statement (walking `if`/`for`/`match` bodies). Used to treat those ports
/// as connected for the missing-connection check (spec 3.12, form 3).
fn collect_field_assign_ports(s: &Stmt, inst: &str, out: &mut HashSet<String>) {
    match s {
        Stmt::Assign {
            target: Expr::Field { base, field, .. },
            ..
        } => {
            if let Expr::Path(p) = base.as_ref() {
                if p.segments.len() == 1 && p.segments[0].text == inst {
                    out.insert(field.text.clone());
                }
            }
        }
        Stmt::If(iff) => {
            for st in &iff.then.stmts {
                collect_field_assign_ports(st, inst, out);
            }
            let mut br = iff.else_.as_deref();
            while let Some(b) = br {
                match b {
                    ElseBranch::Block(blk) => {
                        for st in &blk.stmts {
                            collect_field_assign_ports(st, inst, out);
                        }
                        br = None;
                    }
                    ElseBranch::If(inner) => {
                        for st in &inner.then.stmts {
                            collect_field_assign_ports(st, inst, out);
                        }
                        br = inner.else_.as_deref();
                    }
                }
            }
        }
        Stmt::For { body, .. } => {
            for st in &body.stmts {
                collect_field_assign_ports(st, inst, out);
            }
        }
        Stmt::Match(m) => {
            for arm in &m.arms {
                for st in &arm.body.stmts {
                    collect_field_assign_ports(st, inst, out);
                }
            }
        }
        _ => {}
    }
}

/// Whether an expression names storage a port can drive. Mirrors the shapes
/// `render_signal` can turn into a signal path.
fn is_connection_target(e: &Expr) -> bool {
    match e {
        Expr::Path(_) => true,
        Expr::Index { base, .. } | Expr::Field { base, .. } => is_connection_target(base),
        _ => false,
    }
}

fn render_signal(e: &Expr, env: &HashMap<String, i64>) -> String {
    match e {
        Expr::Path(p) => p
            .segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("::"),
        Expr::Int { text, .. } => text.clone(),
        Expr::CharLit { ch, .. } => format!("'{ch}'"),
        // An indexed connection (`wires[i]`) names the flattened element
        // signal, with the (loop/const) index evaluated.
        Expr::Index { base, index, .. } => {
            let b = render_signal(base, env);
            match eval(index, env) {
                ParamValue::Int(i) => format!("{b}[{i}]"),
                _ => format!("{b}[<expr>]"),
            }
        }
        Expr::Field { base, field, .. } => format!("{}.{}", render_signal(base, env), field.text),
        _ => "<expr>".to_string(),
    }
}

fn type_head_name(ty: &Type) -> Option<&str> {
    match ty {
        Type::Path(p) => p.segments.first().map(|s| s.text.as_str()),
        Type::Generic { base, .. } | Type::Indexed { base, .. } => type_head_name(base),
        Type::View { view, .. } => view.segments.last().map(|i| i.text.as_str()),
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
    use crate::diag::FileId;

    fn elaborate_src(src: &str) -> (Hierarchy, usize) {
        // unsigned/signed are `#[vector]` library types, not seeded.
        let src = format!("{src}\nstruct unsigned(Logic[]);\nstruct signed(Logic[]);\n");
        let src = src.as_str();
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), src, &mut sink);
        assert_eq!(sink.error_count(), 0, "source failed to parse:\n{src}");
        let modules = std::slice::from_ref(&module);
        let resolved = crate::resolve::resolve(modules, &mut sink);
        let typed = crate::types::check(modules, &resolved, &mut sink);
        let before = sink.error_count();
        let hier = elaborate(modules, &typed, &mut sink);
        (hier, sink.error_count() - before)
    }

    /// As `elaborate_src`, but through the root selection `check` uses.
    fn check_src(src: &str) -> usize {
        let src = format!("{src}\nstruct unsigned(Logic[]);\nstruct signed(Logic[]);\n");
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), &src, &mut sink);
        assert_eq!(sink.error_count(), 0, "source failed to parse:\n{src}");
        let modules = std::slice::from_ref(&module);
        let resolved = crate::resolve::resolve(modules, &mut sink);
        let typed = crate::types::check(modules, &resolved, &mut sink);
        let before = sink.error_count();
        elaborate_for_check(modules, &typed, &mut sink);
        sink.error_count() - before
    }

    /// An entity instantiated inside a *behavioural* `if` — one whose
    /// condition tests a signal, i.e. a process — was gathered by nothing and
    /// dropped without a word: the design compiled, ran, and behaved as though
    /// the instance had never been written. Only a generate `if`, whose
    /// condition folds to a constant, may hold one.
    #[test]
    fn an_entity_cannot_be_instantiated_in_a_process() {
        const CELL: &str = "module m;\n\
            entity Cell { i: Bit in, o: Bit out }\n\
            impl Cell { o = i; }\n";

        // A process: the condition tests a signal, so it cannot fold.
        let (_, process) = elaborate_src(&format!(
            "{CELL}#[top] entity Top {{ clk: Bit in, y: Bit out }}\n\
             impl Top {{ y = clk; if clk.rising() {{ let c: Cell = {{ .i = clk }}; }} }}\n"
        ));
        assert_eq!(process, 1, "reported, not silently dropped");

        // Nested inside a process, at depth.
        let (_, nested) = elaborate_src(&format!(
            "{CELL}#[top] entity Top {{ clk: Bit in, y: Bit out }}\n\
             impl Top {{ y = clk; if clk.rising() {{ if 1 == 1 {{ let c: Cell = {{ .i = clk }}; }} }} }}\n"
        ));
        assert_eq!(
            nested, 1,
            "a generate inside a process is still unreachable"
        );

        // The `else` branch of a process counts too.
        let (_, else_arm) = elaborate_src(&format!(
            "{CELL}#[top] entity Top {{ clk: Bit in, y: Bit out }}\n\
             impl Top {{ y = clk; if clk.rising() {{ y = clk; }} else {{ let c: Cell = {{ .i = clk }}; }} }}\n"
        ));
        assert_eq!(else_arm, 1, "the else branch of a process");

        // A generate `if` folds, so its instance is real and legal.
        let (hier, generate) = elaborate_src(&format!(
            "{CELL}#[top] entity Top {{ clk: Bit in, y: Bit out }}\n\
             impl Top {{ y = clk; if 1 == 1 {{ let c: Cell = {{ .i = clk }}; }} }}\n"
        ));
        assert_eq!(generate, 0, "a generate `if` may hold an instance");
        assert!(
            hier.instances.iter().any(|i| i.name.ends_with('c')),
            "and that instance is elaborated"
        );

        // A bare type parameter names data, not an instance, even when an
        // entity shares its name — `gather_instances` already excludes them,
        // and the report has to make the same exclusion or it invents a
        // violation out of `let held: T`.
        let (_, shadowed) = elaborate_src(
            "module m;\nentity T { i: Bit in, o: Bit out }\nimpl T { o = i; }\n\
             entity Buf<T> { clk: Bit in, d: T in, q: T out }\n\
             impl<T> Buf<T> { q = d; if clk.rising() { let held: T; } }\n\
             #[top] entity Top { clk: Bit in, y: Bit out }\n\
             impl Top { let b: Buf<Bit> = { .clk = clk, .d = clk }; y = b.q; }\n",
        );
        assert_eq!(shadowed, 0, "`T` here is the entity's type parameter");
    }

    #[test]
    fn an_out_port_must_connect_to_a_signal() {
        let base = "module m;\n\
            entity Sub { a: Bit in, y: Bit out }\n\
            impl Sub { y = a; }\n\
            entity Top { a: Bit in, y: Bit out }\n\
            impl Top { let d: Sub = { .a = a, .y = CONN }; y = a; }\n";
        // A value there produced a connection to a signal literally named
        // "'1'", which then flowed on in silence.
        assert_eq!(check_src(&base.replace("CONN", "'1'")), 1);
        // Naming a signal for the port to drive is the whole point of the
        // form, and stays legal.
        assert_eq!(check_src(&base.replace("CONN", "y")), 0);
        // An `in` port takes a value quite legitimately.
        let in_port = "module m;\n\
            entity Sub { a: Bit in, y: Bit out }\n\
            impl Sub { y = a; }\n\
            entity Top { y: Bit out }\n\
            impl Top { let d: Sub = { .a = '1' }; y = d.y; }\n";
        assert_eq!(check_src(in_port), 0);
    }

    /// Structural analysis only sees what elaboration reaches, so an entity
    /// written before its first use was never analysed: a misspelled port in
    /// its body reported `check ok`.
    #[test]
    fn check_analyses_an_entity_nothing_instantiates() {
        let src = "module m;\n\
            entity Sub { a: Bit in, y: Bit out }\n\
            impl Sub { y = a; }\n\
            entity Lib { a: Bit in, y: Bit out }\n\
            impl Lib { let d: Sub = { .a = a, .z = a }; y = a; }\n";
        assert_eq!(check_src(src), 1, "the misspelled port is reported");
        // The old root selection reaches neither entity.
        let (_, errors) = elaborate_src(src);
        assert_eq!(errors, 0, "and was not reported before");
    }

    /// An instantiated entity arrives through its parent, so rooting the
    /// unreached ones must not report its contents twice.
    #[test]
    fn check_does_not_double_report_an_instantiated_entity() {
        let src = "module m;\n\
            entity Sub { a: Bit in, y: Bit out }\n\
            impl Sub { let d: Sub2 = { .a = a, .z = a }; y = a; }\n\
            entity Sub2 { a: Bit in, y: Bit out }\n\
            impl Sub2 { y = a; }\n";
        assert_eq!(check_src(src), 1, "reported once, through its parent");
    }

    /// A correct generic entity is elaborated with its parameters unbound.
    /// That must not invent diagnostics for code that is simply unused.
    #[test]
    fn check_is_quiet_about_a_correct_generic_entity() {
        let src = "module m;\n\
            entity Shift<W: integer> { clk: Bit in, d: unsigned[W] in, q: unsigned[W] out }\n\
            impl<W: integer> Shift<W> {\n\
              let r: unsigned[W] = 0;\n\
              if clk.rising() { r = d; }\n\
              q = r;\n\
            }\n";
        assert_eq!(check_src(src), 0);
    }

    const HARNESS: &str = "module m;\n\
        entity Counter<W: integer> {\n\
          clk: Bit in,\n\
          rst: Logic in,\n\
          count: unsigned[W] out,\n\
        }\n\
        impl<W: integer> Counter<W> {\n\
          let value: unsigned[W] = 0;\n\
          count = value;\n\
        }\n\
        #[top]\n\
        entity Harness {}\n\
        impl Harness {\n\
          let clk: Bit = '0';\n\
          let rst: Logic = '1';\n\
          let count: unsigned[8];\n\
          let dut: Counter<W = 8> = {\n\
            .clk = clk,\n\
            .rst = rst,\n\
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
        // Explicit `.clk = clk` / `.count = count` connections resolve by name.
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
    fn unconnected_input_warns_not_errors() {
        // A sub-instance with a forgotten `in` connection holds its default
        // value (§3.29) — a warning (W-P012), not an error.
        let src = "module m;\n\
            entity Sub { a: Bit in, b: Bit in, y: Bit out }\n\
            impl Sub { y = a and b; }\n\
            #[top]\n\
            entity T {}\n\
            impl T {\n\
              let a: Bit = '0';\n\
              let y: Bit;\n\
              let dut: Sub = { .a = a, .y = y };\n\
            }\n\
            struct unsigned(Logic[]);\nstruct signed(Logic[]);\n";
        let mut sink = DiagnosticSink::new();
        let module = crate::syntax::parse_module(FileId(0), src, &mut sink);
        let modules = std::slice::from_ref(&module);
        let resolved = crate::resolve::resolve(modules, &mut sink);
        let typed = crate::types::check(modules, &resolved, &mut sink);
        let before = sink.error_count();
        let _ = elaborate(modules, &typed, &mut sink);
        assert_eq!(
            sink.error_count() - before,
            0,
            "unconnected input must not error"
        );
        let warned = sink
            .diagnostics()
            .iter()
            .any(|d| format!("{:?}", d.code).contains("W-P012"));
        assert!(warned, "unconnected input `b` should warn W-P012");
    }

    #[test]
    fn type_param_named_like_an_entity_is_not_an_instance() {
        // `Buf<T>`'s `let s: T` is data (the bound type `unsigned[8]`), even though
        // the top entity is *also* named `T`. Previously the elaborator treated
        // `s` as an instance of entity `T`, reporting a spurious cyclic
        // instantiation (and IR lowering then recursed forever). `s` must be a
        // signal: `Buf` has no child instances.
        let src = "module m;\n\
            entity Buf<T> { a: T in, y: T out }\n\
            impl Buf<T> {\n\
              let s: T;\n\
              s = a;\n\
              y = s;\n\
            }\n\
            #[top]\n\
            entity T {}\n\
            impl T {\n\
              let a: unsigned[8]; let y: unsigned[8];\n\
              let dut: Buf<unsigned[8]> = { .a = a, .y = y };\n\
            }\n";
        let (hier, errors) = elaborate_src(src);
        assert_eq!(errors, 0, "no cyclic-instantiation error");
        let root = hier.instance(hier.roots[0]);
        assert_eq!(root.entity, "T");
        // The one child is `dut: Buf`; `Buf` itself instantiates nothing.
        assert_eq!(root.children.len(), 1);
        let dut = hier.instance(root.children[0]);
        assert_eq!(dut.entity, "Buf");
        assert!(
            dut.children.is_empty(),
            "`let s: T` must be a signal, not an instance"
        );
    }

    #[test]
    fn tree_string_is_rendered() {
        let (hier, _) = elaborate_src(HARNESS);
        let tree = hier.to_tree_string();
        assert!(tree.contains("Harness"));
        assert!(tree.contains("dut: Counter<W=8>"));
        assert!(tree.contains(".clk: Bit <- clk"));
    }

    #[test]
    fn parameter_widths_are_substituted_into_port_types() {
        let (hier, _) = elaborate_src(HARNESS);
        let root = hier.instance(hier.roots[0]);
        let dut = hier.instance(root.children[0]);
        // `count: unsigned[W]` with W=8 becomes `unsigned[8]` — a bit array (element
        // names the family, length is the bit count).
        let count = dut.connections.iter().find(|c| c.port == "count").unwrap();
        assert_eq!(count.ty.to_string(), "unsigned[8]");
        assert_eq!(count.ty.width(), Some(8));
    }

    #[test]
    fn connection_width_mismatch_is_reported() {
        // Port `a` is unsigned[8] (W=8) but the local signal `a` is unsigned[4].
        let src = "module m;\n\
            entity Sub<W: integer> { a: unsigned[W] in, b: unsigned[W] out }\n\
            impl<W: integer> Sub<W> { b = a; }\n\
            #[top]\n\
            entity Top {}\n\
            impl Top {\n\
              let a: unsigned[4];\n\
              let b: unsigned[8];\n\
              let dut: Sub<W = 8> = { .a = a, .b = b };\n\
            }\n";
        let (_, errors) = elaborate_src(src);
        assert_eq!(errors, 1);
    }

    #[test]
    fn matching_widths_are_fine() {
        let src = "module m;\n\
            entity Sub<W: integer> { a: unsigned[W] in, b: unsigned[W] out }\n\
            impl<W: integer> Sub<W> { b = a; }\n\
            #[top]\n\
            entity Top {}\n\
            impl Top {\n\
              let a: unsigned[8];\n\
              let b: unsigned[8];\n\
              let dut: Sub<W = 8> = { .a = a, .b = b };\n\
            }\n";
        let (_, errors) = elaborate_src(src);
        assert_eq!(errors, 0);
    }

    #[test]
    fn missing_connection_is_reported() {
        // `rst` is left unconnected — a warning (it holds its default), not an
        // error (§3.29). See `unconnected_input_warns_not_errors` for the code.
        let src = "module m;\n\
            entity Counter<W: integer> { clk: Bit in, rst: Logic in, count: unsigned[W] out }\n\
            impl<W: integer> Counter<W> { count = 0; }\n\
            #[top]\n\
            entity H {}\n\
            impl H {\n\
              let clk: Bit = '0';\n\
              let count: unsigned[8];\n\
              let dut: Counter<W = 8> = { .clk = clk, .count = count };\n\
            }\n";
        let (_, errors) = elaborate_src(src);
        assert_eq!(errors, 0, "an unconnected input is a warning, not an error");
    }

    #[test]
    fn unknown_port_is_reported() {
        let src = "module m;\n\
            entity Counter { count: unsigned[8] out }\n\
            impl Counter { count = 0; }\n\
            #[top]\n\
            entity H {}\n\
            impl H {\n\
              let count: unsigned[8];\n\
              let dut: Counter = { .count = count, .nope = count };\n\
            }\n";
        let (_, errors) = elaborate_src(src);
        assert_eq!(errors, 1);
    }

    /// Stage 5 requires every entity parameter to be known after elaboration.
    /// An unbound one produced a signal of unknown width with no diagnostic,
    /// failing much later at the engine. Type parameters are exempt — they
    /// bind to a type, not a number.
    #[test]
    fn value_parameters_must_be_bound_at_instantiation() {
        let base = "module m;\nentity S<W: integer> { y: unsigned[W] out, }\nimpl<W: integer> S<W> { y = 0; }\n\
                    #[top] entity E { y: unsigned[8] out, }\n";
        let (_, errs) = elaborate_src(&format!("{base}impl E {{ let d: S = {{ .y = y }}; }}\n"));
        assert_eq!(errs, 1, "`W` was never given a value");

        let (_, errs) = elaborate_src(&format!(
            "{base}impl E {{ let d: S<W = 8> = {{ .y = y }}; }}\n"
        ));
        assert_eq!(errs, 0, "bound");

        // A *type* parameter has no numeric value and must not be demanded.
        let generic = "module m;\nentity Buf<T> { a: T in, y: T out, }\n\
                       impl Buf<T> { y = a; }\n#[top] entity H {}\n\
                       impl H { let a: unsigned[8]; let y: unsigned[8]; \
                       let d: Buf<unsigned[8]> = { .a = a, .y = y }; }\n";
        let (_, errs) = elaborate_src(generic);
        assert_eq!(errs, 0, "a type parameter is not a value parameter");
    }

    /// A named generic argument that matches no declared parameter used to be
    /// bound anyway, so a typo silently did nothing.
    #[test]
    fn unknown_generic_argument_is_reported() {
        let base = "module m;\nentity S<W: integer> { a: unsigned[W] in, y: unsigned[W] out, }\n\
                    impl<W: integer> S<W> { y = a; }\n#[top] entity E { a: unsigned[8] in, y: unsigned[8] out, }\n";
        let (_, errs) = elaborate_src(&format!(
            "{base}impl E {{ let d: S<W = 8, Z = 3> = {{ .a = a, .y = y }}; }}\n"
        ));
        assert_eq!(errs, 1, "`Z` is not a parameter of `S`");

        let (_, errs) = elaborate_src(&format!(
            "{base}impl E {{ let d: S<W = 8> = {{ .a = a, .y = y }}; }}\n"
        ));
        assert_eq!(errs, 0);
    }

    #[test]
    fn extern_entity_is_a_black_box() {
        let src = "module m;\n\
            extern entity Ram<W: integer> { addr: unsigned[W] in, data: unsigned[8] out }\n\
            #[top]\n\
            entity H {}\n\
            impl H {\n\
              let addr: unsigned[4];\n\
              let data: unsigned[8];\n\
              let mem: Ram<W = 4> = { .addr = addr, .data = data };\n\
            }\n";
        let (hier, errors) = elaborate_src(src);
        assert_eq!(errors, 0);
        let root = hier.instance(hier.roots[0]);
        let mem = hier.instance(root.children[0]);
        assert!(mem.is_extern);
        assert!(mem.children.is_empty());
    }
}
