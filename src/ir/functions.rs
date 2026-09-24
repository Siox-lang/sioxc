//! Resolved function and nominal-owner index used by IR lowering.

use std::collections::HashMap;

use crate::resolve::{is_compiler_trait, DefId, Resolved};
use crate::syntax::ast;

use super::OperatorImpls;

/// Functions available to lowering and constant evaluation.
///
/// Module-level and foreign functions use the resolver's stable declaration
/// identity, so equal leaf names in different modules cannot overwrite one
/// another. Static associated functions do not yet receive their own `DefId`;
/// they remain in a deliberately separate `Type::name` registry whose owner
/// is still the resolver-selected nominal type or entity identity.
pub struct FunctionIndex<'a> {
    /// Resolution this index was built over, for definition lookup.
    resolved: &'a Resolved,
    /// Module-level and foreign functions, by declaration identity.
    free: HashMap<DefId, &'a ast::FnDecl>,
    /// Static associated functions, keyed by `Type::name` because they do
    /// not yet receive a `DefId` of their own.
    associated: HashMap<String, &'a ast::FnDecl>,
    /// Concrete operator implementations, keyed by source symbol and resolved
    /// owner type. Each candidate retains the impl's declared input type for
    /// overload selection; the function body remains ordinary Siox source and
    /// is inlined by the IR consumer that selected it.
    operators: OperatorImpls<'a>,
    /// Source-declared `Operator` implementations over unconstrained `T[]`.
    /// Their loop-shaped bodies are expanded by Process lowering, while each
    /// element still dispatches through its concrete source implementation.
    blanket_array_operators: HashMap<String, &'a ast::FnDecl>,
}

impl<'a> FunctionIndex<'a> {
    /// Build the index over one resolution's definitions.
    pub fn new(resolved: &'a Resolved) -> Self {
        Self {
            resolved,
            free: HashMap::new(),
            associated: HashMap::new(),
            operators: HashMap::new(),
            blanket_array_operators: HashMap::new(),
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

    /// Register the executable `apply` body of one `Operator` impl. Blanket
    /// `T[]` declarations are indexed separately from concrete nominal owners;
    /// Process lowering expands their loop shape before selecting element
    /// implementations.
    pub fn insert_operator_impl(&mut self, implementation: &'a ast::ImplDecl) {
        let Some(trait_path) = implementation.trait_.as_ref() else {
            return;
        };
        if self.trait_path_key(trait_path).as_deref() != Some("Operator") {
            return;
        }
        let Some(symbol) = implementation
            .trait_args
            .first()
            .and_then(|argument| match argument {
                ast::GenericArg::Positional(ast::Expr::StrLit { text, .. }) => Some(text.clone()),
                _ => None,
            })
        else {
            return;
        };
        if is_blanket_array_impl(implementation) {
            if let Some(function) = implementation.items.iter().find_map(|item| match item {
                ast::ImplItem::Fn(function) if function.name.text == "apply" => Some(function),
                _ => None,
            }) {
                self.blanket_array_operators.insert(symbol, function);
            }
            return;
        }
        let Some(owner) = self.type_head_key(&implementation.target) else {
            return;
        };
        let input = implementation
            .trait_args
            .get(1)
            .and_then(|argument| match argument {
                ast::GenericArg::Positional(ast::Expr::Path(path)) => self.type_path_key(path),
                ast::GenericArg::PositionalType(ty) => self.type_head_key(ty),
                _ => None,
            });
        for item in &implementation.items {
            let ast::ImplItem::Fn(function) = item else {
                continue;
            };
            if function.name.text == "apply" {
                self.operators
                    .entry((symbol.clone(), owner.clone()))
                    .or_default()
                    .push((function, input.clone()));
            }
        }
    }

    /// Select a binary operator body by exact right-operand type. A kernel
    /// integer literal may adopt the owner type, matching the type checker's
    /// contextual literal rule. A missing actual type is accepted only when
    /// the owner has one candidate, so ambiguity never depends on declaration
    /// order.
    pub fn get_binary_operator(
        &self,
        symbol: &str,
        owner: &str,
        input: Option<&str>,
    ) -> Option<&'a ast::FnDecl> {
        let candidates = self
            .operators
            .get(&(symbol.to_string(), owner.to_string()))?;
        let declared_input = |function: &ast::FnDecl, input: &Option<String>| {
            input.clone().or_else(|| {
                function
                    .params
                    .iter()
                    .find(|parameter| !parameter.is_self)
                    .and_then(|parameter| parameter.ty.as_ref())
                    .and_then(|ty| self.type_head_key(ty))
            })
        };
        let matches = |function: &ast::FnDecl, declared: &Option<String>, wanted: &str| {
            declared_input(function, declared)
                .map(|input| {
                    if input == "Self" {
                        owner.to_string()
                    } else {
                        input
                    }
                })
                .as_deref()
                == Some(wanted)
        };
        match input {
            Some(input) => candidates
                .iter()
                .find(|(function, declared)| matches(function, declared, input))
                .or_else(|| {
                    if input == "integer" {
                        candidates
                            .iter()
                            .find(|(function, declared)| matches(function, declared, owner))
                    } else {
                        None
                    }
                })
                .map(|(function, _)| *function),
            None if candidates.len() == 1 => candidates.first().map(|(function, _)| *function),
            None => None,
        }
    }

    /// Select the receiver-only `apply(self)` implementation of a unary
    /// operator. Input/output trait parameters describe its contract but do
    /// not add a runtime argument.
    pub fn get_unary_operator(&self, symbol: &str, owner: &str) -> Option<&'a ast::FnDecl> {
        self.operators
            .get(&(symbol.to_string(), owner.to_string()))?
            .iter()
            .find_map(|(function, _)| {
                (function
                    .params
                    .iter()
                    .filter(|parameter| !parameter.is_self)
                    .count()
                    == 0)
                    .then_some(*function)
            })
    }

    /// Whether source declares an unconstrained-array lift with this runtime
    /// arity. The body establishes availability; Process lowering expands its
    /// element loop and then executes concrete element `apply` bodies.
    pub fn has_blanket_array_operator(&self, symbol: &str, argument_count: usize) -> bool {
        self.blanket_array_operators
            .get(symbol)
            .is_some_and(|function| {
                function
                    .params
                    .iter()
                    .filter(|parameter| !parameter.is_self)
                    .count()
                    == argument_count
            })
    }

    /// Look up an associated declaration after the caller has resolved the
    /// receiver/owner type. This is used by Process lowering for ordinary
    /// method syntax, whose callee is a field expression rather than a path.
    pub fn get_associated(&self, owner: &str, name: &str) -> Option<&'a ast::FnDecl> {
        self.associated.get(&format!("{owner}::{name}")).copied()
    }

    /// Stable owner key for an already-resolved nominal type. This is the
    /// typed-expression counterpart of [`Self::type_head_key`].
    pub fn nominal_type_key(&self, id: DefId) -> Option<String> {
        match self.resolved.kind_of(id)? {
            crate::resolve::DefKind::Enum => self.enum_id_key(id),
            crate::resolve::DefKind::Struct => self.struct_id_key(id),
            crate::resolve::DefKind::TypeAlias => self.resolved.qualified_name(id),
            _ => self.resolved.qualified_name(id),
        }
    }

    /// Canonicalize a checked nominal name through the same collision policy
    /// used when implementation declarations enter this index. Checked array
    /// families may retain their fully qualified identity
    /// (`std::bits::unsigned`) while the unique std declaration deliberately
    /// keeps the compact `unsigned` dispatch key; both spellings must select
    /// the same source implementation.
    pub fn canonical_type_key(&self, key: &str) -> String {
        if !key.contains("::") {
            return key.to_string();
        }
        self.resolved
            .defs()
            .iter()
            .enumerate()
            .find_map(|(index, _definition)| {
                let id = DefId(u32::try_from(index).ok()?);
                (self.resolved.qualified_name(id).as_deref() == Some(key))
                    .then(|| self.nominal_type_key(id))
                    .flatten()
            })
            .unwrap_or_else(|| key.to_string())
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

/// Whether an implementation targets an unconstrained array of one of its
/// own type parameters, as in `impl<T: Operator<...>> Operator<...> for T[]`.
fn is_blanket_array_impl(implementation: &ast::ImplDecl) -> bool {
    let ast::Type::Indexed {
        base, index: None, ..
    } = &implementation.target
    else {
        return false;
    };
    let ast::Type::Path(path) = base.as_ref() else {
        return false;
    };
    let [name] = path.segments.as_slice() else {
        return false;
    };
    implementation
        .params
        .params
        .iter()
        .any(|parameter| parameter.name.text == name.text)
}
