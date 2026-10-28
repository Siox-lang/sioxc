//! Computed enum values, shared by analysis and downstream phase products.

use super::*;
use crate::types::consteval::{Binding, Environment, LocalKey, Value};

#[cfg(test)]
mod tests;

#[derive(Clone, Default)]
pub(crate) struct EnumValues {
    values: HashMap<DefId, Vec<(DefId, Option<i128>)>>,
}

impl EnumValues {
    pub(crate) fn collect(declarations: &DeclarationIndex<'_>, resolved: &Resolved) -> Self {
        Self::collect_with_functions(declarations, resolved, None)
    }

    pub(crate) fn collect_with_functions(
        declarations: &DeclarationIndex<'_>,
        resolved: &Resolved,
        functions: Option<&DeclarationIndex<'_>>,
    ) -> Self {
        let mut result = Self::default();
        result.refine(declarations, resolved, functions);
        result
    }

    fn refine(
        &mut self,
        declarations: &DeclarationIndex<'_>,
        resolved: &Resolved,
        functions: Option<&DeclarationIndex<'_>>,
    ) {
        let sources: Vec<_> = declarations
            .enumerations()
            .filter_map(|(owner, _)| {
                declarations
                    .enum_variants(owner)
                    .map(|members| (owner, members))
            })
            .collect();
        for (owner, members) in &sources {
            self.values
                .entry(*owner)
                .or_insert_with(|| members.iter().map(|(id, _)| (*id, None)).collect());
        }
        // Only resolved facts advance the fixed point; an unknown explicit value
        // makes following implicit values unknown, not zero-based guesses.
        loop {
            let mut changed = false;
            for (owner, members) in &sources {
                let mut next = Some(0i128);
                for (position, (_, member)) in members.iter().enumerate() {
                    if let Some(value) = self.values[owner][position].1 {
                        next = value.checked_add(1);
                        continue;
                    }
                    let value = match &member.value {
                        Some(expression) => consteval::evaluate(
                            expression,
                            &Discriminants {
                                declarations,
                                resolved,
                                enums: self,
                                functions,
                            },
                            0,
                        )
                        .ok(),
                        None => next,
                    };
                    next = value.and_then(|value| value.checked_add(1));
                    if let Some(value) = value {
                        let entry = &mut self.values.get_mut(owner).unwrap()[position].1;
                        if entry.is_none() {
                            *entry = Some(value);
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn has_unknown(&self) -> bool {
        self.values
            .values()
            .flatten()
            .any(|(_, value)| value.is_none())
    }

    pub(crate) fn check_declaration(
        &self,
        declaration: &EnumDecl,
        resolved: &Resolved,
        sink: &mut DiagnosticSink,
        previously_known: Option<&HashSet<(DefId, DefId)>>,
    ) {
        let Some(owner) = resolved.declared(declaration.name.span) else {
            return;
        };
        let mut seen: HashMap<i128, (&str, DefId)> = HashMap::new();
        for variant in &declaration.variants {
            let Some(id) = resolved.declared(variant.name.span) else {
                continue;
            };
            let Some(value) = self.number(owner, id) else {
                continue;
            };
            let newly_known =
                |id| previously_known.is_none_or(|known| !known.contains(&(owner, id)));
            if value < 0 {
                if newly_known(id) {
                    sink.emit(Diagnostic::error(format!(
                        "`{}::{}` has the negative discriminant {value}; an enum's discriminants start at 0",
                        declaration.name.text, variant.name.text,
                    )).with_code(codes::TYPE_MISMATCH)
                        .at(variant.value.as_ref().map(expr_span).unwrap_or(variant.name.span)));
                }
                continue;
            }
            if let Some((previous, previous_id)) = seen.insert(value, (&variant.name.text, id)) {
                if newly_known(id) || newly_known(previous_id) {
                    sink.emit(
                        Diagnostic::error(format!(
                            "`{}::{}` and `{}::{previous}` both have the value {value}",
                            declaration.name.text, variant.name.text, declaration.name.text,
                        ))
                        .with_code(codes::DUPLICATE_ITEM)
                        .at(variant.name.span),
                    );
                }
            }
        }
    }

    pub(crate) fn members(&self, owner: DefId) -> Option<&[(DefId, Option<i128>)]> {
        self.values.get(&owner).map(Vec::as_slice)
    }

    pub(crate) fn owners(&self) -> impl Iterator<Item = DefId> + '_ {
        self.values.keys().copied()
    }

    pub(crate) fn number(&self, owner: DefId, variant: DefId) -> Option<i128> {
        self.members(owner)?
            .iter()
            .find(|(id, _)| *id == variant)?
            .1
    }

    pub(crate) fn value(&self, owner: DefId, variant: DefId) -> Option<u64> {
        let value = self.number(owner, variant)?;
        u64::try_from(value).ok()
    }

    pub(crate) fn width(&self, owner: DefId) -> Option<u32> {
        let members = self.members(owner)?;
        let largest = members.iter().try_fold(0u128, |largest, (_, value)| {
            Some(largest.max(u128::try_from((*value)?).ok()?))
        })?;
        let largest = largest.max(members.len().saturating_sub(1) as u128);
        Some((u128::BITS - largest.leading_zeros()).max(1))
    }
}

/// Refine facts after semantic-preserving attribute/length normalization.
pub fn refresh_enum_values(
    modules: &[Module],
    resolved: &Resolved,
    typed: &mut Typed,
    sink: &mut DiagnosticSink,
) {
    if !typed.enum_values.has_unknown() {
        return;
    }
    let known: HashSet<_> = typed
        .enum_values
        .values
        .iter()
        .flat_map(|(&owner, members)| {
            members
                .iter()
                .filter_map(move |(variant, value)| value.map(|_| (owner, *variant)))
        })
        .collect();
    let declarations = DeclarationIndex::collect(resolved, modules);
    let values = std::sync::Arc::make_mut(&mut typed.enum_values);
    values.refine(&declarations, resolved, None);
    // Diagnostics retain source order, independent of the fact table's hashing.
    for item in modules.iter().flat_map(|module| &module.items) {
        if let Item::Enum(declaration) = item {
            values.check_declaration(declaration, resolved, sink, Some(&known));
        }
    }
}

struct Discriminants<'i, 'a> {
    declarations: &'i DeclarationIndex<'a>,
    functions: Option<&'i DeclarationIndex<'a>>,
    resolved: &'i Resolved,
    enums: &'i EnumValues,
}

impl Discriminants<'_, '_> {
    fn named(&self, owner: DefId, name: &str) -> Option<Value> {
        Some(Value::Variant {
            owner,
            variant: self.resolved.enum_variant(owner, name)?,
        })
    }

    fn boolean(&self, value: bool) -> Option<Value> {
        self.named(
            self.resolved.lang("bool")?,
            if value { "true" } else { "false" },
        )
    }

    fn supported(&self, ty: &Type) -> bool {
        let Some(id) = self.declarations.scalar_type_id(ty) else {
            return false;
        };
        self.resolved.kind_of(id) == Some(DefKind::Enum)
            || self.resolved.def(id).is_some_and(|definition| {
                definition.kind == DefKind::Builtin
                    && matches!(definition.name.as_str(), "integer" | "Char")
            })
    }

    fn boolean_operator(&self, role: &str, arity: usize) -> Option<&FnDecl> {
        let owner = self.resolved.lang("bool")?;
        let trait_id = self.resolved.lang(role)?;
        let mut functions = self
            .declarations
            .implementations_for(owner)
            .chain(
                self.functions
                    .into_iter()
                    .flat_map(|functions| functions.implementations_for(owner)),
            )
            .filter(|implementation| {
                implementation
                    .trait_
                    .as_ref()
                    .and_then(|path| self.resolved.resolved(path.span))
                    == Some(trait_id)
            })
            .flat_map(|implementation| &implementation.items)
            .filter_map(|item| match item {
                ImplItem::Fn(function)
                    if function.name.text == role
                        && function.params.len() == arity
                        && function
                            .params
                            .first()
                            .is_some_and(|parameter| parameter.is_self)
                        && function
                            .ret
                            .as_ref()
                            .and_then(|ty| self.enum_type(ty, Some(owner)))
                            == Some(owner)
                        && function
                            .params
                            .iter()
                            .filter(|parameter| !parameter.is_self)
                            .all(|parameter| {
                                parameter
                                    .ty
                                    .as_ref()
                                    .and_then(|ty| self.enum_type(ty, Some(owner)))
                                    == Some(owner)
                            }) =>
                {
                    Some(function)
                }
                _ => None,
            });
        let function = functions.next()?;
        functions.next().is_none().then_some(function)
    }
}

impl Environment for Discriminants<'_, '_> {
    fn transparent_conversion(&self, callee: &Expr) -> bool {
        let Expr::Path(path) = callee else {
            return false;
        };
        self.resolved
            .resolved(path.span)
            .and_then(|id| self.resolved.def(id))
            .is_some_and(|definition| {
                definition.kind == DefKind::Builtin && definition.name == "integer"
            })
    }

    fn binding(&self, path: &Path) -> Option<Binding<'_>> {
        if let Some(constant) = self.declarations.constant_path(path).or_else(|| {
            self.functions
                .and_then(|functions| functions.constant_path(path))
        }) {
            if !self.supported(&constant.ty) {
                return None;
            }
            return Some(Binding::TypedExpression {
                value: &constant.value,
                enum_owner: self.enum_type(&constant.ty, None),
            });
        }
        let owner = self.resolved.enum_owner(path)?;
        let variant = self.resolved.resolved(path.span)?;
        Some(Binding::Value(Value::Variant { owner, variant }))
    }

    fn local_key<'p>(&self, path: &'p Path) -> Option<LocalKey<'p>> {
        self.declarations.local_id(path).map(LocalKey::Definition)
    }

    fn parameter_key<'p>(&self, parameter: &'p FnParam) -> Option<LocalKey<'p>> {
        self.declarations
            .parameter_id(parameter)
            .map(LocalKey::Definition)
    }

    fn enum_type(&self, ty: &Type, receiver: Option<DefId>) -> Option<DefId> {
        let id = self.declarations.scalar_type_id(ty)?;
        if self.resolved.kind_of(id) == Some(DefKind::Enum) {
            return Some(id);
        }
        self.resolved
            .def(id)
            .filter(|definition| {
                definition.kind == DefKind::Local
                    && definition.name == "Self"
                    && definition.span.is_none()
            })
            .and(receiver)
    }

    fn enum_literal(&self, ch: char, owner: DefId) -> Option<Value> {
        self.named(owner, &format!("'{ch}'"))
    }

    fn function(&self, callee: &Expr) -> Option<&FnDecl> {
        let Expr::Path(path) = callee else {
            return None;
        };
        let function = self.declarations.function_path(path).or_else(|| {
            self.functions
                .and_then(|functions| functions.function_path(path))
        })?;
        (function.generics.params.is_empty()
            && function.ret.as_ref().is_some_and(|ty| self.supported(ty))
            && function.params.iter().all(|parameter| {
                !parameter.is_self && parameter.ty.as_ref().is_some_and(|ty| self.supported(ty))
            }))
        .then_some(function)
    }

    fn binary_value(&self, expression: &Expr, left: Value, right: Value) -> Option<Value> {
        let Expr::Binary { op, .. } = expression else {
            return None;
        };
        let order = match (left, right) {
            (Value::Integer(left), Value::Integer(right)) => left.cmp(&right),
            (Value::Character(left), Value::Character(right)) => left.cmp(&right),
            (
                Value::Variant {
                    owner: a,
                    variant: left,
                },
                Value::Variant {
                    owner: b,
                    variant: right,
                },
            ) if a == b => match op {
                BinOp::Eq | BinOp::Ne => {
                    return self.boolean(if matches!(op, BinOp::Eq) {
                        left == right
                    } else {
                        left != right
                    });
                }
                _ => self
                    .enums
                    .number(a, left)?
                    .cmp(&self.enums.number(b, right)?),
            },
            (
                Value::Enum {
                    owner: a,
                    discriminant: left,
                },
                Value::Enum {
                    owner: b,
                    discriminant: right,
                },
            ) if a == b => left.cmp(&right),
            _ => return None,
        };
        self.boolean(match op {
            BinOp::Eq => order.is_eq(),
            BinOp::Ne => !order.is_eq(),
            BinOp::Lt => order.is_lt(),
            BinOp::Le => !order.is_gt(),
            BinOp::Gt => order.is_gt(),
            BinOp::Ge => !order.is_lt(),
            _ => return None,
        })
    }

    fn condition_value(&self, value: Value) -> Option<bool> {
        let owner = self.resolved.lang("bool")?;
        if Some(value) == self.named(owner, "true") {
            Some(true)
        } else if Some(value) == self.named(owner, "false") {
            Some(false)
        } else {
            None
        }
    }

    fn operator_function(
        &self,
        expression: &Expr,
        left: Value,
        right: Option<Value>,
    ) -> Option<&FnDecl> {
        if left.enum_owner() != self.resolved.lang("bool")
            || right.is_some_and(|value| value.enum_owner() != left.enum_owner())
        {
            return None;
        }
        match expression {
            Expr::Binary { op: BinOp::And, .. } => self.boolean_operator("and", 2),
            Expr::Binary { op: BinOp::Or, .. } => self.boolean_operator("or", 2),
            Expr::Unary { op: UnOp::Not, .. } => self.boolean_operator("not", 1),
            _ => None,
        }
    }
}
