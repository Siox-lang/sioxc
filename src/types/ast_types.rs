//! Mapping written AST types to checked `Ty`, and diagnostic emission.

use super::*;

impl<'a> Checker<'a> {
    /// Convert an AST type into a checked type.
    pub(super) fn ast_ty(&self, t: &Type) -> Ty {
        match t {
            Type::Path(p) => self.path_ty(p),
            Type::Indexed { base, index, .. } => {
                // Unconstrained (`Char[]`): width 0 = "set at use".
                let width = index.as_deref().map(width_of).unwrap_or(0);
                match self.ast_ty(base) {
                    // The *first* index on a nominal array family sets its width
                    // (`unsigned[8]`). A *second* index makes an array of those
                    // vectors (`unsigned[8][4]` = 4 elements, each 8 wide).
                    Ty::Array {
                        elem,
                        len: 0,
                        family: Some(family),
                    } => Ty::Array {
                        elem,
                        len: width,
                        family: Some(family),
                    },
                    v @ Ty::Array {
                        family: Some(_), ..
                    } => Ty::Array {
                        elem: Box::new(v),
                        len: width,
                        family: None,
                    },
                    // An index on an *unconstrained* array fills its hole
                    // rather than nesting: `string[5]` is `Char[5]`, not
                    // `Char[0][5]` (`using string = Char[]`, std::text). The
                    // lowerer already did this; the checker rejected the form.
                    Ty::Array {
                        elem,
                        len: 0,
                        family: None,
                    } => Ty::Array {
                        elem,
                        len: width,
                        family: None,
                    },
                    other => Ty::Array {
                        elem: Box::new(other),
                        len: width,
                        family: None,
                    },
                }
            }
            Type::Generic { base, .. } => self.ast_ty(base),
            Type::View { view, .. } => self.path_ty(view),
        }
    }

    /// Interpret a collected method signature at a call site. Signatures are
    /// stored as source `Type`s, so a direct `Self` must be rebound to the
    /// receiver/associated owner rather than the impl-local resolver symbol.
    pub(super) fn ast_ty_for_owner(&self, t: &Type, owner: &Ty) -> Ty {
        if matches!(t, Type::Path(path) if path.segments.len() == 1 && path.segments[0].text == "Self")
        {
            owner.clone()
        } else {
            self.ast_ty(t)
        }
    }

    /// A resolved type-name span as a `Ty`. A **type parameter** (`T` in a
    /// generic entity/struct/impl) is opaque, so it types as `Error` — it
    /// suppresses the assignment/type checks that can't be meaningful until the
    /// parameter is bound at elaboration.
    pub(super) fn named_ty(&self, span: Span) -> Ty {
        match self.resolved.resolved(span) {
            Some(id) if self.resolved.def(id).map(|d| d.kind) == Some(DefKind::Param) => Ty::Error,
            Some(id) => Ty::Named(id),
            None => Ty::Error,
        }
    }

    /// Convert a type path into a checked type, mapping the kernel names
    /// directly.
    pub(super) fn path_ty(&self, p: &Path) -> Ty {
        if p.segments.len() == 1 {
            match p.segments[0].text.as_str() {
                "integer" => Ty::Integer,
                "real" => Ty::Real,
                "Char" => Ty::Char,
                "Self" => self
                    .current_self_ty
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| self.named_ty(p.span)),
                // Elaboration-time range constants (`const BYTE: range`);
                // opaque to value checking.
                "range" => Ty::Error,
                _ => self.named_path_ty(p),
            }
        } else {
            self.named_path_ty(p)
        }
    }

    /// Convert a path naming a nominal type into a checked type.
    pub(super) fn named_path_ty(&self, path: &Path) -> Ty {
        let Some(key) = self.path_key(path) else {
            return self.named_ty(path.span);
        };
        match self.aliases.get(&key) {
            // A cyclic alias has no type; `Error` also suppresses the
            // follow-on diagnostics the cycle would otherwise cause.
            Some(_) if !self.expanding.borrow_mut().insert(key.clone()) => Ty::Error,
            Some(ty) => {
                let ty = ty.clone();
                let result = self.ast_ty(&ty);
                self.expanding.borrow_mut().remove(&key);
                result
            }
            // A nominal array family (`struct F(Logic[])`): the first index
            // supplies its unconstrained base length.
            None if self.is_array_family(&key) => Ty::Array {
                elem: Box::new(self.array_element_ty(&key)),
                family: Some(key),
                len: 0,
            },
            None => self.named_ty(path.span),
        }
    }

    /// Resolve a `using` alias chain transitively. Cycles stop the walk and
    /// return `None` so callers can suppress follow-on diagnostics.
    pub(super) fn resolve_alias_type(&self, ty: &Type) -> Option<Type> {
        let mut current = ty.clone();
        let mut seen = HashSet::new();
        loop {
            let Type::Path(p) = &current else {
                return Some(current);
            };
            let key = self.path_key(p)?;
            if !seen.insert(key.clone()) {
                return None;
            }
            let Some(alias) = self.aliases.get(&key) else {
                return Some(current);
            };
            current = alias.clone();
        }
    }

    /// Emit an error diagnostic with a stable code at `span`.
    pub(super) fn error(&mut self, code: &'static str, span: Span, msg: String) {
        self.sink
            .emit(Diagnostic::error(msg).with_code(code).at(span));
    }

    /// Emit an error diagnostic carrying a suggested fix.
    pub(super) fn error_with_help(
        &mut self,
        code: &'static str,
        span: Span,
        msg: String,
        help: String,
    ) {
        self.sink
            .emit(Diagnostic::error(msg).with_code(code).at(span).help(help));
    }

    /// Emit a warning diagnostic carrying a suggested fix.
    pub(super) fn warn(&mut self, code: &'static str, span: Span, msg: String, help: &str) {
        self.sink.emit(
            Diagnostic::warning(msg)
                .with_code(code)
                .at(span)
                .help(help.to_string()),
        );
    }

    /// The enum name if `t` is a symbolic enum value (`Bit`/`Logic`/`Bool` or a
    /// user `enum`) — the types whose values are written as char/variant
    /// literals, not numbers. `None` for numerics (`unsigned`/`signed`/`integer`/
    /// `real`), `Char`, and non-enums.
    pub(super) fn enum_operand_name(&self, t: &Ty) -> Option<String> {
        match t {
            Ty::Named(id) => {
                let d = self.resolved.def(*id)?;
                matches!(d.kind, DefKind::Enum).then(|| self.definition_key(*id))?
            }
            _ => None,
        }
    }
}
