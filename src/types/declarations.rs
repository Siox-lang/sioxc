//! Borrowed source declarations, selected only by resolver identity.
//!
//! Rebuild this index at immutable phase boundaries after AST rewrites; keeping
//! references in a long-lived typed product would prevent those rewrites.

use std::collections::{HashMap, HashSet};

use crate::resolve::{DefId, DefKind, Resolved};
use crate::syntax::ast::{
    ConstDecl, EntityDecl, FnDecl, Item, Module, Path, Type, Using, UsingKind,
};

enum Declaration<'a> {
    Entity(&'a EntityDecl),
    Function(&'a FnDecl),
    Constant(&'a ConstDecl),
    Alias(&'a Type),
}

pub(crate) struct DeclarationIndex<'a> {
    resolved: &'a Resolved,
    declarations: HashMap<DefId, Declaration<'a>>,
}

impl<'a> DeclarationIndex<'a> {
    pub(crate) fn new(resolved: &'a Resolved) -> Self {
        Self {
            resolved,
            declarations: HashMap::new(),
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
            Item::Entity(declaration) => {
                if let Some(id) = self.resolved.declared(declaration.name.span) {
                    self.declarations
                        .insert(id, Declaration::Entity(declaration));
                }
            }
            Item::Const(declaration) => self.insert_constant(declaration),
            Item::Fn(declaration) => self.insert_function(declaration),
            Item::ExternBlock { fns, .. } => {
                for declaration in fns {
                    self.insert_function(declaration);
                }
            }
            Item::Using(Using {
                kind: UsingKind::Alias { name, params, ty },
                ..
            }) if params.params.is_empty() => {
                if let Some(id) = self.resolved.declared(name.span) {
                    self.declarations.insert(id, Declaration::Alias(ty));
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

    /// Entity identity through application/index wrappers, never a spelling lookup.
    pub(crate) fn entity_type_id(&self, ty: &Type) -> Option<DefId> {
        // shortcut: aliases keep their written heads; normalize them at a shared typed boundary.
        match ty {
            Type::Path(path) => {
                let id = self.resolved.resolved(path.span)?;
                self.entity(id).map(|_| id)
            }
            Type::Generic { base, .. } | Type::Indexed { base, .. } => self.entity_type_id(base),
            Type::View { .. } | Type::Tuple { .. } => None,
        }
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

    fn is_integer_type(&self, mut ty: &'a Type) -> bool {
        let mut seen = HashSet::new();
        loop {
            let Type::Path(path) = ty else { return false };
            let Some(id) = self.resolved.resolved(path.span) else {
                return false;
            };
            let Some(definition) = self.resolved.def(id) else {
                return false;
            };
            if definition.kind == DefKind::Builtin {
                return definition.name == "integer";
            }
            if !seen.insert(id) {
                return false;
            }
            let Some(Declaration::Alias(target)) = self.declarations.get(&id) else {
                return false;
            };
            ty = target;
        }
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
}

#[cfg(test)]
mod tests;
