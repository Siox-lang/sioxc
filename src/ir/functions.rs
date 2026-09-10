//! Resolved function and nominal-owner index used by IR lowering.

use std::collections::HashMap;

use crate::resolve::{is_compiler_trait, DefId, Resolved};
use crate::syntax::ast;

/// Functions available to lowering and constant evaluation.
///
/// Module-level and foreign functions use the resolver's stable declaration
/// identity, so equal leaf names in different modules cannot overwrite one
/// another. Static associated functions do not yet receive their own `DefId`;
/// they remain in a deliberately separate `Type::name` registry whose owner
/// is still the resolver-selected nominal type or entity identity.
pub struct FunctionIndex<'a> {
    resolved: &'a Resolved,
    free: HashMap<DefId, &'a ast::FnDecl>,
    associated: HashMap<String, &'a ast::FnDecl>,
}

impl<'a> FunctionIndex<'a> {
    /// Build the index over one resolution's definitions.
    pub fn new(resolved: &'a Resolved) -> Self {
        Self {
            resolved,
            free: HashMap::new(),
            associated: HashMap::new(),
        }
    }

    /// Register a module-level or foreign function declaration.
    pub fn insert_free(&mut self, function: &'a ast::FnDecl) {
        if let Some(id) = self.resolved.declared(function.name.span) {
            self.free.insert(id, function);
        }
    }

    /// Register a static associated function, replacing an inherited default.
    pub fn insert_associated(&mut self, key: String, function: &'a ast::FnDecl) {
        self.associated.insert(key, function);
    }

    /// Register an inherited static default unless the impl overrides it.
    pub fn insert_associated_default(&mut self, key: String, function: &'a ast::FnDecl) {
        self.associated.entry(key).or_insert(function);
    }

    /// Resolve a call expression to the declaration selected by name
    /// resolution, falling back to the separate associated-function registry.
    pub fn get(&self, callee: &ast::Expr) -> Option<&'a ast::FnDecl> {
        if let ast::Expr::Path(path) = callee {
            if let Some(id) = self.resolved.resolved(path.span) {
                if self.resolved.kind_of(id) == Some(crate::resolve::DefKind::Fn) {
                    return self.free.get(&id).copied();
                }
            }
            if let Some(key) = self.associated_path_key(path) {
                return self.associated.get(&key).copied();
            }
        }
        None
    }

    /// Stable table key for a module constant declaration. Implementation
    /// constants have no module-level declaration identity and deliberately
    /// retain their local leaf spelling.
    pub fn constant_decl_key(&self, constant: &ast::ConstDecl) -> String {
        self.resolved
            .declared(constant.name.span)
            .filter(|id| self.resolved.kind_of(*id) == Some(crate::resolve::DefKind::Const))
            .and_then(|id| self.resolved.qualified_name(id))
            .unwrap_or_else(|| constant.name.text.clone())
    }

    /// Resolver-selected key for a constant path. A bare implementation
    /// constant or function parameter is local and falls back to its leaf;
    /// multi-segment non-constant paths (notably enum variants) return `None`.
    pub fn constant_path_key(&self, path: &ast::Path) -> Option<String> {
        if let Some(id) = self.resolved.resolved(path.span) {
            if self.resolved.kind_of(id) == Some(crate::resolve::DefKind::Const) {
                return self.resolved.qualified_name(id);
            }
        }
        match path.segments.as_slice() {
            [name] => Some(name.text.clone()),
            _ => None,
        }
    }

    /// Constant key for scalar/struct paths (`N`, `a::N`, `K.field`). Indexing
    /// is handled by the separate constant-array table, whose base uses this
    /// same helper.
    pub fn constant_expr_key(&self, expression: &ast::Expr) -> Option<String> {
        match expression {
            ast::Expr::Path(path) => self.constant_path_key(path),
            ast::Expr::Field { base, field, .. } => {
                Some(format!("{}.{}", self.constant_expr_key(base)?, field.text))
            }
            _ => None,
        }
    }

    /// Stable table key for a module type-alias declaration.
    pub fn type_alias_decl_key(&self, name: &ast::Ident) -> String {
        self.resolved
            .declared(name.span)
            .filter(|id| self.resolved.kind_of(*id) == Some(crate::resolve::DefKind::TypeAlias))
            .and_then(|id| self.resolved.qualified_name(id))
            .unwrap_or_else(|| name.text.clone())
    }

    /// Stable table key for an enum declaration.
    pub fn enum_decl_key(&self, name: &ast::Ident) -> String {
        self.resolved
            .declared(name.span)
            .filter(|id| self.resolved.kind_of(*id) == Some(crate::resolve::DefKind::Enum))
            .and_then(|id| self.enum_id_key(id))
            .unwrap_or_else(|| name.text.clone())
    }

    /// Resolver-selected identity of a path that names an enum.
    pub fn enum_path_key(&self, path: &ast::Path) -> Option<String> {
        let id = self.resolved.resolved(path.span)?;
        (self.resolved.kind_of(id) == Some(crate::resolve::DefKind::Enum))
            .then(|| self.enum_id_key(id))
            .flatten()
    }

    /// Enum named directly by a type/call path, or by the owner portion of
    /// `Enum::new`.
    pub fn enum_owner_key(&self, path: &ast::Path) -> Option<String> {
        self.enum_path_key(path).or_else(|| {
            let owner = path.segments.get(path.segments.len().checked_sub(2)?)?;
            let id = self.resolved.resolved(owner.span)?;
            (self.resolved.kind_of(id) == Some(crate::resolve::DefKind::Enum))
                .then(|| self.enum_id_key(id))
                .flatten()
        })
    }

    /// Stable table key for a struct declaration.
    pub fn struct_decl_key(&self, name: &ast::Ident) -> String {
        self.resolved
            .declared(name.span)
            .filter(|id| self.resolved.kind_of(*id) == Some(crate::resolve::DefKind::Struct))
            .and_then(|id| self.struct_id_key(id))
            .unwrap_or_else(|| name.text.clone())
    }

    /// Resolver-selected identity of a path that names a struct.
    pub fn struct_path_key(&self, path: &ast::Path) -> Option<String> {
        let id = self.resolved.resolved(path.span)?;
        (self.resolved.kind_of(id) == Some(crate::resolve::DefKind::Struct))
            .then(|| self.struct_id_key(id))
            .flatten()
    }

    /// Resolver-selected identity of a path that names an entity. Entities
    /// are not value constructors, but they may own static associated
    /// functions and therefore need the same stable owner identity as types.
    pub fn entity_path_key(&self, path: &ast::Path) -> Option<String> {
        let id = self.resolved.resolved(path.span)?;
        (self.resolved.kind_of(id) == Some(crate::resolve::DefKind::Entity))
            .then(|| self.resolved.qualified_name(id))
            .flatten()
    }

    /// Stable table key for a view declaration.
    pub fn view_decl_key(&self, name: &ast::Ident) -> String {
        self.resolved
            .declared(name.span)
            .filter(|id| self.resolved.kind_of(*id) == Some(crate::resolve::DefKind::View))
            .and_then(|id| self.view_id_key(id))
            .unwrap_or_else(|| name.text.clone())
    }

    /// Resolver-selected identity of a path that names a view.
    pub fn view_path_key(&self, path: &ast::Path) -> Option<String> {
        let id = self.resolved.resolved(path.span)?;
        (self.resolved.kind_of(id) == Some(crate::resolve::DefKind::View))
            .then(|| self.view_id_key(id))
            .flatten()
    }

    /// Stable table key for a trait declaration. Compiler hook traits retain
    /// their canonical leaf key; ordinary traits use their qualified identity.
    pub fn trait_decl_key(&self, name: &ast::Ident) -> String {
        let Some(id) = self.resolved.declared(name.span) else {
            return name.text.clone();
        };
        if is_compiler_trait(self.resolved, id) {
            name.text.clone()
        } else {
            self.resolved
                .qualified_name(id)
                .unwrap_or_else(|| name.text.clone())
        }
    }

    /// Resolver-selected identity of a path that names a trait.
    pub fn trait_path_key(&self, path: &ast::Path) -> Option<String> {
        let id = self.resolved.resolved(path.span)?;
        if is_compiler_trait(self.resolved, id) {
            self.resolved
                .def(id)
                .map(|definition| definition.name.clone())
        } else {
            (self.resolved.kind_of(id) == Some(crate::resolve::DefKind::Trait))
                .then(|| self.resolved.qualified_name(id))
                .flatten()
        }
    }

    /// Nominal type named directly by a constructor path, or by the owner of
    /// `T::new`. Every declared owner uses its identity-preserving key.
    pub fn type_owner_key(&self, path: &ast::Path) -> Option<String> {
        let type_key = |id: DefId| match self.resolved.kind_of(id)? {
            crate::resolve::DefKind::Enum => self.enum_id_key(id),
            crate::resolve::DefKind::TypeAlias => self.resolved.qualified_name(id),
            crate::resolve::DefKind::Struct => self.struct_id_key(id),
            _ => None,
        };
        self.resolved
            .resolved(path.span)
            .and_then(type_key)
            .or_else(|| {
                (path.segments.last()?.text == "new").then_some(())?;
                let owner = path.segments.get(path.segments.len().checked_sub(2)?)?;
                self.resolved.resolved(owner.span).and_then(type_key)
            })
    }

    /// Stable lookup key for `Type::function`, based on the resolved owner
    /// rather than the spelling at the call site.  Thus an imported
    /// `Pair::tag` and its fully qualified `a::record::Pair::tag` spelling
    /// select the same implementation without conflating another module's
    /// `Pair`.
    fn associated_path_key(&self, path: &ast::Path) -> Option<String> {
        let function = path.segments.last()?;
        let owner = path.segments.get(path.segments.len().checked_sub(2)?)?;
        let id = self.resolved.resolved(owner.span)?;
        let owner = match self.resolved.kind_of(id)? {
            crate::resolve::DefKind::Enum => self.enum_id_key(id),
            crate::resolve::DefKind::TypeAlias => self.resolved.qualified_name(id),
            crate::resolve::DefKind::Struct => self.struct_id_key(id),
            crate::resolve::DefKind::Entity => self.resolved.qualified_name(id),
            _ => None,
        }?;
        Some(format!("{owner}::{}", function.text))
    }

    /// Enum identity and variant leaf selected by a variant expression path.
    pub fn enum_variant_key(&self, path: &ast::Path) -> Option<(String, String)> {
        let variant = self.resolved.resolved(path.span)?;
        let definition = self.resolved.def(variant)?;
        if definition.kind != crate::resolve::DefKind::EnumVariant {
            return None;
        }
        let written_owner = path
            .segments
            .get(path.segments.len().checked_sub(2)?)
            .and_then(|name| self.resolved.resolved(name.span))
            .filter(|id| self.resolved.kind_of(*id) == Some(crate::resolve::DefKind::Enum));
        let owner = self.enum_id_key(written_owner.or(definition.parent)?)?;
        Some((owner, definition.name.clone()))
    }

    /// Preserve the ordinary leaf in the common case and qualify colliding
    /// enum leaves. Output metadata is user-facing, while the qualified form
    /// remains injective when separate modules both declare (for example)
    /// `State`. A canonical standard-library enum retains its historical leaf
    /// key so compiler-known `Bool`, `Logic`, and `Ordering` tables do not move
    /// merely because user code declares a namesake; that user declaration is
    /// the one that receives a qualified key.
    fn enum_id_key(&self, id: DefId) -> Option<String> {
        let definition = self.resolved.def(id)?;
        let colliders: Vec<_> = self
            .resolved
            .defs()
            .iter()
            .filter(|other| {
                other.kind == crate::resolve::DefKind::Enum
                    && other.name == definition.name
                    && other.module != definition.module
            })
            .collect();
        let is_std = definition
            .module
            .as_deref()
            .is_some_and(|module| module == "std" || module.starts_with("std::"));
        let other_std = colliders.iter().any(|other| {
            other
                .module
                .as_deref()
                .is_some_and(|module| module == "std" || module.starts_with("std::"))
        });
        if !colliders.is_empty() && (!is_std || other_std) {
            self.resolved.qualified_name(id)
        } else {
            Some(definition.name.clone())
        }
    }

    /// Struct counterpart of [`Self::enum_id_key`]. Standard vector/kernel
    /// newtypes retain their historical short keys; a user namesake receives
    /// a qualified key, while unrelated user collisions qualify both sides.
    fn struct_id_key(&self, id: DefId) -> Option<String> {
        let definition = self.resolved.def(id)?;
        let colliders: Vec<_> = self
            .resolved
            .defs()
            .iter()
            .filter(|other| {
                other.kind == crate::resolve::DefKind::Struct
                    && other.name == definition.name
                    && other.module != definition.module
            })
            .collect();
        let is_std = definition
            .module
            .as_deref()
            .is_some_and(|module| module == "std" || module.starts_with("std::"));
        let other_std = colliders.iter().any(|other| {
            other
                .module
                .as_deref()
                .is_some_and(|module| module == "std" || module.starts_with("std::"))
        });
        if !colliders.is_empty() && (!is_std || other_std) {
            self.resolved.qualified_name(id)
        } else {
            Some(definition.name.clone())
        }
    }

    /// View counterpart of [`Self::struct_id_key`]. The common case remains
    /// readable; equal leaves in separate modules qualify both identities.
    fn view_id_key(&self, id: DefId) -> Option<String> {
        let definition = self.resolved.def(id)?;
        let collides = self.resolved.defs().iter().any(|other| {
            other.kind == crate::resolve::DefKind::View
                && other.name == definition.name
                && other.module != definition.module
        });
        if collides {
            self.resolved.qualified_name(id)
        } else {
            Some(definition.name.clone())
        }
    }

    /// Resolver-selected key for a type-alias use. Non-alias single-segment
    /// paths retain their surface spelling so kernel and local types can use
    /// the same callers without entering the module-alias table.
    pub fn type_alias_path_key(&self, path: &ast::Path) -> Option<String> {
        if let Some(id) = self.resolved.resolved(path.span) {
            if self.resolved.kind_of(id) == Some(crate::resolve::DefKind::TypeAlias) {
                return self.resolved.qualified_name(id);
            }
        }
        match path.segments.as_slice() {
            [name] => Some(name.text.clone()),
            _ => None,
        }
    }

    /// Identity-preserving head name for a path used as a type.
    pub fn type_path_key(&self, path: &ast::Path) -> Option<String> {
        self.enum_path_key(path)
            .or_else(|| self.struct_path_key(path))
            .or_else(|| self.entity_path_key(path))
            .or_else(|| self.type_alias_path_key(path))
            .or_else(|| path.segments.last().map(|name| name.text.clone()))
    }

    /// Identity-preserving head name for a declared type. Applied views carry
    /// both the resolved view and backing identities.
    pub fn type_head_key(&self, ty: &ast::Type) -> Option<String> {
        match ty {
            ast::Type::Path(path) => self.type_path_key(path),
            ast::Type::Generic { base, .. } | ast::Type::Indexed { base, .. } => {
                self.type_head_key(base)
            }
            ast::Type::View { view, target, .. } => Some(format!(
                "{}@{}",
                self.view_path_key(view)?,
                self.type_head_key(target)?
            )),
        }
    }
}
