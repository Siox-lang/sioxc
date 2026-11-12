//! Borrow identity-selected phase facts for the shared typed interpreter.

use super::*;
use crate::resolve::DefKind;
use crate::types::consteval::{self, Environment, Value};

pub(super) struct EnumConstants<'l, 'a> {
    lowering: &'l Lowering<'a>,
    tables: HashMap<DefId, String>,
}

impl<'l, 'a> EnumConstants<'l, 'a> {
    pub(super) fn new(lowering: &'l Lowering<'a>) -> Self {
        let tables = lowering
            .resolved
            .defs()
            .iter()
            .enumerate()
            .filter_map(|(index, definition)| {
                if definition.kind != DefKind::Enum {
                    return None;
                }
                let id = DefId(index as u32);
                let key = lowering.free_fns.enum_id_key(id)?;
                lowering.enum_variants.get(&key)?;
                Some((id, key))
            })
            .collect();
        Self { lowering, tables }
    }

    pub(super) fn owner(&self, key: &str) -> Option<DefId> {
        let mut owners = self
            .tables
            .iter()
            .filter_map(|(&id, table)| (table == key).then_some(id));
        let owner = owners.next()?;
        owners.next().is_none().then_some(owner)
    }

    pub(super) fn constant(&self, declaration: &ast::ConstDecl) -> Option<Value> {
        let owner = self.enum_type(&declaration.ty, None);
        if owner.is_none() && !self.primitive_type(&declaration.ty, "Char") {
            return None;
        }
        let value = consteval::evaluate_value(&declaration.value, self, owner, 0);
        self.stored(value.ok()?)
    }

    pub(super) fn predicate(
        &self,
        function: &ast::FnDecl,
        owner: DefId,
        value: u64,
    ) -> Option<bool> {
        self.evaluate(function, owner, value, None)
            .and_then(|result| self.condition_value(&result))
    }

    pub(super) fn enumeration(
        &self,
        function: &ast::FnDecl,
        owner: DefId,
        left: u64,
        right: Option<u64>,
    ) -> Option<u64> {
        match self.evaluate(function, owner, left, right) {
            Some(Value::Enum {
                owner: actual,
                discriminant,
            }) if owner == actual => Some(discriminant),
            _ => None,
        }
    }

    pub(super) fn binary(
        &self,
        functions: &[(&ast::FnDecl, Option<String>, &ast::ImplDecl)],
        owner: DefId,
        left: u64,
        right: u64,
    ) -> Option<u64> {
        let values = [
            Value::Enum {
                owner,
                discriminant: left,
            },
            Value::Enum {
                owner,
                discriminant: right,
            },
        ];
        let mut matching = functions
            .iter()
            .filter_map(|(function, _, _)| self.matches(function, &values).then_some(*function));
        let function = matching.next()?;
        if matching.next().is_some() {
            return None;
        }
        self.enumeration(function, owner, left, Some(right))
    }

    fn evaluate(
        &self,
        function: &ast::FnDecl,
        owner: DefId,
        left: u64,
        right: Option<u64>,
    ) -> Option<Value> {
        let mut values = vec![Value::Enum {
            owner,
            discriminant: left,
        }];
        values.extend(right.map(|discriminant| Value::Enum {
            owner,
            discriminant,
        }));
        if !self.matches(function, &values) {
            return None;
        }
        let value = consteval::evaluate_function(function, values, self, 0);
        self.stored(value.ok()?)
    }

    fn stored(&self, value: Value) -> Option<Value> {
        if let Value::Variant { owner, .. } = value {
            Some(Value::Enum {
                owner,
                discriminant: self.lowering.free_fns.constant_source().word(value)? as u64,
            })
        } else {
            Some(value)
        }
    }

    fn primitive_type(&self, ty: &ast::Type, name: &str) -> bool {
        let Some(ty) = self.lowering.free_fns.declarations().normalize_type(ty) else {
            return false;
        };
        let ast::Type::Path(path) = ty.as_ref() else {
            return false;
        };
        self.lowering
            .resolved
            .resolved(path.span)
            .and_then(|id| self.lowering.resolved.def(id))
            .is_some_and(|definition| {
                definition.kind == DefKind::Builtin && definition.name == name
            })
    }

    fn supported_type(&self, ty: &ast::Type, receiver: Option<DefId>) -> bool {
        self.enum_type(ty, receiver).is_some()
            || self.primitive_type(ty, "integer")
            || self.primitive_type(ty, "Char")
    }

    fn supported_function(&self, function: &ast::FnDecl, receiver: Option<DefId>) -> bool {
        function.generics.params.is_empty()
            && function
                .ret
                .as_ref()
                .is_some_and(|ty| self.supported_type(ty, receiver))
            && function.params.iter().all(|parameter| {
                parameter.is_self
                    || parameter
                        .ty
                        .as_ref()
                        .is_some_and(|ty| self.supported_type(ty, receiver))
            })
    }

    fn matches(&self, function: &ast::FnDecl, values: &[Value]) -> bool {
        let receiver = values.first().and_then(|value| value.enum_owner());
        self.supported_function(function, receiver)
            && function.params.len() == values.len()
            && function
                .params
                .iter()
                .zip(values)
                .all(|(parameter, value)| {
                    if parameter.is_self {
                        return true;
                    }
                    let Some(ty) = &parameter.ty else {
                        return false;
                    };
                    match value {
                        Value::Enum { owner, .. } | Value::Variant { owner, .. } => {
                            self.enum_type(ty, receiver) == Some(*owner)
                        }
                        Value::Integer(_) => self.primitive_type(ty, "integer"),
                        Value::Character(_) => self.primitive_type(ty, "Char"),
                        Value::Natural(_)
                        | Value::Real(_)
                        | Value::Array { .. }
                        | Value::Record { .. }
                        | Value::Tuple(_) => false,
                    }
                })
    }
}

impl Environment for EnumConstants<'_, '_> {
    fn source(&self) -> Option<consteval::Source<'_, '_>> {
        Some(self.lowering.free_fns.constant_source())
    }

    fn domain(&self) -> consteval::IntegerDomain {
        consteval::IntegerDomain::NativeWord
    }

    fn function(&self, callee: &ast::Expr) -> Option<&ast::FnDecl> {
        let function = self.lowering.free_fns.get(callee)?;
        self.supported_function(function, None).then_some(function)
    }
}
