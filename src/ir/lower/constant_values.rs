//! Borrow identity-selected phase facts for the shared typed interpreter.

use super::*;
use crate::resolve::DefKind;
use crate::types::consteval::{self, Environment, Value};

impl Lowering<'_> {
    /// Consume the interpreter's checked value, rather than re-lowering its AST.
    pub(super) fn fold_typed_constant(
        &mut self,
        name: &str,
        declaration: &ast::ConstDecl,
        scope: &HashMap<String, i64>,
    ) -> bool {
        let Some(value) = eval_declared_const(declaration, scope, &self.free_fns) else {
            return false;
        };
        let range = self
            .free_fns
            .constant_source()
            .range_bounds(&value)
            .and_then(|(left, right)| {
                Some((i64::try_from(left).ok()?, i64::try_from(right).ok()?))
            });
        let mut layout = self.source_layout(&declaration.ty, scope);
        let Some(value) = self.materialize_constant(&value, &mut layout) else {
            if let Some(range) = range {
                self.const_ranges.insert(name.to_string(), range);
                return true;
            }
            return false;
        };
        if let Some(range) = range {
            self.const_ranges.insert(name.to_string(), range);
        }
        match &value {
            Val::Scalar(value) => match value {
                Expr::Const(word) => {
                    let signed = matches!(
                        layout.kind,
                        LayoutKind::Scalar {
                            domain: ScalarDomain::Integer,
                            ..
                        }
                    );
                    if signed {
                        self.consts.insert(name.to_string(), *word as i64);
                    } else if let Ok(word) = i64::try_from(*word) {
                        self.consts.insert(name.to_string(), word);
                    }
                }
                Expr::Real(value) => {
                    self.consts_real.insert(name.to_string(), *value);
                }
                _ => return false,
            },
            Val::Fields(fields) => {
                for (field, value) in fields {
                    if let Expr::Const(word) = value {
                        if let Ok(word) = i64::try_from(*word) {
                            let separator = if field.starts_with('[') { "" } else { "." };
                            self.consts
                                .insert(format!("{name}{separator}{field}"), word);
                        }
                    }
                }
            }
        }
        let value = self.bind_value_layout(value, layout, ast::expr_span(&declaration.value));
        match value {
            Val::Scalar(value) => {
                self.const_values.insert(name.to_string(), value);
                true
            }
            Val::Fields(fields) => self.insert_const_leaves(name, fields),
        }
    }

    fn materialize_constant(&self, mut value: &Value, layout: &mut SourceLayout) -> Option<Val> {
        // Newtype identity was checked by the interpreter. Its one base value
        // occupies the same physical layout, including record-backed newtypes.
        while let Value::Record { owner, fields } = value {
            let declaration = self.free_fns.declarations().structure(*owner)?;
            if declaration.base.is_none() {
                break;
            }
            if !declaration.fields.is_empty() {
                return None;
            }
            let [base] = fields.as_ref() else { return None };
            value = base;
        }
        match (value, &mut layout.kind) {
            (
                Value::Integer(value),
                LayoutKind::Scalar {
                    domain: ScalarDomain::Integer,
                    ..
                },
            ) => Some(Val::Scalar(Expr::Const(i64::try_from(*value).ok()? as u64))),
            (
                Value::Natural(value),
                LayoutKind::Scalar {
                    domain: ScalarDomain::Natural,
                    ..
                },
            ) => Some(Val::Scalar(Expr::Const(*value))),
            (
                Value::Real(value),
                LayoutKind::Scalar {
                    domain: ScalarDomain::Real,
                    ..
                },
            ) => Some(Val::Scalar(Expr::Real(*value))),
            (
                Value::Character(value),
                LayoutKind::Scalar {
                    domain: ScalarDomain::Character,
                    ..
                },
            ) => Some(Val::Scalar(Expr::Const(u64::from(u32::from(*value))))),
            (
                Value::Enum { .. } | Value::Variant { .. },
                LayoutKind::Scalar {
                    domain: ScalarDomain::Enum(_),
                    ..
                },
            ) => Some(Val::Scalar(Expr::Const(
                self.free_fns.constant_source().word(value.clone())? as u64,
            ))),
            (
                Value::Record {
                    owner,
                    fields: values,
                },
                LayoutKind::Struct { fields, .. },
            ) => {
                let declaration = self.free_fns.declarations().structure(*owner)?;
                if declaration.fields.len() != values.len() || fields.len() != values.len() {
                    return None;
                }
                let mut leaves = Vec::new();
                for ((declaration, value), field) in
                    declaration.fields.iter().zip(values.iter()).zip(fields)
                {
                    if declaration.name.text != field.name {
                        return None;
                    }
                    Self::prefix_block_value(
                        &field.name,
                        self.materialize_constant(value, &mut field.layout)?,
                        &mut leaves,
                    );
                }
                Some(Val::Fields(leaves))
            }
            (Value::Tuple(values), LayoutKind::Struct { fields, .. }) => {
                if values.len() != fields.len() {
                    return None;
                }
                let mut leaves = Vec::new();
                for (index, (value, field)) in values.iter().zip(fields).enumerate() {
                    if field.name != index.to_string() {
                        return None;
                    }
                    Self::prefix_block_value(
                        &field.name,
                        self.materialize_constant(value, &mut field.layout)?,
                        &mut leaves,
                    );
                }
                Some(Val::Fields(leaves))
            }
            (
                Value::Array {
                    values,
                    left,
                    descending,
                    ..
                },
                LayoutKind::Array { range, element },
            ) => {
                // An open declaration inherits the interpreter's concrete shape, not new bounds.
                if range.is_none() {
                    let extent = i64::try_from(values.len().checked_sub(1)?).ok()?;
                    *range = Some(LayoutRange {
                        left: *left,
                        right: if *descending {
                            left.checked_sub(extent)?
                        } else {
                            left.checked_add(extent)?
                        },
                    });
                }
                let range = range.as_ref()?;
                if range.left != *left
                    || range.ascending() == *descending
                    || range.len()? != u64::try_from(values.len()).ok()?
                {
                    return None;
                }
                let mut leaves = Vec::new();
                for (offset, value) in values.iter().enumerate() {
                    let offset = i64::try_from(offset).ok()?;
                    let label = if *descending {
                        left.checked_sub(offset)?
                    } else {
                        left.checked_add(offset)?
                    };
                    Self::prefix_block_value(
                        &format!("[{label}]"),
                        self.materialize_constant(value, element)?,
                        &mut leaves,
                    );
                }
                Some(Val::Fields(leaves))
            }
            // Packed/empty-open/generic representations still use their existing
            // consumers until their typed-value migrations are complete.
            _ => None,
        }
    }
}

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
