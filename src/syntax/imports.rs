//! Imports beyond a plain name (language §3.4).
//!
//! The resolver binds plain imports: `use a::b::C;`, groups of them, renamed
//! ones, and `pub use` re-exports. Everything else Rust's import model allows
//! is rewritten here, once, over every loaded module and before resolution,
//! into paths and plain imports the later stages already understand:
//!
//! - `self::X` and `super::X` become absolute paths.
//! - A module import (`use std::math;`, `use a::{self}`, `use m = a::b;`)
//!   is a module alias: `math::PI` becomes `std::math::PI`. A `pub` one is
//!   followed through by importers (`facade::math::PI`).
//! - An enum variant import (`use self::State::{Idle, Busy};`) rewrites
//!   `Idle` to `State::Idle` where it is used: patterns and the later stages
//!   expect exactly `Enum::Variant`. When `State` is not otherwise visible it
//!   is imported unseen; when the name means something else, the full path
//!   is used. `use m::State::{self}` imports the enum itself.
//! - A glob (`use a::*;`) expands into plain imports of every public name of
//!   `a`, or every variant of an enum. A local declaration or an explicit
//!   import shadows a glob name silently; a name two globs both provide is an
//!   error only where it is used.
//! - A `use` inside a block binds for the rest of that block only; a `let`,
//!   parameter or loop variable of the same name shadows it.
//! - A generic alias (`type Pair<T> = Packet<T>;`) is substituted with its
//!   arguments at every use, and then removed, so later stages see only the
//!   type it denotes.
//!
//! Spans are kept: a rewritten path points at what was written, its added
//! qualifiers at an empty span just before it, so diagnostics still point at
//! the source and later stages, which resolve by span, tell them apart.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::diag::{codes, Diagnostic, DiagnosticSink, Span};
use crate::syntax::ast::*;

/// What kind of declaration a name is, as far as imports care.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Enum,
    Alias,
    Other,
}

/// One declaration in the index.
#[derive(Clone)]
struct Decl {
    kind: Kind,
    variants: Vec<String>,
}

/// A generic alias: its parameters and the type they appear in.
#[derive(Clone)]
struct GenericAlias {
    params: Vec<String>,
    ty: Type,
    /// A struct whose parameters shape its base's index range (see
    /// [`format_struct_alias`]): its bare name is the family itself, and
    /// importing it imports the struct.
    structure: bool,
}

/// A struct whose integer parameters only shape its base's index *range* —
/// `struct float<W: integer, M: integer>(Logic[W - M - 1 .. 0 - M]);` — is
/// that family over the range: `float<32, 23>` stands for `float[8..-23]`,
/// and the struct itself is the plain family `float(Logic[])`. A base sized
/// by a width (`Word<N>(Logic[N])`) keeps its ordinary generic meaning.
fn format_struct_alias(s: &StructDecl) -> Option<GenericAlias> {
    if s.params.params.is_empty() || !s.fields.is_empty() {
        return None;
    }
    let integer_bound = |param: &Param| {
        matches!(&param.bound, Some(Type::Path(path))
            if path.segments.last().is_some_and(|last| last.text == "integer"))
    };
    if !s.params.params.iter().all(integer_bound) {
        return None;
    }
    let Some(Type::Indexed {
        index: Some(index),
        span,
        ..
    }) = &s.base
    else {
        return None;
    };
    if !matches!(index.as_ref(), Expr::Range { .. }) {
        return None;
    }
    Some(GenericAlias {
        params: s
            .params
            .params
            .iter()
            .map(|p| p.name.text.clone())
            .collect(),
        ty: Type::Indexed {
            base: Box::new(Type::Path(Path {
                segments: vec![s.name.clone()],
                span: s.name.span,
            })),
            index: Some(index.clone()),
            span: *span,
        },
        structure: true,
    })
}

/// Every loaded module's declarations, public names and generic aliases.
#[derive(Default)]
struct Index {
    modules: HashSet<String>,
    decls: HashMap<(String, String), Decl>,
    public: HashMap<String, Vec<String>>,
    generic: HashMap<(String, String), GenericAlias>,
    /// Non-generic aliases (`type Word = unsigned[8];`) and what they name,
    /// for their use as a constructor (`Word(x)`).
    plain: HashMap<(String, String), Type>,
    /// `pub use a::b;` where `a::b` is a module: `(module, local) -> a::b`.
    module_exports: HashMap<(String, String), Vec<String>>,
}

/// What a name rewrites to in one scope.
#[derive(Clone, Default)]
struct Env {
    /// Module aliases: `math` -> `std::math`.
    aliases: HashMap<String, Vec<String>>,
    /// Names that rewrite to a full path: variant imports, block imports.
    names: HashMap<String, Vec<String>>,
    /// Generic aliases by local name: `Pair` -> `(module, Pair)`.
    generic: HashMap<String, (String, String)>,
    /// Names two globs both provide: each glob's module.
    ambiguous: HashMap<String, Vec<String>>,
    /// Names a local declaration, parameter or `let` shadows.
    shadow: HashSet<String>,
    /// Item names visible at module level, and what they name:
    /// `Color` -> `shapes::colors::Color`.
    items: HashMap<String, String>,
}

/// Rewrite every import form beyond a plain name; see the module docs.
pub fn desugar(modules: &mut [Module], sink: &mut DiagnosticSink) {
    let mut index = index(modules);
    let mut envs = Vec::with_capacity(modules.len());
    for module in modules.iter_mut() {
        envs.push(plan_module(module, &mut index, sink));
    }
    // Every module's scope, for an alias used as a constructor elsewhere:
    // its type is read where it was written.
    let scopes: HashMap<String, Env> = modules
        .iter()
        .zip(&envs)
        .map(|(module, env)| (path_segments(&module.path).join("::"), env.clone()))
        .collect();
    for (module, env) in modules.iter_mut().zip(envs) {
        let here = path_segments(&module.path);
        let rewriter = Rewriter {
            generated: false,
            scopes: &scopes,
            index: &index,
            module: here,
            hidden: RefCell::default(),
        };
        rewriter.items(&mut module.items, &env, sink);
        let mut hidden: Vec<_> = rewriter.hidden.into_inner().into_iter().collect();
        hidden.sort();
        for (enum_name, module_path) in hidden {
            let span = module.path.span;
            module.items.push(plain_import(
                &module_path,
                ident(&enum_name, span),
                None,
                false,
                true,
                span,
            ));
        }
    }
    // Generic aliases are fully substituted; later stages never see them.
    for module in modules.iter_mut() {
        module.items.retain(|item| {
            !matches!(item, Item::Using(Using { kind: UsingKind::Alias { params, .. }, .. })
                if !params.params.is_empty())
        });
    }
}

/// `a::b` as segment strings.
fn path_segments(path: &Path) -> Vec<String> {
    path.segments.iter().map(|s| s.text.clone()).collect()
}

fn ident(text: &str, span: Span) -> Ident {
    Ident {
        text: text.to_string(),
        span,
    }
}

/// Build the index of every loaded module.
fn index(modules: &[Module]) -> Index {
    let mut index = Index::default();
    for module in modules {
        let here = path_segments(&module.path).join("::");
        index.modules.insert(here.clone());
        let add =
            |name: &Ident, kind: Kind, is_pub: bool, variants: Vec<String>, index: &mut Index| {
                index
                    .decls
                    .insert((here.clone(), name.text.clone()), Decl { kind, variants });
                if is_pub {
                    index
                        .public
                        .entry(here.clone())
                        .or_default()
                        .push(name.text.clone());
                }
            };
        for item in &module.items {
            match item {
                Item::Enum(e) => {
                    let variants = e.variants.iter().map(|v| v.name.text.clone()).collect();
                    add(&e.name, Kind::Enum, e.is_pub, variants, &mut index);
                }
                Item::Struct(s) => {
                    add(&s.name, Kind::Other, s.is_pub, Vec::new(), &mut index);
                    if let Some(alias) = format_struct_alias(s) {
                        index
                            .generic
                            .insert((here.clone(), s.name.text.clone()), alias);
                    }
                }
                Item::View(v) => add(&v.name, Kind::Other, v.is_pub, Vec::new(), &mut index),
                Item::Entity(e) => add(&e.name, Kind::Other, e.is_pub, Vec::new(), &mut index),
                Item::Trait(t) => add(&t.name, Kind::Other, t.is_pub, Vec::new(), &mut index),
                Item::Fn(f) => add(&f.name, Kind::Other, f.is_pub, Vec::new(), &mut index),
                Item::Const(c) => add(&c.name, Kind::Other, c.is_pub, Vec::new(), &mut index),
                Item::AttrDecl(a) => add(&a.name, Kind::Other, a.is_pub, Vec::new(), &mut index),
                Item::ExternBlock { fns, .. } => {
                    for f in fns {
                        add(&f.name, Kind::Other, f.is_pub, Vec::new(), &mut index);
                    }
                }
                Item::Using(u) => match &u.kind {
                    UsingKind::Alias { name, params, ty } => {
                        add(name, Kind::Alias, u.is_pub, Vec::new(), &mut index);
                        if params.params.is_empty() {
                            index
                                .plain
                                .insert((here.clone(), name.text.clone()), ty.clone());
                        } else {
                            index.generic.insert(
                                (here.clone(), name.text.clone()),
                                GenericAlias {
                                    params: params
                                        .params
                                        .iter()
                                        .map(|p| p.name.text.clone())
                                        .collect(),
                                    ty: ty.clone(),
                                    structure: false,
                                },
                            );
                        }
                    }
                    UsingKind::Import { names, .. } if u.is_pub => {
                        for n in names.iter().filter(|n| !n.glob && n.name.text != "self") {
                            index
                                .public
                                .entry(here.clone())
                                .or_default()
                                .push(n.binding().text.clone());
                        }
                    }
                    UsingKind::Import { .. } => {}
                },
                Item::Impl(_) | Item::AttrBinding(_) | Item::Macro(_) | Item::MacroCall { .. } => {}
            }
        }
    }
    index
}

/// Make `self::`/`super::` paths absolute against `module`. `None` with a
/// diagnostic when `super` climbs above the root.
fn absolute_head(
    segments: &[Ident],
    module: &[String],
    sink: &mut DiagnosticSink,
) -> Option<Vec<Ident>> {
    let mut base: Vec<String> = Vec::new();
    let mut rest = segments;
    let mut relative = false;
    if let Some(first) = segments.first() {
        if first.text == "self" && segments.len() > 1 {
            base = module.to_vec();
            rest = &segments[1..];
            relative = true;
        } else if first.text == "super" {
            base = module.to_vec();
            relative = true;
            while rest.first().is_some_and(|s| s.text == "super") {
                if base.pop().is_none() {
                    sink.emit(
                        Diagnostic::error("`super` goes above the root module")
                            .with_code(codes::UNRESOLVED_IMPORT)
                            .at(rest[0].span),
                    );
                    return None;
                }
                rest = &rest[1..];
            }
        }
    }
    if !relative {
        return Some(segments.to_vec());
    }
    let span = segments[0].span;
    let mut out: Vec<Ident> = base.iter().map(|s| ident(s, span)).collect();
    out.extend(rest.iter().cloned());
    Some(out)
}

/// Replace a leading module alias, then follow `pub use` module re-exports
/// along the path (`facade::math::PI` -> `std::math::PI`).
fn expand_aliases(segments: Vec<Ident>, env: &Env, index: &Index) -> Vec<Ident> {
    let mut segments = segments;
    if segments.len() >= 2 && !env.shadow.contains(&segments[0].text) {
        if let Some(target) = env.aliases.get(&segments[0].text) {
            let span = segments[0].span;
            let mut out: Vec<Ident> = target.iter().map(|s| ident(s, span)).collect();
            out.extend(segments.drain(1..));
            segments = out;
        }
    }
    // Follow re-exported module aliases, at most a few hops.
    for _ in 0..8 {
        let mut changed = false;
        for split in 1..segments.len() {
            let module = segments[..split]
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join("::");
            if let Some(target) = index
                .module_exports
                .get(&(module, segments[split].text.clone()))
            {
                let span = segments[split].span;
                let mut out: Vec<Ident> = target.iter().map(|s| ident(s, span)).collect();
                out.extend(segments.drain(split + 1..));
                segments = out;
                changed = true;
                break;
            }
        }
        if !changed {
            break;
        }
    }
    segments
}

/// Classify one module's top-level imports, rewrite their bases to absolute
/// paths, expand its globs, and return the rewrites its bodies need.
fn plan_module(module: &mut Module, index: &mut Index, sink: &mut DiagnosticSink) -> Env {
    let here = path_segments(&module.path);
    let here_str = here.join("::");
    let mut env = Env::default();
    let locals: HashSet<String> = index
        .decls
        .keys()
        .filter(|(m, _)| *m == here_str)
        .map(|(_, n)| n.clone())
        .collect();

    // Pass 1: absolute bases, and the module aliases (which later bases may use).
    for item in module.items.iter_mut() {
        let Item::Using(u) = item else { continue };
        let UsingKind::Import { base, names } = &mut u.kind else {
            continue;
        };
        if let Some(abs) = absolute_head(&base.segments, &here, sink) {
            base.segments = abs;
        }
        for n in names.iter() {
            let target = full_path(base, n);
            let owner_is_module = index
                .modules
                .contains(&target[..target.len() - 1].join("::"));
            let is_module = (n.name.text == "self" && owner_is_module)
                || (n.name.text != "self" && index.modules.contains(&target.join("::")));
            if is_module && !n.glob {
                let module_path = if n.name.text == "self" {
                    target[..target.len() - 1].to_vec()
                } else {
                    target.clone()
                };
                let local = n
                    .local
                    .as_ref()
                    .map(|l| l.text.clone())
                    .unwrap_or_else(|| module_path.last().cloned().unwrap_or_default());
                if !index.modules.contains(&module_path.join("::")) {
                    sink.emit(
                        Diagnostic::error(format!(
                            "`self` in an import names a module, and `{}` is not a loaded module",
                            module_path.join("::")
                        ))
                        .with_code(codes::UNRESOLVED_IMPORT)
                        .at(n.name.span),
                    );
                    continue;
                }
                if u.is_pub {
                    index
                        .module_exports
                        .insert((here_str.clone(), local.clone()), module_path.clone());
                }
                env.aliases.insert(local, module_path);
            }
        }
    }

    // Pass 2: everything else, with aliases applied to the remaining bases.
    let mut globs: Vec<(Vec<String>, bool, Span)> = Vec::new();
    let mut explicit: HashSet<String> = locals.clone();
    explicit.extend(env.aliases.keys().cloned());
    for local in &locals {
        env.items
            .insert(local.clone(), format!("{here_str}::{local}"));
    }
    // `(local, owner module, enum, variant)` for every imported variant.
    let mut imported: Vec<(String, String, String, String, Span)> = Vec::new();
    // `use m::E::{self}`: the enum itself, as a plain import.
    let mut enum_imports: Vec<(Vec<String>, Ident, Option<Ident>, bool)> = Vec::new();
    for item in module.items.iter_mut() {
        let Item::Using(u) = item else { continue };
        let is_pub = u.is_pub;
        let UsingKind::Import { base, names } = &mut u.kind else {
            continue;
        };
        let expanded = expand_aliases(std::mem::take(&mut base.segments), &env, index);
        base.segments = expanded;
        names.retain(|n| {
            let target = full_path(base, n);
            if n.glob {
                globs.push((target[..target.len() - 1].to_vec(), is_pub, n.name.span));
                return false;
            }
            if n.name.text == "self" {
                // `self` names the module (an alias, recorded in pass 1) or,
                // as in Rust, an enum the group's other leaves come from.
                let owner = &target[..target.len() - 1];
                if let Some((enum_name, module_path)) = owner.split_last() {
                    if index.decls.get(&(module_path.join("::"), enum_name.clone())).map(|d| d.kind)
                        == Some(Kind::Enum)
                    {
                        let local = n.local.as_ref().map_or(enum_name.clone(), |l| l.text.clone());
                        explicit.insert(local.clone());
                        env.items.insert(local, owner.join("::"));
                        enum_imports.push((
                            module_path.to_vec(),
                            ident(enum_name, n.name.span),
                            n.local.clone(),
                            is_pub,
                        ));
                    } else if !index.modules.contains(&owner.join("::")) {
                        sink.emit(
                            Diagnostic::error(format!(
                                "`self` in an import names a module or an enum, and `{}` is neither",
                                owner.join("::")
                            ))
                            .with_code(codes::UNRESOLVED_IMPORT)
                            .at(n.name.span),
                        );
                    }
                }
                return false;
            }
            if index.modules.contains(&target.join("::")) {
                return false; // a module alias, recorded in pass 1
            }
            let local = n.binding().text.clone();
            let owner = target[..target.len() - 1].to_vec();
            if let Some((module_path, enum_name)) =
                owner.split_last().map(|(l, m)| (m.join("::"), l.clone()))
            {
                match index
                    .decls
                    .get(&(module_path.clone(), enum_name.clone()))
                    .map(|d| d.kind)
                {
                    Some(Kind::Enum) => {
                        let known = &index.decls[&(module_path.clone(), enum_name.clone())].variants;
                        if !known.contains(&n.name.text) {
                            sink.emit(
                                Diagnostic::error(format!(
                                    "`{}` is not a variant of enum `{enum_name}`",
                                    n.name.text
                                ))
                                .with_code(codes::UNRESOLVED_IMPORT)
                                .at(n.name.span),
                            );
                        } else {
                            explicit.insert(local.clone());
                            imported.push((local, module_path, enum_name.clone(), n.name.text.clone(), n.name.span));
                        }
                        return false;
                    }
                    Some(Kind::Alias) => {
                        sink.emit(
                            Diagnostic::error(format!(
                                "an import cannot pass through the type alias `{enum_name}`"
                            ))
                            .with_code(codes::UNRESOLVED_IMPORT)
                            .at(n.name.span)
                            .help("import from the type the alias names"),
                        );
                        return false;
                    }
                    _ => {}
                }
            }
            let owner_str = owner.join("::");
            if let Some(alias) = index.generic.get(&(owner_str.clone(), n.name.text.clone())) {
                explicit.insert(local.clone());
                env.generic
                    .insert(local.clone(), (owner_str, n.name.text.clone()));
                if !alias.structure {
                    return false;
                }
            }
            explicit.insert(local.clone());
            env.items.insert(local, target.join("::"));
            true
        });
    }

    // Globs: expand into plain imports, shadowed by anything explicit.
    let mut candidates: HashMap<String, Vec<(Vec<String>, bool, Span)>> = HashMap::new();
    for (target, is_pub, span) in &globs {
        let target_str = target.join("::");
        let names: Vec<String> = if index.modules.contains(&target_str) {
            index.public.get(&target_str).cloned().unwrap_or_default()
        } else if let Some((name, module_path)) = target.split_last() {
            match index.decls.get(&(module_path.join("::"), name.clone())) {
                Some(d) if d.kind == Kind::Enum => d.variants.clone(),
                _ => {
                    sink.emit(
                        Diagnostic::error(format!("`{target_str}::*` names no module or enum"))
                            .with_code(codes::UNRESOLVED_IMPORT)
                            .at(*span),
                    );
                    continue;
                }
            }
        } else {
            continue;
        };
        for name in names {
            if explicit.contains(&name) {
                continue;
            }
            let entry = candidates.entry(name).or_default();
            if !entry.iter().any(|(t, _, _)| t == target) {
                entry.push((target.clone(), *is_pub, *span));
            }
        }
    }
    let mut synthesized: Vec<Item> = Vec::new();
    let mut sorted: Vec<_> = candidates.into_iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, sources) in sorted {
        if sources.len() > 1 {
            env.ambiguous
                .insert(name, sources.iter().map(|(t, _, _)| t.join("::")).collect());
            continue;
        }
        let (target, is_pub, span) = &sources[0];
        let target_str = target.join("::");
        if !index.modules.contains(&target_str) {
            // An enum glob: its variants rewrite like variant imports.
            if let Some((enum_name, module_path)) = target.split_last() {
                imported.push((
                    name.clone(),
                    module_path.join("::"),
                    enum_name.clone(),
                    name,
                    *span,
                ));
            }
            continue;
        }
        if let Some(module_path) = index
            .module_exports
            .get(&(target_str.clone(), name.clone()))
        {
            env.aliases.insert(name, module_path.clone());
            continue;
        }
        if let Some(alias) = index.generic.get(&(target_str.clone(), name.clone())) {
            env.generic
                .insert(name.clone(), (target_str.clone(), name.clone()));
            if !alias.structure {
                continue;
            }
        }
        env.items
            .insert(name.clone(), format!("{target_str}::{name}"));
        synthesized.push(plain_import(
            &target_str,
            ident(&name, *span),
            None,
            *is_pub,
            true,
            *span,
        ));
    }
    for (module_path, enum_name, local, is_pub) in enum_imports {
        let span = enum_name.span;
        synthesized.push(plain_import(
            &module_path.join("::"),
            enum_name,
            local,
            is_pub,
            false,
            span,
        ));
    }
    for (local, module_path, enum_name, variant, span) in imported {
        let (target, hide) = variant_target(&module_path, &enum_name, &variant, &mut env.items);
        if hide {
            synthesized.push(plain_import(
                &module_path,
                ident(&enum_name, span),
                None,
                false,
                true,
                span,
            ));
        }
        env.names.insert(local, target);
    }
    module.items.extend(synthesized);
    env
}

/// The path an imported variant rewrites to. Patterns and the later stages
/// expect `Enum::Variant`, so that is the form whenever `Enum` names this
/// enum; when the name is free, the second result asks for an unseen,
/// lint-free import of the enum. A name taken by something else leaves the
/// full path.
fn variant_target(
    module_path: &str,
    enum_name: &str,
    variant: &str,
    items: &mut HashMap<String, String>,
) -> (Vec<String>, bool) {
    let full = format!("{module_path}::{enum_name}");
    let short = vec![enum_name.to_string(), variant.to_string()];
    match items.get(enum_name) {
        Some(existing) if *existing == full => (short, false),
        Some(_) => {
            let mut path: Vec<String> = module_path.split("::").map(str::to_string).collect();
            path.extend(short);
            (path, false)
        }
        None => {
            items.insert(enum_name.to_string(), full);
            (short, true)
        }
    }
}

/// A plain `use module::name;` item.
fn plain_import(
    module_path: &str,
    name: Ident,
    local: Option<Ident>,
    is_pub: bool,
    expanded: bool,
    span: Span,
) -> Item {
    Item::Using(Using {
        is_pub,
        kind: UsingKind::Import {
            base: Path {
                segments: module_path.split("::").map(|s| ident(s, span)).collect(),
                span,
            },
            names: vec![ImportName {
                via: Vec::new(),
                name,
                local,
                glob: false,
                expanded,
            }],
        },
        span,
    })
}

/// `base::via::name` as segment strings.
fn full_path(base: &Path, n: &ImportName) -> Vec<String> {
    base.segments
        .iter()
        .chain(n.via.iter())
        .map(|s| s.text.clone())
        .chain(std::iter::once(n.name.text.clone()))
        .collect()
}

/// Rewrites the paths of one module.
struct Rewriter<'a> {
    index: &'a Index,
    /// Every module's scope, by module path.
    scopes: &'a HashMap<String, Env>,
    module: Vec<String>,
    /// Rewriting a substituted alias body: its `float[8..-23]` is the
    /// expansion of `float<32, 23>`, not a range the user wrote.
    generated: bool,
    /// Enums a block-level variant import needs visible: name -> module.
    hidden: RefCell<HashMap<String, String>>,
}

impl Rewriter<'_> {
    fn items(&self, items: &mut [Item], env: &Env, sink: &mut DiagnosticSink) {
        for item in items.iter_mut() {
            match item {
                Item::Using(u) => {
                    if let UsingKind::Alias { ty, params, .. } = &mut u.kind {
                        if params.params.is_empty() {
                            self.ty(ty, env, sink);
                        }
                    }
                }
                Item::Const(c) => {
                    self.ty(&mut c.ty, env, sink);
                    self.expr(&mut c.value, env, sink);
                }
                Item::Fn(f) => self.function(f, env, sink),
                Item::ExternBlock { fns, .. } => {
                    for f in fns {
                        self.function(f, env, sink);
                    }
                }
                Item::Struct(s) => {
                    // A format struct is the plain family; its parameters
                    // live on as the alias `float<W, M>` (`format_struct_alias`).
                    if format_struct_alias(s).is_some() {
                        s.params.params.clear();
                        if let Some(Type::Indexed { index, .. }) = &mut s.base {
                            *index = None;
                        }
                    }
                    self.params(&mut s.params, env, sink);
                    if let Some(base) = &mut s.base {
                        self.ty(base, env, sink);
                    }
                    for field in &mut s.fields {
                        self.ty(&mut field.ty, env, sink);
                    }
                }
                Item::View(v) => {
                    self.params(&mut v.params, env, sink);
                    self.ty(&mut v.target, env, sink);
                }
                Item::Enum(e) => {
                    if let Some(repr) = &mut e.repr {
                        self.ty(repr, env, sink);
                    }
                    for variant in &mut e.variants {
                        if let Some(value) = &mut variant.value {
                            self.expr(value, env, sink);
                        }
                    }
                }
                Item::Entity(e) => {
                    self.params(&mut e.params, env, sink);
                    for port in &mut e.ports {
                        self.ty(&mut port.ty, env, sink);
                    }
                }
                Item::Impl(im) => {
                    let mut env = env.clone();
                    for member in &im.items {
                        match member {
                            ImplItem::Let(l) => {
                                env.shadow.insert(l.name.text.clone());
                            }
                            ImplItem::Fn(f) => {
                                env.shadow.insert(f.name.text.clone());
                            }
                            _ => {}
                        }
                    }
                    self.params(&mut im.params, &env, sink);
                    if let Some(trait_) = &mut im.trait_ {
                        self.path(trait_, &env, sink);
                    }
                    for arg in &mut im.trait_args {
                        self.generic_arg(arg, &env, sink);
                    }
                    self.ty(&mut im.target, &env, sink);
                    for member in &mut im.items {
                        match member {
                            ImplItem::Const(c) => {
                                self.ty(&mut c.ty, &env, sink);
                                self.expr(&mut c.value, &env, sink);
                            }
                            ImplItem::Let(l) => {
                                if let Some(ty) = &mut l.ty {
                                    self.ty(ty, &env, sink);
                                }
                                if let Some(value) = &mut l.value {
                                    self.expr(value, &env, sink);
                                }
                            }
                            ImplItem::Fn(f) => self.function(f, &env, sink),
                            ImplItem::Process(p) => self.block(&mut p.body, &env, sink),
                            ImplItem::Stmt(s) => {
                                let mut scope = env.clone();
                                self.stmt(s, &mut scope, sink);
                            }
                            ImplItem::AttrBinding(b) => self.expr(&mut b.value, &env, sink),
                            ImplItem::Attr(a) => {
                                self.ty(&mut a.ty, &env, sink);
                                if let Some(value) = &mut a.default {
                                    self.expr(value, &env, sink);
                                }
                            }
                            ImplItem::ModeField { .. } => {}
                        }
                    }
                }
                Item::Trait(t) => {
                    self.params(&mut t.params, env, sink);
                    for f in &mut t.items {
                        self.function(f, env, sink);
                    }
                }
                Item::AttrDecl(a) => {
                    self.ty(&mut a.ty, env, sink);
                    if let Some(default) = &mut a.default {
                        self.expr(default, env, sink);
                    }
                }
                Item::AttrBinding(b) => self.expr(&mut b.value, env, sink),
                Item::Macro(_) | Item::MacroCall { .. } => {}
            }
        }
    }

    fn function(&self, f: &mut FnDecl, env: &Env, sink: &mut DiagnosticSink) {
        let mut env = env.clone();
        self.params(&mut f.generics, &env, sink);
        for p in &mut f.params {
            if let Some(ty) = &mut p.ty {
                self.ty(ty, &env, sink);
            }
            if let Some(name) = &p.name {
                env.shadow.insert(name.text.clone());
            }
        }
        if let Some(ret) = &mut f.ret {
            self.ty(ret, &env, sink);
        }
        if let Some(body) = &mut f.body {
            self.block(body, &env, sink);
        }
    }

    fn params(&self, params: &mut Params, env: &Env, sink: &mut DiagnosticSink) {
        for p in &mut params.params {
            if let Some(bound) = &mut p.bound {
                self.ty(bound, env, sink);
            }
        }
    }

    fn block(&self, block: &mut Block, env: &Env, sink: &mut DiagnosticSink) {
        let mut scope = env.clone();
        for statement in &mut block.stmts {
            self.stmt(statement, &mut scope, sink);
        }
    }

    /// One statement. A `let` or a `use` changes the scope for the
    /// statements after it, which is why `env` is mutable here.
    fn stmt(&self, statement: &mut Stmt, env: &mut Env, sink: &mut DiagnosticSink) {
        match statement {
            Stmt::Use(u) => self.block_use(u, env, sink),
            Stmt::Let(l) => {
                if let Some(ty) = &mut l.ty {
                    self.ty(ty, env, sink);
                }
                if let Some(value) = &mut l.value {
                    self.expr(value, env, sink);
                }
                env.shadow.insert(l.name.text.clone());
                env.names.remove(&l.name.text);
            }
            Stmt::Assign {
                target,
                value,
                after,
                ..
            } => {
                self.expr(target, env, sink);
                self.expr(value, env, sink);
                if let Some(after) = after {
                    self.expr(after, env, sink);
                }
            }
            Stmt::If(iff) => self.if_stmt(iff, env, sink),
            Stmt::Match(m) => {
                self.expr(&mut m.scrutinee, env, sink);
                for arm in &mut m.arms {
                    self.pattern(&mut arm.pattern, env, sink);
                    self.block(&mut arm.body, env, sink);
                }
            }
            Stmt::For {
                var, range, body, ..
            } => {
                self.expr(range, env, sink);
                let mut inner = env.clone();
                inner.shadow.insert(var.text.clone());
                inner.names.remove(&var.text);
                self.block(body, &inner, sink);
            }
            Stmt::Expr(e) => self.expr(e, env, sink),
            Stmt::Return { value, .. } => {
                if let Some(value) = value {
                    self.expr(value, env, sink);
                }
            }
        }
    }

    fn if_stmt(&self, iff: &mut IfStmt, env: &Env, sink: &mut DiagnosticSink) {
        self.expr(&mut iff.cond, env, sink);
        self.block(&mut iff.then, env, sink);
        match iff.else_.as_deref_mut() {
            Some(ElseBranch::Block(b)) => self.block(b, env, sink),
            Some(ElseBranch::If(inner)) => self.if_stmt(inner, env, sink),
            None => {}
        }
    }

    /// A `use` inside a block: every name it binds rewrites to its full path
    /// for the rest of the block.
    fn block_use(&self, u: &mut Using, env: &mut Env, sink: &mut DiagnosticSink) {
        let UsingKind::Import { base, names } = &mut u.kind else {
            return;
        };
        let Some(abs) = absolute_head(&base.segments, &self.module, sink) else {
            return;
        };
        base.segments = expand_aliases(abs, env, self.index);
        for n in names.iter() {
            let target = full_path(base, n);
            let target_str = target.join("::");
            if n.glob {
                let owner = target[..target.len() - 1].to_vec();
                let owner_str = owner.join("::");
                let glob_names = if self.index.modules.contains(&owner_str) {
                    self.index
                        .public
                        .get(&owner_str)
                        .cloned()
                        .unwrap_or_default()
                } else {
                    owner
                        .split_last()
                        .and_then(|(l, m)| self.index.decls.get(&(m.join("::"), l.clone())))
                        .map(|d| d.variants.clone())
                        .unwrap_or_default()
                };
                let is_enum = !self.index.modules.contains(&owner_str);
                for name in glob_names {
                    if env.shadow.contains(&name) || env.names.contains_key(&name) {
                        continue;
                    }
                    let full = match owner.split_last() {
                        Some((enum_name, module_path)) if is_enum => {
                            self.variant(&module_path.join("::"), enum_name, &name, env)
                        }
                        _ => [owner.clone(), vec![name.clone()]].concat(),
                    };
                    env.names.insert(name, full);
                }
                continue;
            }
            let local = n.binding().text.clone();
            let owner = &target[..target.len() - 1];
            if let Some((enum_name, module_path)) = owner.split_last() {
                let module_str = module_path.join("::");
                if self
                    .index
                    .decls
                    .get(&(module_str.clone(), enum_name.clone()))
                    .map(|d| d.kind)
                    == Some(Kind::Enum)
                {
                    if n.name.text == "self" {
                        // The enum itself: `Local::X` reads as `Enum::X`.
                        let local = n
                            .local
                            .as_ref()
                            .map_or(enum_name.clone(), |l| l.text.clone());
                        let short = self.variant(&module_str, enum_name, "", env);
                        env.shadow.remove(&local);
                        env.aliases.insert(local, short[..short.len() - 1].to_vec());
                    } else {
                        let path = self.variant(&module_str, enum_name, &n.name.text, env);
                        env.shadow.remove(&local);
                        env.names.insert(local, path);
                    }
                    continue;
                }
            }
            if n.name.text == "self" || self.index.modules.contains(&target_str) {
                let module_path = if n.name.text == "self" {
                    target[..target.len() - 1].to_vec()
                } else {
                    target
                };
                let local = n
                    .local
                    .as_ref()
                    .map(|l| l.text.clone())
                    .unwrap_or_else(|| module_path.last().cloned().unwrap_or_default());
                env.aliases.insert(local, module_path);
                continue;
            }
            let owner = target[..target.len() - 1].join("::");
            if let Some(alias) = self
                .index
                .generic
                .get(&(owner.clone(), n.name.text.clone()))
            {
                env.generic
                    .insert(local.clone(), (owner, n.name.text.clone()));
                if !alias.structure {
                    continue;
                }
            }
            env.shadow.remove(&local);
            env.names.insert(local, target);
        }
    }

    /// A variant imported inside a block, as [`variant_target`] spells it;
    /// the enum, when it must become visible, is imported at module level.
    fn variant(
        &self,
        module_path: &str,
        enum_name: &str,
        variant: &str,
        env: &mut Env,
    ) -> Vec<String> {
        let mut hidden = self.hidden.borrow_mut();
        if let Some(full) = hidden.get(enum_name) {
            env.items
                .insert(enum_name.to_string(), format!("{full}::{enum_name}"));
        }
        if env.shadow.contains(enum_name) {
            return [module_path, enum_name, variant]
                .iter()
                .map(|s| s.to_string())
                .collect();
        }
        let (path, hide) = variant_target(module_path, enum_name, variant, &mut env.items);
        if hide {
            hidden.insert(enum_name.to_string(), module_path.to_string());
        }
        path
    }

    fn pattern(&self, pattern: &mut Pattern, env: &Env, sink: &mut DiagnosticSink) {
        match pattern {
            Pattern::Path(p) => self.path(p, env, sink),
            Pattern::Or { alts, .. } => {
                for alt in alts {
                    self.pattern(alt, env, sink);
                }
            }
            Pattern::Bounds { lo, hi, .. } => {
                for bound in [lo, hi].into_iter().flatten() {
                    self.expr(bound, env, sink);
                }
            }
            Pattern::Wildcard
            | Pattern::BitPattern { .. }
            | Pattern::Range { .. }
            | Pattern::CharLit { .. } => {}
        }
    }

    fn generic_arg(&self, arg: &mut GenericArg, env: &Env, sink: &mut DiagnosticSink) {
        match arg {
            GenericArg::Positional(e) | GenericArg::Named { value: e, .. } => {
                self.expr(e, env, sink)
            }
            GenericArg::PositionalType(t) | GenericArg::NamedType { ty: t, .. } => {
                self.ty(t, env, sink)
            }
        }
    }

    fn expr(&self, expression: &mut Expr, env: &Env, sink: &mut DiagnosticSink) {
        match expression {
            Expr::Int { .. }
            | Expr::SuffixLit { .. }
            | Expr::BitStrLit { .. }
            | Expr::CharLit { .. }
            | Expr::StrLit { .. } => {}
            Expr::Path(p) => self.path(p, env, sink),
            Expr::Field { base, .. } | Expr::SysAttr { base, .. } => self.expr(base, env, sink),
            Expr::Index { base, index, .. } => {
                self.expr(base, env, sink);
                self.expr(index, env, sink);
            }
            Expr::Range { lo, hi, .. } => {
                self.expr(lo, env, sink);
                self.expr(hi, env, sink);
            }
            Expr::PartialRange { lo, hi, .. } => {
                for bound in [lo, hi].into_iter().flatten() {
                    self.expr(bound, env, sink);
                }
            }
            Expr::Unary { rhs, .. } => self.expr(rhs, env, sink),
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs, env, sink);
                self.expr(rhs, env, sink);
            }
            Expr::IfExpr {
                cond, then, els, ..
            } => {
                self.expr(cond, env, sink);
                self.expr(then, env, sink);
                self.expr(els, env, sink);
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.expr(scrutinee, env, sink);
                for arm in arms {
                    self.pattern(&mut arm.pattern, env, sink);
                    self.block(&mut arm.body, env, sink);
                }
            }
            Expr::Call {
                callee,
                type_args,
                qualifier,
                args,
                span,
                ..
            } => {
                if let Some(qualifier) = qualifier {
                    self.ty(qualifier, env, sink);
                }
                if let Expr::Index { base, .. } = callee.as_ref() {
                    if let Expr::Path(p) = base.as_ref() {
                        self.reject_format_range(p, env, sink);
                    }
                }
                for arg in type_args.iter_mut() {
                    self.generic_arg(arg, env, sink);
                }
                // `float<32, 23>(x)`: the applied alias's constructor, as
                // `float[8..-23](x)`.
                let alias = match callee.as_ref() {
                    Expr::Path(p) if !type_args.is_empty() => {
                        self.generic_alias(p, env).map(|alias| (p.clone(), alias))
                    }
                    _ => None,
                };
                // ...and `Word(x)` for `type Word = unsigned[8];` as
                // `unsigned[8](x)`: an alias is transparent.
                let plain = match callee.as_ref() {
                    Expr::Path(p) if type_args.is_empty() => self.plain_alias(p, env, sink),
                    _ => None,
                };
                if let Some((site, (key, alias))) = alias {
                    if let Some(body) =
                        self.expand_alias(&site, &key, alias, type_args, *span, sink)
                    {
                        if let Some(constructor) = type_constructor(body) {
                            **callee = constructor;
                            type_args.clear();
                        }
                    }
                } else if let Some(constructor) = plain.and_then(type_constructor) {
                    **callee = constructor;
                } else {
                    self.expr(callee, env, sink);
                }
                for arg in args {
                    self.expr(arg, env, sink);
                }
            }
            Expr::Construct {
                ty, args, spread, ..
            } => {
                if let Some(ty) = ty {
                    self.ty(ty, env, sink);
                }
                for arg in args {
                    if let Some(value) = &mut arg.value {
                        self.expr(value, env, sink);
                    }
                }
                if let Some(spread) = spread {
                    self.expr(spread, env, sink);
                }
            }
            Expr::Concat { parts: items, .. } | Expr::Array { elems: items, .. } => {
                for item in items {
                    self.expr(item, env, sink);
                }
            }
        }
    }

    /// Rewrite one path: absolute `self::`/`super::`, module aliases, and
    /// single names bound by variant, glob or block imports.
    fn path(&self, path: &mut Path, env: &Env, sink: &mut DiagnosticSink) {
        let Some(abs) = absolute_head(&path.segments, &self.module, sink) else {
            return;
        };
        let mut segments = expand_aliases(abs, env, self.index);
        if let [only] = &segments[..] {
            if !env.shadow.contains(&only.text) {
                if let Some(target) = env.names.get(&only.text) {
                    // Later stages resolve by span, so the qualifiers get
                    // an empty span of their own and the name keeps its.
                    let span = only.span;
                    let head = Span {
                        end: span.start,
                        ..span
                    };
                    segments = target.iter().map(|s| ident(s, head)).collect();
                    if let Some(last) = segments.last_mut() {
                        last.span = span;
                    }
                } else if let Some(sources) = env.ambiguous.get(&only.text) {
                    sink.emit(
                        Diagnostic::error(format!(
                            "`{}` is ambiguous: both `{}::*` and `{}::*` import it",
                            only.text, sources[0], sources[1]
                        ))
                        .with_code(codes::DUPLICATE_ITEM)
                        .at(only.span)
                        .help(format!(
                            "import it explicitly: `use {}::{};`",
                            sources[0], only.text
                        )),
                    );
                    // Read it as the first, so the error does not cascade.
                    let span = only.span;
                    segments = sources[0]
                        .split("::")
                        .chain([only.text.as_str()])
                        .map(|s| {
                            ident(
                                s,
                                Span {
                                    end: span.start,
                                    ..span
                                },
                            )
                        })
                        .collect();
                    if let Some(last) = segments.last_mut() {
                        last.span = span;
                    }
                }
            }
        }
        path.segments = segments;
    }

    /// Rewrite a type's paths, and substitute generic aliases.
    fn ty(&self, ty: &mut Type, env: &Env, sink: &mut DiagnosticSink) {
        match ty {
            Type::Path(p) => {
                if let Some((key, alias)) = self
                    .generic_alias(p, env)
                    .filter(|(_, alias)| !alias.structure)
                {
                    sink.emit(
                        Diagnostic::error(format!(
                            "the generic type alias `{}` needs {} argument(s)",
                            key.1,
                            alias.params.len()
                        ))
                        .with_code(codes::TYPE_MISMATCH)
                        .at(p.span),
                    );
                    return;
                }
                self.path(p, env, sink);
            }
            Type::Generic { base, args, span } => {
                for arg in args.iter_mut() {
                    self.generic_arg(arg, env, sink);
                }
                if let Type::Path(p) = base.as_mut() {
                    if let Some((key, alias)) = self.generic_alias(p, env) {
                        let site = p.clone();
                        if let Some(body) = self.expand_alias(&site, &key, alias, args, *span, sink)
                        {
                            *ty = body;
                        }
                        return;
                    }
                }
                self.ty(base, env, sink);
            }
            Type::Indexed { base, index, .. } => {
                if let Type::Path(p) = base.as_ref() {
                    self.reject_format_range(p, env, sink);
                }
                self.ty(base, env, sink);
                if let Some(index) = index {
                    self.expr(index, env, sink);
                }
            }
            Type::View { view, target, .. } => {
                self.path(view, env, sink);
                self.ty(target, env, sink);
            }
        }
    }

    /// A generic alias applied to `args`, substituted: `Pair<u8>` is
    /// `Packet<u8>`, `float<32, 23>` is `float[8..-23]`. `None` after an
    /// arity error.
    fn expand_alias(
        &self,
        site: &Path,
        key: &(String, String),
        alias: &GenericAlias,
        args: &[GenericArg],
        span: Span,
        sink: &mut DiagnosticSink,
    ) -> Option<Type> {
        if args.len() != alias.params.len() {
            sink.emit(
                Diagnostic::error(format!(
                    "the generic type `{}` takes {} argument(s), not {}",
                    key.1,
                    alias.params.len(),
                    args.len()
                ))
                .with_code(codes::TYPE_MISMATCH)
                .at(span),
            );
            return None;
        }
        let mut bindings: HashMap<String, GenericArg> = HashMap::new();
        for (position, arg) in args.iter().enumerate() {
            match arg {
                GenericArg::Named { name, .. } | GenericArg::NamedType { name, .. } => {
                    bindings.insert(name.text.clone(), arg.clone());
                }
                _ => {
                    bindings.insert(alias.params[position].clone(), arg.clone());
                }
            }
        }
        let mut body = alias.ty.clone();
        // The alias body is written in its own module.
        let alias_module: Vec<String> = key.0.split("::").map(str::to_string).collect();
        let home = Rewriter {
            generated: true,
            scopes: self.scopes,
            index: self.index,
            module: alias_module,
            hidden: RefCell::default(),
        };
        qualify_home(&mut body, &key.0, &alias.params, self.index);
        substitute_type(&mut body, &bindings);
        // A format struct is named as the use site names it (`float`, or a
        // renamed import), so that import is the one in use.
        if alias.structure {
            if let Type::Indexed { base, .. } = &mut body {
                **base = Type::Path(site.clone());
            }
        }
        home.ty(&mut body, &Env::default(), sink);
        Some(body)
    }

    /// The non-generic alias `path` names, with its type read in its own
    /// module and qualified so it means the same wherever it is used.
    fn plain_alias(&self, path: &Path, env: &Env, sink: &mut DiagnosticSink) -> Option<Type> {
        let key = match &path.segments[..] {
            [only] if !env.shadow.contains(&only.text) => match env.items.get(&only.text) {
                Some(full) => {
                    let (module, name) = full.rsplit_once("::")?;
                    (module.to_string(), name.to_string())
                }
                None => (self.module.join("::"), only.text.clone()),
            },
            [prefix @ .., last] if !prefix.is_empty() => {
                let abs = expand_aliases(prefix.to_vec(), env, self.index);
                (
                    abs.iter()
                        .map(|s| s.text.as_str())
                        .collect::<Vec<_>>()
                        .join("::"),
                    last.text.clone(),
                )
            }
            _ => return None,
        };
        let mut ty = self.index.plain.get(&key)?.clone();
        let home_env = self.scopes.get(&key.0).cloned().unwrap_or_default();
        let home = Rewriter {
            generated: true,
            scopes: self.scopes,
            index: self.index,
            module: key.0.split("::").map(str::to_string).collect(),
            hidden: RefCell::default(),
        };
        home.ty(&mut ty, &home_env, sink);
        qualify_with_scope(&mut ty, &key.0, &home_env, self.index);
        Some(ty)
    }

    /// A format struct's range written by hand (`float[8..-23]`): its
    /// parameters are the format, so `[...]` keeps meaning an array's size or
    /// range.
    fn reject_format_range(&self, path: &Path, env: &Env, sink: &mut DiagnosticSink) {
        if self.generated {
            return;
        }
        if let Some((key, alias)) = self.generic_alias(path, env) {
            if alias.structure {
                sink.emit(
                    Diagnostic::error(format!(
                        "`{}` takes its format as parameters, not an index range",
                        key.1
                    ))
                    .with_code(codes::TYPE_MISMATCH)
                    .at(path.span)
                    .help(format!(
                        "write `{}<{}>`",
                        key.1,
                        alias.params.join(", ")
                    )),
                );
            }
        }
    }

    /// The generic alias `path` names, if any.
    fn generic_alias(&self, path: &Path, env: &Env) -> Option<((String, String), &GenericAlias)> {
        let key = match &path.segments[..] {
            [only] if !env.shadow.contains(&only.text) => match env.generic.get(&only.text) {
                Some(key) => key.clone(),
                None => (self.module.join("::"), only.text.clone()),
            },
            [prefix @ .., last] if !prefix.is_empty() => {
                let abs = expand_aliases(prefix.to_vec(), env, self.index);
                (
                    abs.iter()
                        .map(|s| s.text.as_str())
                        .collect::<Vec<_>>()
                        .join("::"),
                    last.text.clone(),
                )
            }
            _ => return None,
        };
        self.index.generic.get(&key).map(|alias| (key, alias))
    }
}

/// Qualify the names a generic alias's body declares in its own module, so
/// the body means the same once substituted into another module:
/// `Packet<T>` in module `net` becomes `net::Packet<T>`.
fn qualify_home(ty: &mut Type, home: &str, params: &[String], index: &Index) {
    let qualify = |p: &mut Path| {
        if let [only] = &p.segments[..] {
            if !params.contains(&only.text)
                && index
                    .decls
                    .contains_key(&(home.to_string(), only.text.clone()))
            {
                let span = only.span;
                let mut segments: Vec<Ident> = home.split("::").map(|s| ident(s, span)).collect();
                segments.push(only.clone());
                p.segments = segments;
            }
        }
    };
    match ty {
        Type::Path(p) => qualify(p),
        Type::Generic { base, args, .. } => {
            qualify_home(base, home, params, index);
            for arg in args {
                if let GenericArg::PositionalType(t) | GenericArg::NamedType { ty: t, .. } = arg {
                    qualify_home(t, home, params, index);
                }
            }
        }
        Type::Indexed { base, .. } => qualify_home(base, home, params, index),
        Type::View { view, target, .. } => {
            qualify(view);
            qualify_home(target, home, params, index);
        }
    }
}

/// Qualify the names an alias's type uses where it was written — imported
/// ones by the import, local ones by their module — so it means the same in
/// the module that uses the alias.
fn qualify_with_scope(ty: &mut Type, home: &str, env: &Env, index: &Index) {
    let qualify = |p: &mut Path| {
        if let [only] = &p.segments[..] {
            let full = env.items.get(&only.text).cloned().or_else(|| {
                index
                    .decls
                    .contains_key(&(home.to_string(), only.text.clone()))
                    .then(|| format!("{home}::{}", only.text))
            });
            if let Some(full) = full {
                let span = only.span;
                p.segments = full.split("::").map(|s| ident(s, span)).collect();
            }
        }
    };
    match ty {
        Type::Path(p) => qualify(p),
        Type::Generic { base, .. } | Type::Indexed { base, .. } => {
            qualify_with_scope(base, home, env, index)
        }
        Type::View { target, .. } => qualify_with_scope(target, home, env, index),
    }
}

/// The constructor expression of a type: `float[8..-23]` as the callee
/// `float[8..-23](x)` is written with.
fn type_constructor(ty: Type) -> Option<Expr> {
    match ty {
        Type::Path(path) => Some(Expr::Path(path)),
        Type::Indexed {
            base,
            index: Some(index),
            span,
        } => Some(Expr::Index {
            base: Box::new(type_constructor(*base)?),
            index,
            span,
        }),
        _ => None,
    }
}

/// Replace a generic alias's parameters in its body with the arguments.
fn substitute_type(ty: &mut Type, bindings: &HashMap<String, GenericArg>) {
    match ty {
        Type::Path(p) => {
            if let [only] = &p.segments[..] {
                match bindings.get(&only.text) {
                    Some(GenericArg::PositionalType(t))
                    | Some(GenericArg::NamedType { ty: t, .. }) => {
                        *ty = t.clone();
                    }
                    Some(GenericArg::Positional(Expr::Path(path)))
                    | Some(GenericArg::Named {
                        value: Expr::Path(path),
                        ..
                    }) => {
                        *ty = Type::Path(path.clone());
                    }
                    _ => {}
                }
            }
        }
        Type::Generic { base, args, .. } => {
            substitute_type(base, bindings);
            for arg in args {
                match arg {
                    GenericArg::Positional(e) | GenericArg::Named { value: e, .. } => {
                        substitute_expr(e, bindings)
                    }
                    GenericArg::PositionalType(t) | GenericArg::NamedType { ty: t, .. } => {
                        substitute_type(t, bindings)
                    }
                }
            }
        }
        Type::Indexed { base, index, .. } => {
            substitute_type(base, bindings);
            if let Some(index) = index {
                substitute_expr(index, bindings);
            }
        }
        Type::View { target, .. } => substitute_type(target, bindings),
    }
}

/// Replace value parameters (`W` in `unsigned[W]`) with the arguments.
fn substitute_expr(expression: &mut Expr, bindings: &HashMap<String, GenericArg>) {
    match expression {
        Expr::Path(p) => {
            if let [only] = &p.segments[..] {
                match bindings.get(&only.text) {
                    Some(GenericArg::Positional(e)) | Some(GenericArg::Named { value: e, .. }) => {
                        *expression = e.clone();
                    }
                    // A call's name argument (`float<W, M>(x)`) parses as a type.
                    Some(GenericArg::PositionalType(Type::Path(path))) => {
                        *expression = Expr::Path(path.clone());
                    }
                    _ => {}
                }
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            substitute_expr(lhs, bindings);
            substitute_expr(rhs, bindings);
        }
        Expr::Unary { rhs, .. } => substitute_expr(rhs, bindings),
        Expr::Range { lo, hi, .. } => {
            substitute_expr(lo, bindings);
            substitute_expr(hi, bindings);
        }
        Expr::Call { args, .. } => {
            for arg in args {
                substitute_expr(arg, bindings);
            }
        }
        Expr::IfExpr {
            cond, then, els, ..
        } => {
            substitute_expr(cond, bindings);
            substitute_expr(then, bindings);
            substitute_expr(els, bindings);
        }
        _ => {}
    }
}
