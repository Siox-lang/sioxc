//! Borrowed source declarations, selected only by resolver identity.
//!
//! Rebuild this index at immutable phase boundaries after AST rewrites; keeping
//! references in a long-lived typed product would prevent those rewrites.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use super::{DefId, DefKind, Resolved};
use crate::syntax::ast::{
    ConstDecl, EntityDecl, EnumDecl, EnumVariant, FnDecl, ImplDecl, Item, Module, Params, Path,
    StructDecl, TraitDecl, Type, Using, UsingKind,
};

pub(crate) mod expand;
mod substitute;

pub(super) use substitute::parameter_kind;
pub(crate) use substitute::ArgumentError;

enum Declaration<'a> {
    Struct(&'a StructDecl),
    Entity(&'a EntityDecl),
    Enum(&'a EnumDecl),
    Trait(&'a TraitDecl),
    Function(&'a FnDecl),
    Constant(&'a ConstDecl),
    Alias(&'a Params, &'a Type),
}

pub(crate) struct DeclarationIndex<'a> {
    resolved: &'a Resolved,
    declarations: HashMap<DefId, Declaration<'a>>,
    implementations: HashMap<DefId, Vec<&'a ImplDecl>>,
}

/// One checked application; reuse its parameter identities for bounds and methods.
pub(crate) struct AppliedArguments<'a> {
    bindings: HashMap<DefId, substitute::Argument<'a>>,
}

impl<'a> AppliedArguments<'a> {
    pub(crate) fn type_argument(&self, parameter: DefId) -> Option<&Type> {
        match self.bindings.get(&parameter)? {
            substitute::Argument::Type(ty) => Some(ty),
            substitute::Argument::Value(_) => None,
        }
    }

    pub(crate) fn value(&self, parameter: DefId) -> Option<&'a crate::syntax::ast::Expr> {
        match self.bindings.get(&parameter)? {
            substitute::Argument::Value(value) => Some(*value),
            substitute::Argument::Type(_) => None,
        }
    }
}

#[derive(Default)]
struct Expansion {
    active: HashSet<DefId>,
    cycle: Option<DefId>,
}

impl Expansion {
    fn enter(&mut self, id: DefId) -> bool {
        if self.active.insert(id) {
            true
        } else {
            self.cycle.get_or_insert(id);
            false
        }
    }
}

impl<'a> DeclarationIndex<'a> {
    pub(crate) fn new(resolved: &'a Resolved) -> Self {
        Self {
            resolved,
            declarations: HashMap::new(),
            implementations: HashMap::new(),
        }
    }

    pub(crate) fn collect(resolved: &'a Resolved, modules: &'a [Module]) -> Self {
        let mut index = Self::new(resolved);
        for item in modules.iter().flat_map(|module| &module.items) {
            index.insert_item(item);
        }
        index
    }

    pub(crate) fn insert_item(&mut self, item: &'a Item) {
        match item {
            Item::Struct(declaration) => {
                if let Some(id) = self.resolved.declared(declaration.name.span) {
                    self.declarations
                        .insert(id, Declaration::Struct(declaration));
                }
            }
            Item::Entity(declaration) => {
                if let Some(id) = self.resolved.declared(declaration.name.span) {
                    self.declarations
                        .insert(id, Declaration::Entity(declaration));
                }
            }
            Item::Enum(declaration) => {
                if let Some(id) = self.resolved.declared(declaration.name.span) {
                    self.declarations.insert(id, Declaration::Enum(declaration));
                }
            }
            Item::Impl(implementation) => {
                let mut target = &implementation.target;
                loop {
                    match target {
                        Type::Path(path) => {
                            if let Some(id) = self.resolved.resolved(path.span) {
                                self.implementations
                                    .entry(id)
                                    .or_default()
                                    .push(implementation);
                            }
                            break;
                        }
                        Type::Generic { base, .. } | Type::Indexed { base, .. } => target = base,
                        Type::View { target: base, .. } => target = base,
                        Type::Tuple { .. } => break,
                    }
                }
            }
            Item::Const(declaration) => self.insert_constant(declaration),
            Item::Trait(declaration) => {
                if let Some(id) = self.resolved.declared(declaration.name.span) {
                    self.declarations
                        .insert(id, Declaration::Trait(declaration));
                }
            }
            Item::Fn(declaration) => self.insert_function(declaration),
            Item::ExternBlock { fns, .. } => {
                for declaration in fns {
                    self.insert_function(declaration);
                }
            }
            Item::Using(Using {
                kind: UsingKind::Alias { name, params, ty },
                ..
            }) => {
                if let Some(id) = self.resolved.declared(name.span) {
                    self.declarations.insert(id, Declaration::Alias(params, ty));
                }
            }
            _ => {}
        }
    }

    pub(crate) fn insert_function(&mut self, declaration: &'a FnDecl) {
        if let Some(id) = self
            .resolved
            .declared(declaration.name.span)
            .filter(|id| self.resolved.kind_of(*id) == Some(DefKind::Fn))
        {
            self.declarations
                .insert(id, Declaration::Function(declaration));
        }
    }

    pub(crate) fn insert_constant(&mut self, declaration: &'a ConstDecl) {
        if let Some(id) = self
            .resolved
            .declared(declaration.name.span)
            .filter(|id| self.resolved.kind_of(*id) == Some(DefKind::Const))
        {
            self.declarations
                .insert(id, Declaration::Constant(declaration));
        }
    }

    pub(crate) fn entity(&self, id: DefId) -> Option<&'a EntityDecl> {
        match self.declarations.get(&id)? {
            Declaration::Entity(declaration) => Some(declaration),
            _ => None,
        }
    }

    pub(crate) fn enumeration(&self, id: DefId) -> Option<&'a EnumDecl> {
        match self.declarations.get(&id)? {
            Declaration::Enum(declaration) => Some(declaration),
            _ => None,
        }
    }

    pub(crate) fn enumerations(&self) -> impl Iterator<Item = (DefId, &'a EnumDecl)> + '_ {
        self.declarations
            .iter()
            .filter_map(|(&id, declaration)| match declaration {
                Declaration::Enum(declaration) => Some((id, *declaration)),
                _ => None,
            })
    }

    pub(crate) fn enum_base(&self, owner: DefId) -> Option<DefId> {
        let declaration = self.enumeration(owner)?;
        let base = self.normalize_type(declaration.repr.as_ref()?)?;
        let Type::Path(path) = base.as_ref() else {
            return None;
        };
        let id = self.resolved.resolved(path.span)?;
        self.enumeration(id).map(|_| id)
    }

    pub(crate) fn scalar_type_id(&self, ty: &Type) -> Option<DefId> {
        let normalized = self.normalize_type(ty)?;
        let Type::Path(path) = normalized.as_ref() else {
            return None;
        };
        self.resolved.resolved(path.span)
    }

    pub(crate) fn implementations_for(
        &self,
        owner: DefId,
    ) -> impl Iterator<Item = &'a ImplDecl> + '_ {
        self.implementations
            .values()
            .flatten()
            .copied()
            .filter(move |implementation| {
                self.scalar_type_id(&implementation.target) == Some(owner)
            })
    }

    /// Inherited members borrow the base declaration; the selected enum keeps its own ID.
    pub(crate) fn enum_variants(&self, owner: DefId) -> Option<Vec<(DefId, &'a EnumVariant)>> {
        let mut chain = Vec::new();
        let mut current = owner;
        loop {
            if chain.contains(&current) {
                return None;
            }
            self.enumeration(current)?;
            chain.push(current);
            let Some(base) = self.enum_base(current) else {
                break;
            };
            current = base;
        }
        let mut variants = Vec::new();
        for owner in chain.into_iter().rev() {
            // Expanded members can share spans; their resolved owner disambiguates them.
            for variant in &self.enumeration(owner)?.variants {
                let id = self.resolved.enum_variant(owner, &variant.name.text)?;
                if self.resolved.kind_of(id) != Some(DefKind::EnumVariant) {
                    return None;
                }
                variants.push((id, variant));
            }
        }
        Some(variants)
    }

    pub(crate) fn layout_params(&self, id: DefId) -> Option<&'a Params> {
        match self.declarations.get(&id)? {
            Declaration::Struct(declaration) => Some(&declaration.params),
            Declaration::Entity(declaration) => Some(&declaration.params),
            Declaration::Alias(params, _) => Some(params),
            _ => None,
        }
    }

    pub(crate) fn layout_types(&self, id: DefId) -> impl Iterator<Item = &'a Type> + '_ {
        self.declarations
            .get(&id)
            .into_iter()
            .flat_map(|declaration| {
                let (structure, entity, alias) = match declaration {
                    Declaration::Struct(structure) => (Some(*structure), None, None),
                    Declaration::Entity(entity) => (None, Some(*entity), None),
                    Declaration::Alias(_, target) => (None, None, Some(*target)),
                    _ => (None, None, None),
                };
                structure
                    .into_iter()
                    .flat_map(|structure| {
                        structure
                            .base
                            .iter()
                            .chain(structure.fields.iter().map(|field| &field.ty))
                    })
                    .chain(
                        entity
                            .into_iter()
                            .flat_map(|entity| entity.ports.iter().map(|port| &port.ty)),
                    )
                    .chain(alias)
            })
    }

    pub(crate) fn generic_alias(&self, id: DefId) -> bool {
        matches!(self.declarations.get(&id), Some(Declaration::Alias(params, _)) if !params.params.is_empty())
    }

    pub(crate) fn trait_decl(&self, id: DefId) -> Option<&'a TraitDecl> {
        match self.declarations.get(&id)? {
            Declaration::Trait(declaration) => Some(declaration),
            _ => None,
        }
    }

    pub(crate) fn alias_arity(&self, id: DefId) -> Option<usize> {
        match self.declarations.get(&id)? {
            Declaration::Alias(params, _) => Some(params.params.len()),
            _ => None,
        }
    }

    pub(crate) fn valid_alias_arguments(&self, ty: &Type) -> bool {
        let Type::Generic { base, args, .. } = ty else {
            return false;
        };
        let Type::Path(path) = base.as_ref() else {
            return false;
        };
        let Some(Declaration::Alias(params, _)) = self
            .resolved
            .resolved(path.span)
            .and_then(|id| self.declarations.get(&id))
        else {
            return false;
        };
        substitute::bind(params, args, self.resolved).is_ok()
    }

    pub(crate) fn arguments<'args>(
        &self,
        parameters: &Params,
        arguments: &'args [crate::syntax::ast::GenericArg],
    ) -> Result<AppliedArguments<'args>, ArgumentError> {
        substitute::bind(parameters, arguments, self.resolved)
            .map(|bindings| AppliedArguments { bindings })
    }

    /// Normalize type-head aliases without losing applied parameters or array shape.
    pub(crate) fn normalize_type<'t>(&self, ty: &'t Type) -> Option<Cow<'t, Type>>
    where
        'a: 't,
    {
        self.normalize_type_inner(ty, &mut Expansion::default())
    }

    /// Report only a cycle actually encountered during shared normalization.
    pub(crate) fn alias_cycle(&self, target: &Type) -> Option<DefId> {
        let mut expansion = Expansion::default();
        let _ = self.normalize_type_inner(target, &mut expansion);
        expansion.cycle
    }

    /// Instantiate a source type without rebinding its lexical names.
    pub(crate) fn instantiate_type<'t>(
        &self,
        arguments: &AppliedArguments<'_>,
        ty: &'t Type,
    ) -> Option<Cow<'t, Type>>
    where
        'a: 't,
    {
        if arguments.bindings.is_empty() {
            return self.normalize_type(ty);
        }
        let mut instantiated = ty.clone();
        substitute::ty(&mut instantiated, &arguments.bindings, self.resolved)?;
        let normalized = match self.normalize_type(&instantiated)? {
            Cow::Borrowed(_) => instantiated,
            Cow::Owned(normalized) => normalized,
        };
        Some(Cow::Owned(normalized))
    }

    fn normalize_type_inner<'t>(&self, ty: &'t Type, seen: &mut Expansion) -> Option<Cow<'t, Type>>
    where
        'a: 't,
    {
        match ty {
            Type::Path(path) => {
                let id = self.resolved.resolved(path.span)?;
                let Some(Declaration::Alias(params, target)) = self.declarations.get(&id) else {
                    return Some(Cow::Borrowed(ty));
                };
                if !params.params.is_empty() || !seen.enter(id) {
                    return None;
                }
                let normalized = self.normalize_type_inner(target, seen);
                seen.active.remove(&id);
                normalized
            }
            Type::Generic { base, args, span } => {
                let mut rewritten = None;
                for (position, arg) in args.iter().enumerate() {
                    let Some(actual) = arg.type_ref() else {
                        continue;
                    };
                    let normalized = self.normalize_type_inner(actual, seen)?;
                    if std::ptr::eq(normalized.as_ref(), actual) {
                        continue;
                    }
                    let replacements = rewritten.get_or_insert_with(|| args.clone());
                    replacements[position] = match arg {
                        crate::syntax::ast::GenericArg::NamedType { name, .. } => {
                            crate::syntax::ast::GenericArg::NamedType {
                                name: name.clone(),
                                ty: normalized.into_owned(),
                            }
                        }
                        _ => {
                            crate::syntax::ast::GenericArg::PositionalType(normalized.into_owned())
                        }
                    };
                }
                let args = rewritten.as_ref().unwrap_or(args);
                if let Type::Path(path) = base.as_ref() {
                    let id = self.resolved.resolved(path.span)?;
                    if self.resolved.def(id).is_some_and(|d| {
                        d.kind == DefKind::Builtin && d.name == crate::syntax::ast::TUPLE_POSITION
                    }) {
                        let [whole, crate::syntax::ast::GenericArg::Positional(
                            crate::syntax::ast::Expr::Int { text, .. },
                        )] = args.as_slice()
                        else {
                            return None;
                        };
                        let whole = whole.as_type()?;
                        let normalized = self.normalize_type_inner(&whole, seen)?;
                        let Type::Tuple { elems, .. } = normalized.as_ref() else {
                            return None;
                        };
                        return Some(Cow::Owned(elems.get(text.parse::<usize>().ok()?)?.clone()));
                    }
                    if let Some(Declaration::Alias(params, target)) = self.declarations.get(&id) {
                        if !seen.enter(id) {
                            return None;
                        }
                        let bindings = substitute::bind(params, args, self.resolved).ok()?;
                        let mut instantiated = (*target).clone();
                        substitute::ty(&mut instantiated, &bindings, self.resolved)?;
                        let normalized =
                            self.normalize_type_inner(&instantiated, seen)?.into_owned();
                        seen.active.remove(&id);
                        return Some(Cow::Owned(normalized));
                    }
                }
                let normalized = self.normalize_type_inner(base, seen)?;
                if std::ptr::eq(normalized.as_ref(), base.as_ref()) && rewritten.is_none() {
                    Some(Cow::Borrowed(ty))
                } else {
                    Some(Cow::Owned(Type::Generic {
                        base: Box::new(normalized.into_owned()),
                        args: args.clone(),
                        span: *span,
                    }))
                }
            }
            Type::Indexed { base, index, span } => {
                let normalized = self.normalize_type_inner(base, seen)?;
                if std::ptr::eq(normalized.as_ref(), base.as_ref()) {
                    Some(Cow::Borrowed(ty))
                } else {
                    Some(Cow::Owned(Type::Indexed {
                        base: Box::new(normalized.into_owned()),
                        index: index.clone(),
                        span: *span,
                    }))
                }
            }
            Type::View { view, target, span } => {
                let normalized = self.normalize_type_inner(target, seen)?;
                if std::ptr::eq(normalized.as_ref(), target.as_ref()) {
                    Some(Cow::Borrowed(ty))
                } else {
                    Some(Cow::Owned(Type::View {
                        view: view.clone(),
                        target: Box::new(normalized.into_owned()),
                        span: *span,
                    }))
                }
            }
            Type::Tuple { elems, span } => {
                let mut rewritten = None;
                for (position, elem) in elems.iter().enumerate() {
                    let normalized = self.normalize_type_inner(elem, seen)?;
                    if !std::ptr::eq(normalized.as_ref(), elem) {
                        rewritten.get_or_insert_with(|| elems.clone())[position] =
                            normalized.into_owned();
                    }
                }
                match rewritten {
                    Some(elems) => Some(Cow::Owned(Type::Tuple { elems, span: *span })),
                    None => Some(Cow::Borrowed(ty)),
                }
            }
        }
    }

    /// Entity identity through normalized application/index wrappers.
    pub(crate) fn entity_type_id(&self, ty: &Type) -> Option<DefId> {
        let normalized = self.normalize_type(ty)?;
        let mut head = normalized.as_ref();
        loop {
            match head {
                Type::Path(path) => {
                    let id = self.resolved.resolved(path.span)?;
                    return self.entity(id).map(|_| id);
                }
                Type::Generic { base, .. } | Type::Indexed { base, .. } => head = base,
                Type::View { .. } | Type::Tuple { .. } => return None,
            }
        }
    }

    /// An owned resolution fact; it keeps no borrow of the mutable module tree.
    pub(crate) fn entity_aliases(&self) -> HashMap<DefId, DefId> {
        self.declarations
            .iter()
            .filter_map(|(&id, declaration)| {
                let Declaration::Alias(_, target) = declaration else {
                    return None;
                };
                self.entity_type_id(target).map(|entity| (id, entity))
            })
            .collect()
    }

    pub(crate) fn function(&self, id: DefId) -> Option<&'a FnDecl> {
        match self.declarations.get(&id)? {
            Declaration::Function(declaration) => Some(declaration),
            _ => None,
        }
    }

    pub(crate) fn constant(&self, id: DefId) -> Option<&'a ConstDecl> {
        match self.declarations.get(&id)? {
            Declaration::Constant(declaration) => Some(declaration),
            _ => None,
        }
    }

    pub(crate) fn function_path(&self, path: &Path) -> Option<&'a FnDecl> {
        self.function(self.resolved.resolved(path.span)?)
    }

    /// The scalar integer interpreter cannot erase a packed/real argument or
    /// return conversion. Keep those calls unknown until typed values exist.
    pub(crate) fn integer_function_path(&self, path: &Path) -> Option<&'a FnDecl> {
        let function = self.function_path(path)?;
        (self.is_integer_type(function.ret.as_ref()?)
            && function
                .params
                .iter()
                .filter(|parameter| !parameter.is_self)
                .all(|parameter| {
                    parameter
                        .ty
                        .as_ref()
                        .is_some_and(|ty| self.is_integer_type(ty))
                }))
        .then_some(function)
    }

    fn is_integer_type(&self, ty: &Type) -> bool {
        let Some(normalized) = self.normalize_type(ty) else {
            return false;
        };
        let Type::Path(path) = normalized.as_ref() else {
            return false;
        };
        self.resolved
            .resolved(path.span)
            .and_then(|id| self.resolved.def(id))
            .is_some_and(|definition| {
                definition.kind == DefKind::Builtin && definition.name == "integer"
            })
    }

    pub(crate) fn constant_path(&self, path: &Path) -> Option<&'a ConstDecl> {
        self.constant(self.resolved.resolved(path.span)?)
    }

    pub(crate) fn integer_constant_path(&self, path: &Path) -> Option<&'a ConstDecl> {
        let constant = self.constant_path(path)?;
        self.is_integer_type(&constant.ty).then_some(constant)
    }

    /// A bare constant use still denotes its declaration, never a caller's
    /// same-named parameter. Locals and impl-only bindings keep their spelling.
    pub(crate) fn local_name<'p>(&self, path: &'p Path) -> Option<&'p str> {
        if self
            .resolved
            .resolved(path.span)
            .is_some_and(|id| self.resolved.kind_of(id) == Some(DefKind::Const))
        {
            return None;
        }
        match path.segments.as_slice() {
            [name] => Some(&name.text),
            _ => None,
        }
    }

    pub(crate) fn local_id(&self, path: &Path) -> Option<DefId> {
        self.resolved.resolved(path.span).filter(|id| {
            matches!(
                self.resolved.kind_of(*id),
                Some(DefKind::Local | DefKind::Param)
            )
        })
    }

    pub(crate) fn parameter_id(&self, parameter: &crate::syntax::ast::FnParam) -> Option<DefId> {
        self.resolved.declared(
            parameter
                .name
                .as_ref()
                .map_or(parameter.span, |name| name.span),
        )
    }
}
