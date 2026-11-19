//! Stable registry keys and type-head lookups shared by the other checks.

use super::*;

impl<'a> Checker<'a> {
    /// Borrow the effective enum domain from the shared member identities.
    pub(super) fn enum_member_names(&self, owner: DefId) -> Option<impl Iterator<Item = &str>> {
        Some(
            self.enum_values
                .members(owner)?
                .iter()
                .filter_map(|(member, _)| self.resolved.def(*member))
                .map(|member| member.name.as_str()),
        )
    }

    pub(super) fn is_enum_ty(&self, ty: &Ty) -> bool {
        matches!(ty, Ty::Named(owner) if self.enum_values.members(*owner).is_some())
    }

    pub(super) fn is_struct_ty(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Named(owner) => self.declarations.structure(*owner).is_some(),
            Ty::Array {
                family: Some(family),
                ..
            } => self.is_array_family(family),
            _ => false,
        }
    }

    /// A checked entity type already carries its resolver-selected identity.
    pub(super) fn entity_id(&self, ty: &Ty) -> Option<DefId> {
        match ty {
            Ty::Named(id) => self.declarations.entity(*id).map(|_| *id),
            _ => None,
        }
    }

    /// The visibility of a field, following nominal derivation to wherever it
    /// was declared.
    pub(super) fn field_visibility_for(
        &self,
        owner: DefId,
        field: &str,
    ) -> Option<MemberVisibility> {
        let (owner, field) = self.field_declaration(owner, field)?;
        Some(MemberVisibility {
            is_pub: field.is_pub,
            type_private: true,
            owner: self.definition_key(owner)?,
            module: self.module_of(field.span),
            span: field.name.span,
        })
    }

    /// The module a span belongs to. Files are not modules: several files may
    /// declare one.
    pub(super) fn module_of(&self, span: Span) -> String {
        self.file_modules
            .get(&span.file)
            .cloned()
            .unwrap_or_else(|| format!("<file:{}>", span.file.0))
    }

    /// The stable module-qualified key for a definition, used everywhere a type
    /// is compared by identity rather than spelling.
    pub(super) fn definition_key(&self, id: DefId) -> Option<String> {
        self.definition_key_str(id).map(str::to_owned)
    }

    /// [`Checker::definition_key`], borrowed.
    pub(super) fn definition_key_str(&self, id: DefId) -> Option<&str> {
        let definition = self.resolved.def(id)?;
        if matches!(
            definition.kind,
            DefKind::Builtin | DefKind::Param | DefKind::Local
        ) {
            Some(&definition.name)
        } else {
            self.resolved.qualified_name_str(id)
        }
    }

    /// The key for whatever is declared at `span`.
    pub(super) fn declaration_key(&self, span: Span) -> Option<String> {
        self.resolved
            .declared(span)
            .and_then(|id| self.definition_key(id))
    }

    /// The key for whatever a path resolves to.
    pub(super) fn path_key(&self, path: &Path) -> Option<String> {
        self.resolved
            .resolved(path.span)
            .and_then(|id| self.definition_key(id))
    }

    /// The key for whatever an identifier resolves to.
    pub(super) fn ident_key(&self, ident: &Ident) -> Option<String> {
        self.resolved
            .resolved(ident.span)
            .and_then(|id| self.definition_key(id))
    }

    /// Prefer an explicitly registered alias method, otherwise its checked owner.
    pub(super) fn associated_owner_key(&self, path: &Path) -> Option<String> {
        let owner = path.segments.get(path.segments.len().checked_sub(2)?)?;
        let source = self.ident_key(owner).unwrap_or_else(|| owner.text.clone());
        if self
            .methods
            .contains_key(&(source.clone(), path.segments.last()?.text.clone()))
        {
            return Some(source);
        }
        self.associated_owner_ty(path)
            .and_then(|ty| self.ty_head(&ty))
            .or(Some(source))
    }

    /// Interpret a qualifier using its resolver span, including aliases and `Self`.
    pub(super) fn associated_owner_ty(&self, path: &Path) -> Option<Ty> {
        let owner = path.segments.get(path.segments.len().checked_sub(2)?)?;
        Some(self.path_ty(&Path {
            segments: vec![owner.clone()],
            span: owner.span,
        }))
    }

    /// The key for a type expression.
    pub(super) fn type_key(&self, ty: &Type) -> Option<String> {
        match ty {
            Type::Path(path) => self.path_key(path),
            Type::Generic { base, .. } | Type::Indexed { base, .. } => self.type_key(base),
            Type::View { view, target, .. } => Some(format!(
                "{}@{}",
                self.path_key(view)?,
                self.type_key(target)?
            )),
            Type::Tuple { .. } => None,
        }
    }

    /// The key for a trait path.
    pub(super) fn trait_key(&self, path: &Path) -> Option<String> {
        let id = self.resolved.resolved(path.span)?;
        self.trait_definition_key(id)
    }

    /// The key for a trait named by a type expression.
    pub(super) fn trait_type_key(&self, ty: &Type) -> Option<String> {
        self.trait_definition_key(self.trait_type_id(ty)?)
    }

    /// The trait head selected by resolution, independent of its spelling.
    pub(super) fn trait_type_id(&self, ty: &Type) -> Option<DefId> {
        match ty {
            Type::Path(path) => self.resolved.resolved(path.span),
            Type::Generic { base, .. } => self.trait_type_id(base),
            Type::Indexed { .. } | Type::View { .. } | Type::Tuple { .. } => None,
        }
    }

    /// The key for a trait definition, or `None` if the id is not a trait.
    pub(super) fn trait_definition_key(&self, id: DefId) -> Option<String> {
        let definition = self.resolved.def(id)?;
        if definition.kind == DefKind::Builtin || is_compiler_trait(self.resolved, id) {
            Some(definition.name.clone())
        } else {
            self.definition_key(id)
        }
    }

    /// The trait a blanket array impl requires of its element type, as in
    /// `impl<T: Resolve> Resolve for T[]`.
    pub(super) fn blanket_requirement(&self, im: &ImplDecl) -> Option<String> {
        let Type::Indexed { base, .. } = &im.target else {
            return None;
        };
        let parameter = type_head_name(base)?;
        let bound = im
            .params
            .params
            .iter()
            .find(|candidate| candidate.name.text == parameter)?
            .bound
            .as_ref()?;
        let trait_key = self.trait_type_key(bound)?;
        if let Some(symbol) = crate::syntax::ast::operator_trait_symbol(&trait_key) {
            return Some(symbol.to_string());
        }
        if trait_key == "CustomOperator" {
            let Type::Generic { args, .. } = bound else {
                return Some(trait_key);
            };
            args.first().and_then(|argument| match argument {
                GenericArg::Positional(Expr::StrLit { text, .. }) => Some(text.clone()),
                _ => None,
            })
        } else {
            Some(trait_key)
        }
    }

    /// The leaf name of a module-qualified key.
    pub(super) fn key_leaf<'k>(&self, key: &'k str) -> &'k str {
        key.rsplit("::").next().unwrap_or(key)
    }

    /// The key for an array family, leaving non-family names unchanged.
    pub(super) fn array_family_key(&self, name: &str) -> String {
        if self.array_families.contains(name) {
            return name.to_string();
        }
        self.array_families
            .iter()
            .find(|key| self.key_leaf(key) == name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    /// The element type head of a *plain* array operand (`Logic[3]` ->
    /// `Logic`). A nominal array family has its own head and is handled by
    /// the ordinary lookup.
    pub(super) fn array_operand_element(&self, t: &Ty) -> Option<String> {
        match t {
            Ty::Array {
                elem, family: None, ..
            } => self.ty_head(elem),
            _ => None,
        }
    }

    /// Whether `owner` implements the operator `symbol` on itself.
    pub(super) fn has_operator_impl(&self, symbol: &str, owner: &str) -> bool {
        self.operator_sigs
            .get(&(symbol.to_string(), owner.to_string()))
            .is_some_and(|sigs| {
                sigs.iter().any(|(declared, _)| {
                    declared.as_deref() == Some(owner) || declared.as_deref() == Some("Self")
                })
            })
    }

    /// A converted value's layout name. Selection uses checked contracts and
    /// declaration IDs; retain qualifiers here for source-layout construction.
    pub(super) fn conversion_key(&self, t: &Ty) -> Option<String> {
        match t {
            Ty::Array {
                family: None, elem, ..
            } => Some(format!("{}[]", self.ty_head(elem)?)),
            other => self.ty_head(other),
        }
    }

    /// The head name of a checked type, for diagnostics and impl lookup.
    pub(super) fn ty_head(&self, t: &Ty) -> Option<String> {
        Some(match t {
            Ty::Named(id) => self.definition_key(*id)?,
            Ty::Real => "real".to_string(),
            Ty::Integer => "integer".to_string(),
            Ty::Natural => "natural".to_string(),
            Ty::Char => "Char".to_string(),
            Ty::Array {
                family: Some(name), ..
            } => name.clone(),
            _ => return None,
        })
    }

    /// Compiler-produced Boolean values use the registered declaration only.
    pub(super) fn bool_ty(&self) -> Ty {
        self.resolved.lang("bool").map_or(Ty::Error, Ty::Named)
    }

    /// The checked type a head name denotes, mapping the kernel names to their
    /// built-in types.
    pub(super) fn ty_from_head(&self, name: &str) -> Ty {
        match name {
            "integer" => Ty::Integer,
            "natural" => Ty::Natural,
            "real" => Ty::Real,
            "Char" => Ty::Char,
            name if self.is_array_family(name) => Ty::Array {
                elem: Box::new(self.array_element_ty(name)),
                family: Some(name.to_string()),
                len: 0,
                low: 0,
            },
            // Only a type can be a type head: an enum variant or constant
            // that happens to share the name (a variant `Style::Logic`) must not
            // capture it.
            name => self
                .resolved
                .defs()
                .iter()
                .enumerate()
                .filter(|(_, definition)| is_type_kind(definition.kind))
                .find(|(index, _)| self.definition_key_str(DefId(*index as u32)) == Some(name))
                // Packed literals still name their library element by its
                // leaf while their classification is migrated. Prefer the
                // canonical library declaration before
                // the compatibility leaf fallback, otherwise an unrelated
                // user enum called `Logic` can retarget every such
                // value merely by appearing earlier in the module list.
                .or_else(|| {
                    self.resolved
                        .defs()
                        .iter()
                        .enumerate()
                        .find(|(_, definition)| {
                            is_type_kind(definition.kind)
                                && definition.name == name
                                && definition
                                    .module
                                    .as_deref()
                                    .is_some_and(crate::resolve::is_library_module)
                        })
                })
                .or_else(|| {
                    self.resolved
                        .defs()
                        .iter()
                        .enumerate()
                        .find(|(_, definition)| {
                            is_type_kind(definition.kind) && definition.name == name
                        })
                })
                .map(|(index, _)| Ty::Named(DefId(index as u32)))
                .unwrap_or(Ty::Error),
        }
    }

    /// The element type of an array family.
    pub(super) fn array_element_ty(&self, family: &str) -> Ty {
        self.array_elements
            .get(family)
            .map(|element| self.ty_from_head(element))
            .unwrap_or(Ty::Error)
    }

    /// The declared name of an operand's type when it is a user struct/enum
    /// (the types operator-trait impls target). `None` for intrinsics and
    /// unknowns, which keep built-in operator semantics.
    pub(super) fn named_operand_name(&self, e: &Expr, sym: &HashMap<String, Ty>) -> Option<String> {
        match self.type_of(e, sym) {
            Ty::Named(id) => {
                let d = self.resolved.def(id)?;
                matches!(d.kind, DefKind::Struct | DefKind::Enum)
                    .then(|| self.definition_key(id))?
            }
            Ty::Array {
                family: Some(name), ..
            } => Some(name),
            _ => None,
        }
    }

    /// Whether a nominal array family is represented by one packed word
    /// sequence rather than flattened aggregate fields. These newtypes retain
    /// the backend's intrinsic arithmetic fallback.
    pub(super) fn is_packed_array_newtype(&self, name: &str) -> bool {
        self.array_families.contains(name)
    }

    /// A constant initializer must lie inside a value-range-constrained
    /// numeric type (`let b: integer<0..255> = 300;` is an error). Literal
    /// bounds only; named ranges and dynamic values are runtime checks later.
    /// The declared bounds of a ranged numeric (`integer<left..right>`),
    /// resolving any alias chain (`type Byte = integer<0..255>; type Octet =
    /// Byte`). `None` for every other type.
    pub(super) fn declared_range(&self, decl_ty: &Type) -> Option<(i64, i64)> {
        let resolved = self.declarations.normalize_type(decl_ty)?;
        let t = resolved.as_ref();
        let Type::Generic { base, args, .. } = t else {
            return None;
        };
        let Type::Path(p) = base.as_ref() else {
            return None;
        };
        if p.segments.last().map(|s| s.text.as_str()) != Some("integer") {
            return None;
        }
        let [GenericArg::Positional(Expr::Range { lo, hi, .. })] = args.as_slice() else {
            return None;
        };
        Some((signed_lit(lo)?, signed_lit(hi)?))
    }
}

/// Whether a definition of this kind names a type.
fn is_type_kind(kind: DefKind) -> bool {
    matches!(
        kind,
        DefKind::Builtin
            | DefKind::Struct
            | DefKind::View
            | DefKind::Enum
            | DefKind::Entity
            | DefKind::Trait
            | DefKind::TypeAlias
    )
}
