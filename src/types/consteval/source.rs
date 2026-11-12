//! Borrowed scalar semantics shared by constant-evaluation consumers.

use super::{Binding, Environment, LocalKey, SelectedFunction, Value};
use crate::resolve::declarations::{AppliedArguments, DeclarationIndex};
use crate::resolve::{DefId, DefKind, Resolved};
use crate::syntax::ast::{
    BinOp, Expr, FnDecl, FnParam, Ident, ImplItem, Path, StructDecl, Type, UnOp,
};
use crate::types::enum_values::EnumValues;

#[derive(Clone, Copy)]
pub(crate) struct Source<'i, 'a> {
    declarations: &'i DeclarationIndex<'a>,
    functions: Option<&'i DeclarationIndex<'a>>,
    enums: Option<&'i EnumValues>,
}

impl<'i, 'a> Source<'i, 'a> {
    pub(crate) fn new(declarations: &'i DeclarationIndex<'a>) -> Self {
        Self {
            declarations,
            functions: None,
            enums: None,
        }
    }

    pub(crate) fn with_enums(mut self, enums: &'i EnumValues) -> Self {
        self.enums = Some(enums);
        self
    }

    /// Project a typed scalar only where a consumer requires its stored word.
    pub(crate) fn word(&self, value: Value) -> Option<i64> {
        match value {
            Value::Integer(value) => i64::try_from(value).ok(),
            Value::Natural(_) | Value::Real(_) => None,
            Value::Character(ch) => Some(i64::from(u32::from(ch))),
            Value::Enum { discriminant, .. } => Some(discriminant as i64),
            Value::Variant { owner, variant } => Some(self.enums?.value(owner, variant)? as i64),
            Value::Array { .. } | Value::Record { .. } | Value::Tuple(_) => None,
        }
    }

    pub(super) fn range_value(&self, left: Value, right: Value) -> Option<Value> {
        let owner = self.resolved().lang("range")?;
        let declaration = self.structure(owner)?;
        if declaration.base.is_some()
            || !declaration.params.params.is_empty()
            || declaration.fields.len() != 2
            || declaration
                .fields
                .iter()
                .any(|field| self.type_id(&field.ty, None) != self.resolved().builtin("integer"))
            || left.integer().is_none()
            || right.integer().is_none()
        {
            return None;
        }
        Some(Value::Record {
            owner,
            fields: [left, right].into(),
        })
    }

    /// Range syntax uses the registered nominal declaration, not a lookalike record.
    pub(crate) fn range_bounds(&self, value: &Value) -> Option<(i128, i128)> {
        let Value::Record { owner, fields } = value else {
            return None;
        };
        if Some(*owner) != self.resolved().lang("range") {
            return None;
        }
        let [left, right] = fields.as_ref() else {
            return None;
        };
        Some((left.integer()?, right.integer()?))
    }

    fn enum_number(&self, value: &Value) -> Option<i128> {
        match value {
            Value::Enum { discriminant, .. } => Some(i128::from(*discriminant)),
            Value::Variant { owner, variant } => self.enums?.number(*owner, *variant),
            _ => None,
        }
    }

    pub(super) fn equivalent(&self, left: &Value, right: &Value) -> Option<bool> {
        match (left, right) {
            (Value::Variant { .. }, Value::Variant { .. }) => Some(left == right),
            _ if left.enum_owner().is_some() && left.enum_owner() == right.enum_owner() => {
                Some(self.enum_number(left)? == self.enum_number(right)?)
            }
            _ if matches!(
                left,
                Value::Array { .. } | Value::Record { .. } | Value::Tuple(_)
            ) || matches!(
                right,
                Value::Array { .. } | Value::Record { .. } | Value::Tuple(_)
            ) =>
            {
                None
            }
            _ => Some(left == right),
        }
    }

    pub(crate) fn with_functions(mut self, functions: Option<&'i DeclarationIndex<'a>>) -> Self {
        self.functions = functions;
        self
    }

    fn resolved(&self) -> &'a Resolved {
        self.declarations.resolved()
    }

    fn named(&self, owner: DefId, name: &str) -> Option<Value> {
        Some(Value::Variant {
            owner,
            variant: self.resolved().enum_variant(owner, name)?,
        })
    }

    pub(super) fn boolean(&self, value: bool) -> Option<Value> {
        self.named(
            self.resolved().lang("bool")?,
            if value { "true" } else { "false" },
        )
    }

    pub(super) fn type_id(&self, ty: &Type, receiver: Option<DefId>) -> Option<DefId> {
        let id = self.declarations.scalar_type_id(ty)?;
        if Some(id) == self.resolved().builtin("range") {
            return self.resolved().lang("range");
        }
        if self.resolved().def(id).is_some_and(|definition| {
            definition.kind == DefKind::Local
                && definition.name == "Self"
                && definition.span.is_none()
        }) {
            receiver
        } else {
            Some(id)
        }
    }

    pub(super) fn supported(&self, ty: &Type, receiver: Option<DefId>) -> bool {
        let Some(normalized) = self.declarations.normalize_type(ty) else {
            return false;
        };
        match normalized.as_ref() {
            Type::Indexed { base, .. } => return self.supported(base, receiver),
            Type::Tuple { elems, .. } => {
                return elems.iter().all(|ty| self.supported(ty, receiver))
            }
            _ => {}
        }
        self.type_id(ty, receiver).is_some_and(|id| {
            self.supported_scalar(id)
                || self
                    .structure(id)
                    .is_some_and(|s| s.base.is_none() && s.params.params.is_empty())
        })
    }

    fn supported_scalar(&self, id: DefId) -> bool {
        self.resolved().kind_of(id) == Some(DefKind::Enum)
            || ["integer", "natural", "real", "Char"]
                .iter()
                .any(|name| Some(id) == self.resolved().builtin(name))
    }

    pub(super) fn bind_value(
        &self,
        ty: &Type,
        value: Value,
        receiver: Option<DefId>,
        literal: bool,
    ) -> Option<Value> {
        let id = self.type_id(ty, receiver)?;
        if Some(id) == self.resolved().builtin("integer") {
            let Value::Integer(value) = value else {
                return None;
            };
            return i64::try_from(value)
                .ok()
                .map(|value| Value::Integer(i128::from(value)));
        }
        if self.value_type(&value) == Some(id) {
            return Some(value);
        }
        if literal {
            if let Value::Integer(value) = value {
                if Some(id) == self.resolved().builtin("natural") {
                    return u64::try_from(value).ok().map(Value::Natural);
                }
                if Some(id) == self.resolved().builtin("real") {
                    return Some(Value::Real(value as f64));
                }
            }
        }
        None
    }

    pub(super) fn normalized_type<'t>(&self, ty: &'t Type) -> Option<std::borrow::Cow<'t, Type>>
    where
        'a: 't,
    {
        self.declarations.normalize_type(ty)
    }

    pub(super) fn instantiated_type<'t>(
        &self,
        arguments: &AppliedArguments<'_>,
        ty: &'t Type,
    ) -> Option<std::borrow::Cow<'t, Type>>
    where
        'a: 't,
    {
        self.declarations.instantiate_type(arguments, ty)
    }

    pub(super) fn structure(&self, owner: DefId) -> Option<&'a StructDecl> {
        self.declarations
            .structure(owner)
            .or_else(|| self.functions?.structure(owner))
    }

    pub(super) fn value_type(&self, value: &Value) -> Option<DefId> {
        match value {
            Value::Integer(_) => self.resolved().builtin("integer"),
            Value::Natural(_) => self.resolved().builtin("natural"),
            Value::Real(_) => self.resolved().builtin("real"),
            Value::Character(_) => self.resolved().builtin("Char"),
            Value::Enum { owner, .. }
            | Value::Variant { owner, .. }
            | Value::Record { owner, .. } => Some(*owner),
            Value::Array { .. } | Value::Tuple(_) => None,
        }
    }

    pub(super) fn primitive_id(&self, name: &str) -> Option<DefId> {
        self.resolved().builtin(name)
    }

    pub(crate) fn binding(&self, path: &Path) -> Option<Binding<'a>> {
        if let Some(constant) = self.declarations.constant_path(path).or_else(|| {
            self.functions
                .and_then(|functions| functions.constant_path(path))
        }) {
            if !self.supported(&constant.ty, None) {
                return None;
            }
            return Some(Binding::TypedExpression {
                value: &constant.value,
                ty: &constant.ty,
            });
        }
        let (owner, variant) = self.declarations.enum_variant(path)?;
        Some(Binding::Value(Value::Variant { owner, variant }))
    }

    pub(super) fn local_key<'p>(&self, path: &'p Path) -> Option<LocalKey<'p>> {
        self.declarations.local_id(path).map(LocalKey::Definition)
    }

    pub(super) fn parameter_key<'p>(&self, parameter: &'p FnParam) -> Option<LocalKey<'p>> {
        self.declarations
            .parameter_id(parameter)
            .map(LocalKey::Definition)
    }

    pub(super) fn declaration_key<'p>(&self, name: &'p Ident) -> Option<LocalKey<'p>> {
        self.resolved()
            .declared(name.span)
            .filter(|&id| self.resolved().kind_of(id) == Some(DefKind::Local))
            .map(LocalKey::Definition)
    }

    pub(super) fn enum_type(&self, ty: &Type, receiver: Option<DefId>) -> Option<DefId> {
        let id = self.type_id(ty, receiver)?;
        (self.resolved().kind_of(id) == Some(DefKind::Enum)).then_some(id)
    }

    pub(super) fn enum_literal(&self, ch: char, owner: DefId) -> Option<Value> {
        self.named(owner, &format!("'{ch}'"))
    }

    pub(super) fn function(&self, callee: &Expr) -> Option<&'a FnDecl> {
        let Expr::Path(path) = callee else {
            return None;
        };
        let function = self.declarations.function_path(path).or_else(|| {
            self.functions
                .and_then(|functions| functions.function_path(path))
        })?;
        self.supports_function(function).then_some(function)
    }

    pub(crate) fn supports_function(&self, function: &FnDecl) -> bool {
        function.generics.params.is_empty()
            && function
                .ret
                .as_ref()
                .is_some_and(|ty| self.supported(ty, None))
            && function.params.iter().all(|parameter| {
                !parameter.is_self
                    && parameter
                        .ty
                        .as_ref()
                        .is_some_and(|ty| self.supported(ty, None))
            })
    }

    pub(super) fn method_function(&self, method: &Ident, receiver: &Value) -> Option<&'a FnDecl> {
        let owner = self.value_type(receiver)?;
        let supported = |function: &&FnDecl| {
            function.name.text == method.text
                && function.body.is_some()
                && function.generics.params.is_empty()
                && function
                    .params
                    .first()
                    .is_some_and(|parameter| parameter.is_self)
                && function
                    .ret
                    .as_ref()
                    .is_some_and(|ty| self.supported(ty, Some(owner)))
                && function.params.iter().skip(1).all(|parameter| {
                    !parameter.is_self
                        && parameter
                            .ty
                            .as_ref()
                            .is_some_and(|ty| self.supported(ty, Some(owner)))
                })
        };
        // Inherent methods take priority; distinct matching trait bodies are ambiguous.
        for inherent in [true, false] {
            let mut selected: Option<&'a FnDecl> = None;
            for implementation in self.declarations.implementations_for(owner).chain(
                self.functions
                    .into_iter()
                    .flat_map(|functions| functions.implementations_for(owner)),
            ) {
                if implementation.trait_.is_none() != inherent {
                    continue;
                }
                let explicit = implementation.items.iter().find_map(|item| match item {
                    ImplItem::Fn(function) if function.name.text == method.text => Some(function),
                    _ => None,
                });
                let candidate = explicit
                    .or_else(|| {
                        let trait_id = self
                            .resolved()
                            .resolved(implementation.trait_.as_ref()?.span)?;
                        let declaration = self.declarations.trait_decl(trait_id).or_else(|| {
                            self.functions
                                .and_then(|functions| functions.trait_decl(trait_id))
                        })?;
                        declaration
                            .items
                            .iter()
                            .find(|function| function.name.text == method.text)
                    })
                    .filter(supported);
                if let Some(function) = candidate {
                    if selected.is_some_and(|previous| !std::ptr::eq(previous, function)) {
                        return None;
                    }
                    selected = Some(function);
                }
            }
            if selected.is_some() {
                return selected;
            }
        }
        None
    }

    fn constructor(&self, callee: &Expr) -> Option<&'static str> {
        let Expr::Path(path) = callee else {
            return None;
        };
        let id = self
            .declarations
            .scalar_type_id(&Type::Path(path.clone()))?;
        if Some(id) == self.resolved().builtin("integer") {
            Some("integer")
        } else if Some(id) == self.resolved().builtin("natural") {
            Some("natural")
        } else if Some(id) == self.resolved().builtin("real") {
            Some("real")
        } else if Some(id) == self.resolved().builtin("Char") {
            Some("Char")
        } else {
            None
        }
    }

    pub(super) fn transparent_conversion(&self, callee: &Expr) -> bool {
        self.constructor(callee).is_some()
    }

    pub(super) fn convert_value(&self, callee: &Expr, value: Value) -> Option<Value> {
        match (self.constructor(callee)?, value) {
            ("integer", Value::Integer(value)) => i64::try_from(value)
                .ok()
                .map(|value| Value::Integer(i128::from(value))),
            ("integer", Value::Natural(value)) => i64::try_from(value)
                .ok()
                .map(|value| Value::Integer(i128::from(value))),
            ("integer", Value::Real(value))
                if (i64::MIN as f64..-(i64::MIN as f64)).contains(&value) =>
            {
                Some(Value::Integer(i128::from(value as i64)))
            }
            ("integer", Value::Character(ch)) => Some(Value::Integer(i128::from(ch as u32))),
            ("integer", Value::Enum { discriminant, .. }) => {
                Some(Value::Integer(i128::from(discriminant as i64)))
            }
            ("integer", Value::Variant { owner, variant }) => Some(Value::Integer(i128::from(
                self.enums?.value(owner, variant)? as i64,
            ))),
            ("Char", value @ Value::Character(_)) => Some(value),
            ("Char", Value::Integer(value)) => {
                char::from_u32(u32::try_from(value).ok()?).map(Value::Character)
            }
            ("natural", value @ Value::Natural(_)) | ("real", value @ Value::Real(_)) => {
                Some(value)
            }
            ("natural", Value::Integer(value)) => i64::try_from(value)
                .ok()
                .and_then(|value| u64::try_from(value).ok())
                .map(Value::Natural),
            ("natural", Value::Real(value)) if (0.0..u64::MAX as f64).contains(&value) => {
                Some(Value::Natural(value as u64))
            }
            ("real", Value::Integer(value)) => i64::try_from(value)
                .ok()
                .map(|value| Value::Real(value as f64)),
            ("real", Value::Natural(value)) => Some(Value::Real(value as f64)),
            _ => None,
        }
    }

    fn signature_matches(
        &self,
        function: &FnDecl,
        owner: DefId,
        values: &[&Value],
        arguments: Option<&AppliedArguments<'_>>,
    ) -> bool {
        let type_id = |ty: &Type| {
            let ty = match arguments {
                Some(arguments) => self.declarations.instantiate_type(arguments, ty)?,
                None => std::borrow::Cow::Borrowed(ty),
            };
            self.type_id(&ty, Some(owner))
        };
        function.generics.params.is_empty()
            && function.body.is_some()
            && function.params.len() == values.len()
            && function
                .params
                .first()
                .is_some_and(|parameter| parameter.is_self)
            && function
                .ret
                .as_ref()
                .and_then(type_id)
                .is_some_and(|id| self.supported_scalar(id))
            && function
                .params
                .iter()
                .zip(values)
                .all(|(parameter, value)| {
                    parameter.is_self
                        || parameter.ty.as_ref().and_then(type_id) == self.value_type(value)
                })
    }

    fn hook(
        &self,
        role: &str,
        method: &str,
        values: &[&Value],
        symbol: Option<&str>,
    ) -> Option<SelectedFunction<'a>> {
        let owner = self.value_type(values.first()?)?;
        let trait_id = self.resolved().lang(role)?;
        let mut selected: Option<SelectedFunction<'a>> = None;
        for implementation in self.declarations.implementations_for(owner).chain(
            self.functions
                .into_iter()
                .flat_map(|functions| functions.implementations_for(owner)),
        ) {
            if implementation
                .trait_
                .as_ref()
                .and_then(|path| self.resolved().resolved(path.span))
                .and_then(|id| self.resolved().lang_of(id))
                != Some(role)
            {
                continue;
            }
            if let Some(symbol) = symbol {
                if !matches!(implementation.trait_args.first(),
                    Some(crate::syntax::ast::GenericArg::Positional(Expr::StrLit { text, .. })) if text == symbol)
                {
                    continue;
                }
            }
            let explicit = implementation.items.iter().find_map(|item| match item {
                ImplItem::Fn(function) if function.name.text == method => Some(function),
                _ => None,
            });
            let candidate = if let Some(function) = explicit {
                self.signature_matches(function, owner, values, None)
                    .then_some(SelectedFunction {
                        function,
                        implementation: Some(implementation),
                        applied: None,
                    })
            } else {
                let declaration = self.declarations.trait_decl(trait_id).or_else(|| {
                    self.functions
                        .and_then(|functions| functions.trait_decl(trait_id))
                })?;
                let arguments = self
                    .declarations
                    .arguments(&declaration.params, &implementation.trait_args)
                    .ok()?;
                declaration
                    .items
                    .iter()
                    .find(|function| {
                        function.name.text == method
                            && self.signature_matches(function, owner, values, Some(&arguments))
                    })
                    .map(|function| SelectedFunction {
                        function,
                        implementation: Some(implementation),
                        applied: Some(arguments),
                    })
            };
            if let Some(function) = candidate {
                if selected.as_ref().is_some_and(|previous| {
                    !std::ptr::eq(previous.function, function.function)
                        || previous
                            .implementation
                            .is_none_or(|previous| !std::ptr::eq(previous, implementation))
                }) {
                    return None;
                }
                selected = Some(function);
            }
        }
        selected
    }

    pub(super) fn operator_function(
        &self,
        expression: &Expr,
        left: &Value,
        right: Option<&Value>,
    ) -> Option<SelectedFunction<'a>> {
        let (role, method, symbol) = match expression {
            Expr::Unary { op: UnOp::Not, .. } => ("not", "not", None),
            Expr::Unary { op: UnOp::Neg, .. } => ("neg", "neg", None),
            Expr::Binary { op, .. } => return self.binary_function(op, left, right?),
            _ => return None,
        };
        self.hook(role, method, &[left], symbol)
    }

    pub(super) fn binary_function(
        &self,
        op: &BinOp,
        left: &Value,
        right: &Value,
    ) -> Option<SelectedFunction<'a>> {
        let (role, method, symbol) = match op {
            BinOp::Eq => ("eq", "eq", None),
            BinOp::Ne => ("eq", "ne", None),
            BinOp::Lt => ("ord", "lt", None),
            BinOp::Le => ("ord", "le", None),
            BinOp::Gt => ("ord", "gt", None),
            BinOp::Ge => ("ord", "ge", None),
            BinOp::And => ("and", "and", None),
            BinOp::Or => ("or", "or", None),
            BinOp::Add => ("add", "add", None),
            BinOp::Sub => ("sub", "sub", None),
            BinOp::Mul => ("mul", "mul", None),
            BinOp::Div => ("div", "div", None),
            BinOp::Rem => ("rem", "rem", None),
            BinOp::Shl => ("shl", "shl", None),
            BinOp::Shr => ("shr", "shr", None),
            BinOp::Custom { symbol, .. } => ("custom_operator", "apply", Some(symbol.as_str())),
        };
        self.hook(role, method, &[left, right], symbol)
    }

    pub(super) fn condition_function(&self, value: &Value) -> Option<SelectedFunction<'a>> {
        self.hook("boolean", "as_bool", &[value], None)
    }

    pub(super) fn condition_value(&self, value: &Value) -> Option<bool> {
        let owner = self.resolved().lang("bool")?;
        if Some(value) == self.named(owner, "true").as_ref() {
            Some(true)
        } else if Some(value) == self.named(owner, "false").as_ref() {
            Some(false)
        } else if let Value::Enum {
            owner: actual,
            discriminant,
        } = value
        {
            if *actual != owner {
                return None;
            }
            let encoded = |name| {
                self.enums?
                    .value(owner, self.resolved().enum_variant(owner, name)?)
            };
            if encoded("true") == Some(*discriminant) {
                Some(true)
            } else if encoded("false") == Some(*discriminant) {
                Some(false)
            } else {
                None
            }
        } else {
            None
        }
    }

    pub(super) fn binary_value(
        &self,
        expression: &Expr,
        left: &Value,
        right: &Value,
        intrinsic: bool,
    ) -> Option<Value> {
        let Expr::Binary { op, .. } = expression else {
            return None;
        };
        self.binary_values(op, left, right, intrinsic)
    }

    pub(super) fn binary_values(
        &self,
        op: &BinOp,
        left: &Value,
        right: &Value,
        intrinsic: bool,
    ) -> Option<Value> {
        let order = match (left, right) {
            (Value::Integer(left), Value::Integer(right)) => match op {
                BinOp::And => return Some(Value::Integer(left & right)),
                BinOp::Or => return Some(Value::Integer(left | right)),
                _ => left.cmp(right),
            },
            (Value::Natural(left), Value::Natural(right)) => match op {
                BinOp::And => return Some(Value::Natural(left & right)),
                BinOp::Or => return Some(Value::Natural(left | right)),
                _ => left.cmp(right),
            },
            (Value::Real(left), Value::Real(right)) => {
                return self.boolean(match op {
                    BinOp::Eq => left == right,
                    BinOp::Ne => left != right,
                    BinOp::Lt => left < right,
                    BinOp::Le => left <= right,
                    BinOp::Gt => left > right,
                    BinOp::Ge => left >= right,
                    _ => return None,
                });
            }
            (Value::Character(left), Value::Character(right)) => left.cmp(right),
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
                BinOp::Eq => return self.boolean(left == right),
                BinOp::Ne => return self.boolean(left != right),
                _ if intrinsic => self
                    .enums?
                    .number(*a, *left)?
                    .cmp(&self.enums?.number(*b, *right)?),
                _ => return None,
            },
            (left, right)
                if left.enum_owner().is_some()
                    && left.enum_owner() == right.enum_owner()
                    && (intrinsic || matches!(op, BinOp::Eq | BinOp::Ne)) =>
            {
                if matches!(op, BinOp::Eq | BinOp::Ne) {
                    let same = self.equivalent(left, right)?;
                    return self.boolean(if matches!(op, BinOp::Eq) { same } else { !same });
                }
                self.enum_number(left)?.cmp(&self.enum_number(right)?)
            }
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
}

impl Environment for Source<'_, '_> {
    fn source(&self) -> Option<Source<'_, '_>> {
        Some(*self)
    }

    fn local_name<'p>(&self, path: &'p Path) -> Option<&'p str> {
        self.declarations.local_name(path)
    }
}
