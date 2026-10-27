//! Borrow identity-selected phase facts for the shared typed interpreter.

use super::*;
use crate::resolve::DefKind;
use crate::types::consteval::{self, Binding, Environment, LocalKey, Value};

struct EnumTable<'a> {
    key: String,
    values: &'a HashMap<String, u64>,
}

pub(super) struct EnumConstants<'l, 'a> {
    lowering: &'l Lowering<'a>,
    tables: HashMap<DefId, EnumTable<'l>>,
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
                let values = lowering.enum_variants.get(&key)?;
                Some((id, EnumTable { key, values }))
            })
            .collect();
        Self { lowering, tables }
    }

    pub(super) fn owner(&self, key: &str) -> Option<DefId> {
        let mut owners = self
            .tables
            .iter()
            .filter_map(|(&id, table)| (table.key == key).then_some(id));
        let owner = owners.next()?;
        owners.next().is_none().then_some(owner)
    }

    pub(super) fn constant(&self, declaration: &ast::ConstDecl) -> Option<Value> {
        let owner = self.enum_type(&declaration.ty, None);
        if owner.is_none() && !self.primitive_type(&declaration.ty, "Char") {
            return None;
        }
        consteval::evaluate_value(&declaration.value, self, owner, 0).ok()
    }

    pub(super) fn predicate(
        &self,
        function: &ast::FnDecl,
        owner: DefId,
        value: u64,
    ) -> Option<bool> {
        self.evaluate(function, owner, value, None)
            .and_then(|result| self.condition_value(result))
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
        functions: &[(&ast::FnDecl, Option<String>)],
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
            .filter_map(|(function, _)| self.matches(function, &values).then_some(*function));
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
        consteval::evaluate_function(function, values, self, 0).ok()
    }

    fn type_key(&self, value: Value) -> Option<&str> {
        match value {
            Value::Integer(_) => Some("integer"),
            Value::Character(_) => Some("Char"),
            Value::Enum { owner, .. } => self.tables.get(&owner).map(|table| table.key.as_str()),
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
                        Value::Enum { owner, .. } => self.enum_type(ty, receiver) == Some(*owner),
                        Value::Integer(_) => self.primitive_type(ty, "integer"),
                        Value::Character(_) => self.primitive_type(ty, "Char"),
                    }
                })
    }

    fn hook(&self, hook: &str, method: &str, values: &[Value]) -> Option<&'a ast::FnDecl> {
        let key = self.type_key(*values.first()?)?;
        let mut candidates =
            self.lowering
                .op_impls
                .get(hook, key)?
                .iter()
                .filter_map(|(function, _)| {
                    (function.name.text == method && self.matches(function, values))
                        .then_some(*function)
                });
        let function = candidates.next()?;
        candidates.next().is_none().then_some(function)
    }

    fn boolean(&self, condition: bool) -> Option<Value> {
        let owner = self.lowering.resolved.lang("bool")?;
        let discriminant =
            *self
                .tables
                .get(&owner)?
                .values
                .get(if condition { "true" } else { "false" })?;
        Some(Value::Enum {
            owner,
            discriminant,
        })
    }
}

impl Environment for EnumConstants<'_, '_> {
    fn binding(&self, path: &ast::Path) -> Option<Binding<'_>> {
        if let Some(constant) = self.lowering.free_fns.declarations().constant_path(path) {
            if !self.supported_type(&constant.ty, None) {
                return None;
            }
            return Some(Binding::TypedExpression {
                value: &constant.value,
                enum_owner: self.enum_type(&constant.ty, None),
            });
        }
        let owner = self.lowering.resolved.enum_owner(path)?;
        let variant = self
            .lowering
            .resolved
            .def(self.lowering.resolved.resolved(path.span)?)?;
        let discriminant = *self.tables.get(&owner)?.values.get(&variant.name)?;
        Some(Binding::Value(Value::Enum {
            owner,
            discriminant,
        }))
    }

    fn domain(&self) -> consteval::IntegerDomain {
        consteval::IntegerDomain::NativeWord
    }

    fn local_key<'p>(&self, path: &'p ast::Path) -> Option<LocalKey<'p>> {
        self.lowering
            .free_fns
            .declarations()
            .local_id(path)
            .map(LocalKey::Definition)
    }

    fn parameter_key<'p>(&self, parameter: &'p ast::FnParam) -> Option<LocalKey<'p>> {
        self.lowering
            .free_fns
            .declarations()
            .parameter_id(parameter)
            .map(LocalKey::Definition)
    }

    fn enum_type(&self, ty: &ast::Type, receiver: Option<DefId>) -> Option<DefId> {
        let normalized = self.lowering.free_fns.declarations().normalize_type(ty)?;
        let ast::Type::Path(path) = normalized.as_ref() else {
            return None;
        };
        let id = self.lowering.resolved.resolved(path.span)?;
        let definition = self.lowering.resolved.def(id)?;
        if definition.kind == DefKind::Local
            && definition.name == "Self"
            && definition.span.is_none()
        {
            receiver
        } else {
            self.tables.contains_key(&id).then_some(id)
        }
    }

    fn enum_literal(&self, ch: char, owner: DefId) -> Option<Value> {
        let discriminant = *self.tables.get(&owner)?.values.get(&format!("'{ch}'"))?;
        Some(Value::Enum {
            owner,
            discriminant,
        })
    }

    fn function(&self, callee: &ast::Expr) -> Option<&ast::FnDecl> {
        let function = self.lowering.free_fns.get(callee)?;
        self.supported_function(function, None).then_some(function)
    }

    fn method_function(&self, method: &ast::Ident, receiver: Value) -> Option<&ast::FnDecl> {
        let key = self.type_key(receiver)?;
        let mut inherent = self
            .lowering
            .inherent_impls
            .get(key)
            .into_iter()
            .flatten()
            .flat_map(|implementation| &implementation.items)
            .filter_map(|item| {
                let ast::ImplItem::Fn(function) = item else {
                    return None;
                };
                (function.name.text == method.text
                    && self.supported_function(function, receiver.enum_owner())
                    && function
                        .params
                        .first()
                        .is_some_and(|parameter| parameter.is_self))
                .then_some(function)
            });
        if let Some(function) = inherent.next() {
            return inherent.next().is_none().then_some(function);
        }
        let mut candidates = self
            .lowering
            .op_impls
            .iter()
            .filter(|(_, owner, _)| *owner == key)
            .flat_map(|(_, _, methods)| methods)
            .filter_map(|(function, _)| {
                (function.name.text == method.text
                    && self.supported_function(function, receiver.enum_owner())
                    && function
                        .params
                        .first()
                        .is_some_and(|parameter| parameter.is_self))
                .then_some(*function)
            });
        let function = candidates.next()?;
        candidates.next().is_none().then_some(function)
    }

    fn operator_function(
        &self,
        expression: &ast::Expr,
        left: Value,
        right: Option<Value>,
    ) -> Option<&ast::FnDecl> {
        let (hook, method) = match expression {
            ast::Expr::Unary {
                op: ast::UnOp::Not, ..
            } => ("not", "not"),
            ast::Expr::Unary {
                op: ast::UnOp::Neg, ..
            } => ("Neg", "neg"),
            ast::Expr::Binary { op, .. } => match op {
                ast::BinOp::And => ("and", "and"),
                ast::BinOp::Or => ("or", "or"),
                ast::BinOp::Add => ("+", "add"),
                ast::BinOp::Sub => ("-", "sub"),
                ast::BinOp::Mul => ("*", "mul"),
                ast::BinOp::Div => ("/", "div"),
                ast::BinOp::Rem => ("%", "rem"),
                ast::BinOp::Shl => ("<<", "shl"),
                ast::BinOp::Shr => (">>", "shr"),
                ast::BinOp::Custom { symbol, .. } => (symbol.as_str(), "apply"),
                _ => return None,
            },
            _ => return None,
        };
        match right {
            Some(right) => self.hook(hook, method, &[left, right]),
            None => self.hook(hook, method, &[left]),
        }
    }

    fn condition_value(&self, value: Value) -> Option<bool> {
        let Value::Enum {
            owner,
            discriminant,
        } = value
        else {
            return None;
        };
        if self.lowering.resolved.lang("bool") != Some(owner) {
            return None;
        }
        let values = self.tables.get(&owner)?.values;
        if values.get("true") == Some(&discriminant) {
            Some(true)
        } else if values.get("false") == Some(&discriminant) {
            Some(false)
        } else {
            None
        }
    }

    fn condition_function(&self, value: Value) -> Option<&ast::FnDecl> {
        self.hook("Boolean", "as_bool", &[value])
    }

    fn binary_value(&self, expression: &ast::Expr, left: Value, right: Value) -> Option<Value> {
        let ast::Expr::Binary { op, .. } = expression else {
            return None;
        };
        let ordering = match (left, right) {
            (Value::Integer(left), Value::Integer(right)) => left.cmp(&right),
            (Value::Character(left), Value::Character(right)) => left.cmp(&right),
            (
                Value::Enum {
                    owner: left_owner,
                    discriminant: left,
                },
                Value::Enum {
                    owner: right_owner,
                    discriminant: right,
                },
            ) if left_owner == right_owner => left.cmp(&right),
            _ => return None,
        };
        self.boolean(match op {
            ast::BinOp::Eq => ordering.is_eq(),
            ast::BinOp::Ne => !ordering.is_eq(),
            ast::BinOp::Lt => ordering.is_lt(),
            ast::BinOp::Le => !ordering.is_gt(),
            ast::BinOp::Gt => ordering.is_gt(),
            ast::BinOp::Ge => !ordering.is_lt(),
            _ => return None,
        })
    }
}
