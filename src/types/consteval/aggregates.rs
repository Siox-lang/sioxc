//! Owned constant data; source contracts remain borrowed from the shared index.

use super::*;
use crate::syntax::ast::type_span;

impl<'a> Interpreter<'a> {
    fn instantiated_type<'t>(&self, ty: &'t Type) -> Option<std::borrow::Cow<'t, Type>>
    where
        'a: 't,
    {
        match self.locals.last().and_then(|frame| frame.applied.as_ref()) {
            Some(applied) => self.environment.source()?.instantiated_type(applied, ty),
            None => Some(std::borrow::Cow::Borrowed(ty)),
        }
    }

    pub(super) fn aggregate_attribute(
        &mut self,
        base: &Expr,
        attr: &Ident,
        span: Span,
        depth: u32,
    ) -> Result<Value, NotConstant> {
        if let Some(value) = self.declared_attribute(base, attr, span, depth) {
            return value;
        }
        let fail = || NotConstant {
            span,
            reason: Reason::Unsupported,
        };
        let value = self.expression(base, None, depth).map_err(|error| {
            if error.reason == Reason::UnknownConstant {
                fail()
            } else {
                error
            }
        })?;
        let Value::Array {
            values,
            left,
            descending,
            ..
        } = value
        else {
            return Err(fail());
        };
        let len = u64::try_from(values.len()).map_err(|_| fail())?;
        self.array_attribute(len, left, descending, attr, span)
    }

    fn declared_attribute(
        &mut self,
        base: &Expr,
        attr: &Ident,
        span: Span,
        depth: u32,
    ) -> Option<Result<Value, NotConstant>> {
        let source = self.environment.source()?;
        let mut fields = Vec::new();
        let mut base = base;
        while let Expr::Field {
            base: inner, field, ..
        } = base
        {
            fields.push(field);
            base = inner;
        }
        // Only places with no calls or index expressions can ignore runtime contents.
        let local = match base {
            Expr::Path(path) => self.environment.local_key(path).and_then(|key| {
                self.locals
                    .last()?
                    .bindings
                    .iter()
                    .rev()
                    .find(|binding| binding.key == key)?
                    .ty
            }),
            _ => None,
        };
        let ty = local.or_else(|| source.declared_type(base))?;
        let mut ty = self.instantiated_type(ty)?;
        for field in fields.into_iter().rev() {
            ty = source.field_type(&ty, &field.text)?;
        }
        let representation = source.array_representation(&ty)?;
        let Type::Indexed {
            index: Some(index), ..
        } = representation.as_ref()
        else {
            return None;
        };
        Some(
            self.array_bounds(Some(index), None, depth + 1, span)
                .and_then(|(len, left, descending)| {
                    self.array_attribute(u64::from(len), left, descending, attr, span)
                }),
        )
    }

    fn array_attribute(
        &self,
        len: u64,
        left: i64,
        descending: bool,
        attr: &Ident,
        span: Span,
    ) -> Result<Value, NotConstant> {
        let fail = || NotConstant {
            span,
            reason: Reason::Unsupported,
        };
        if attr.text == "length" {
            return Ok(Value::Integer(i128::from(len)));
        }
        if attr.text == "ascending" {
            return self
                .environment
                .source()
                .and_then(|source| source.boolean(!descending))
                .ok_or_else(fail);
        }
        let distance = i64::try_from(len.checked_sub(1).ok_or_else(fail)?).map_err(|_| fail())?;
        let right = if descending {
            left.checked_sub(distance)
        } else {
            left.checked_add(distance)
        }
        .ok_or_else(fail)?;
        Ok(Value::Integer(i128::from(match attr.text.as_str() {
            "left" => left,
            "right" => right,
            "low" => left.min(right),
            "high" => left.max(right),
            _ => return Err(fail()),
        })))
    }

    pub(super) fn declared_expression(
        &mut self,
        expression: &Expr,
        ty: &Type,
        receiver: Option<DefId>,
        depth: u32,
    ) -> Result<Value, NotConstant> {
        let fail = || NotConstant {
            span: expr_span(expression),
            reason: Reason::Unsupported,
        };
        let ty = self.instantiated_type(ty).ok_or_else(fail)?;
        let ty = ty.as_ref();
        let expected = self.environment.source().and_then(|source| {
            let id = source.type_id(ty, receiver)?;
            if Some(id) == source.primitive_id("natural") {
                Some(Ty::Natural)
            } else if Some(id) == source.primitive_id("real") {
                Some(Ty::Real)
            } else {
                None
            }
        });
        let value =
            if let Some(value) = expected.and_then(|ty| self.numeric_literal(expression, &ty)) {
                value?
            } else {
                match expression {
                    Expr::Int { text, .. } => {
                        let value = self.environment.literal_value(text).ok_or_else(fail)?;
                        match self.environment.source() {
                            Some(source) => source
                                .bind_value(ty, value, receiver, true)
                                .ok_or_else(fail)?,
                            None => value,
                        }
                    }
                    Expr::StrLit { .. }
                    | Expr::Array { .. }
                    | Expr::Tuple { .. }
                    | Expr::Construct { .. } => {
                        self.aggregate_expression(expression, Some(ty), receiver, depth)?
                    }
                    Expr::IfExpr {
                        cond, then, els, ..
                    } => {
                        let value = self.expression(cond, None, depth + 1)?;
                        let selected = self.condition(value, depth + 1, expr_span(cond))?;
                        return self.declared_expression(
                            if selected { then } else { els },
                            ty,
                            receiver,
                            depth + 1,
                        );
                    }
                    Expr::Match {
                        scrutinee, arms, ..
                    } => {
                        let value = self.expression(scrutinee, None, depth + 1)?;
                        let arm = self
                            .select_arm(arms, &value, expr_span(expression), depth + 1)?
                            .and_then(|arm| arm.value_expr())
                            .ok_or_else(fail)?;
                        return self.declared_expression(arm, ty, receiver, depth + 1);
                    }
                    _ => {
                        let owner = self.environment.enum_type(ty, receiver);
                        self.expression(expression, owner, depth)?
                    }
                }
            };
        let value = self.bind_concrete(ty, value, receiver, depth, expr_span(expression))?;
        self.checked_value(value, expr_span(expression))
    }

    pub(super) fn value_ty(value: &Value) -> Option<Ty> {
        Some(match value {
            Value::Integer(_) => Ty::Integer,
            Value::Natural(_) => Ty::Natural,
            Value::Real(_) => Ty::Real,
            Value::Character(_) => Ty::Char,
            Value::Enum { owner, .. }
            | Value::Variant { owner, .. }
            | Value::Record { owner, .. } => Ty::Named(*owner),
            Value::Tuple(values) => {
                Ty::Tuple(values.iter().map(Self::value_ty).collect::<Option<_>>()?)
            }
            Value::Array {
                element,
                values,
                left,
                descending,
            } => Ty::Array {
                elem: Box::new(element.as_ref().clone()),
                len: u32::try_from(values.len()).ok()?,
                family: None,
                low: if *descending {
                    left.checked_sub(i64::try_from(values.len().checked_sub(1)?).ok()?)?
                } else {
                    *left
                },
            },
        })
    }

    fn contract(
        &mut self,
        ty: &Type,
        receiver: Option<DefId>,
        depth: u32,
    ) -> Result<Ty, NotConstant> {
        let fail = || NotConstant {
            span: type_span(ty),
            reason: Reason::Unsupported,
        };
        if depth > 64 {
            return Err(NotConstant {
                reason: Reason::RecursionLimit,
                ..fail()
            });
        }
        let source = self.environment.source().ok_or_else(fail)?;
        let normalized = source.normalized_type(ty).ok_or_else(fail)?;
        match normalized.as_ref() {
            Type::Indexed { base, index, .. } => {
                let (len, left, descending) =
                    self.array_bounds(index.as_deref(), None, depth + 1, type_span(ty))?;
                Ok(Ty::Array {
                    elem: Box::new(self.contract(base, receiver, depth + 1)?),
                    len,
                    family: None,
                    low: if descending {
                        left.checked_sub(i64::from(len) - 1).ok_or_else(fail)?
                    } else {
                        left
                    },
                })
            }
            Type::Tuple { elems, .. } => Ok(Ty::Tuple(
                elems
                    .iter()
                    .map(|ty| self.contract(ty, receiver, depth + 1))
                    .collect::<Result<_, _>>()?,
            )),
            _ => {
                let id = source.type_id(&normalized, receiver).ok_or_else(fail)?;
                if Some(id) == source.primitive_id("integer") {
                    Ok(Ty::Integer)
                } else if Some(id) == source.primitive_id("natural") {
                    Ok(Ty::Natural)
                } else if Some(id) == source.primitive_id("real") {
                    Ok(Ty::Real)
                } else if Some(id) == source.primitive_id("Char") {
                    Ok(Ty::Char)
                } else if source.supported(&normalized, receiver) {
                    Ok(Ty::Named(id))
                } else {
                    Err(fail())
                }
            }
        }
    }

    fn array_bounds(
        &mut self,
        index: Option<&Expr>,
        inferred: Option<usize>,
        depth: u32,
        span: Span,
    ) -> Result<(u32, i64, bool), NotConstant> {
        let fail = || NotConstant {
            span: index.map(expr_span).unwrap_or(span),
            reason: Reason::Unsupported,
        };
        let integer = |this: &mut Self, value: &Expr| {
            this.expression(value, None, depth)?
                .integer()
                .ok_or_else(fail)
        };
        match index {
            Some(Expr::Range { lo, hi, .. }) => {
                let left = i64::try_from(integer(self, lo)?).map_err(|_| fail())?;
                let right = i64::try_from(integer(self, hi)?).map_err(|_| fail())?;
                let len = left
                    .abs_diff(right)
                    .checked_add(1)
                    .and_then(|len| u32::try_from(len).ok())
                    .ok_or_else(fail)?;
                Ok((len, left, left > right))
            }
            Some(expression) => {
                let value = self.expression(expression, None, depth)?;
                if let Some((left, right)) = self
                    .environment
                    .source()
                    .and_then(|source| source.range_bounds(&value))
                {
                    let left = i64::try_from(left).map_err(|_| fail())?;
                    let right = i64::try_from(right).map_err(|_| fail())?;
                    let len = left
                        .abs_diff(right)
                        .checked_add(1)
                        .and_then(|len| u32::try_from(len).ok())
                        .ok_or_else(fail)?;
                    Ok((len, left, left > right))
                } else {
                    Ok((
                        u32::try_from(value.integer().ok_or_else(fail)?).map_err(|_| fail())?,
                        0,
                        false,
                    ))
                }
            }
            None => Ok((
                u32::try_from(inferred.ok_or_else(fail)?).map_err(|_| fail())?,
                0,
                false,
            )),
        }
    }

    pub(super) fn bind_declared(
        &mut self,
        ty: &Type,
        value: Value,
        receiver: Option<DefId>,
        depth: u32,
        span: Span,
    ) -> Result<Value, NotConstant> {
        let ty = self.instantiated_type(ty).ok_or(NotConstant {
            span,
            reason: Reason::Unsupported,
        })?;
        self.bind_concrete(&ty, value, receiver, depth, span)
    }

    pub(super) fn bind_concrete(
        &mut self,
        ty: &Type,
        value: Value,
        receiver: Option<DefId>,
        depth: u32,
        span: Span,
    ) -> Result<Value, NotConstant> {
        if depth > 64 {
            return Err(NotConstant {
                span,
                reason: Reason::RecursionLimit,
            });
        }
        if self.environment.source().is_none() {
            return Ok(value);
        }
        let fail = || NotConstant {
            span,
            reason: Reason::Unsupported,
        };
        let source = self.environment.source().unwrap();
        let normalized = source.normalized_type(ty).ok_or_else(fail)?;
        if let (
            Type::Indexed { base, index, .. },
            Value::Array {
                element, values, ..
            },
        ) = (normalized.as_ref(), &value)
        {
            let (len, left, descending) =
                self.array_bounds(index.as_deref(), Some(values.len()), depth + 1, span)?;
            let target = self.contract(base, receiver, depth + 1)?;
            let nested = matches!(target, Ty::Array { .. } | Ty::Tuple(_));
            if usize::try_from(len).ok() != Some(values.len())
                || ((!nested || values.is_empty()) && target != **element)
            {
                return Err(fail());
            }
            // Array assignment conforms positions to the target's declared labels.
            let Value::Array { values, .. } = value else {
                unreachable!()
            };
            let values = if nested {
                let count = values.len();
                self.conform_values(
                    values,
                    std::iter::repeat_n(base.as_ref(), count),
                    receiver,
                    depth + 1,
                    span,
                )?
            } else {
                values
            };
            return Ok(Value::Array {
                element: Arc::new(target),
                values,
                left,
                descending,
            });
        }
        if let (Type::Tuple { elems, .. }, Value::Tuple(values)) = (normalized.as_ref(), &value) {
            if elems.len() != values.len() {
                return Err(fail());
            }
            let Value::Tuple(values) = value else {
                unreachable!()
            };
            return self
                .conform_values(values, elems.iter(), receiver, depth + 1, span)
                .map(Value::Tuple);
        }
        if let Value::Record { owner, fields } = &value {
            if source.type_id(ty, receiver) == Some(*owner) {
                if source.derived_base(*owner).is_some() {
                    return self.derived_value(*owner, fields.clone(), depth + 1, span);
                }
                let declaration = source
                    .structure(*owner)
                    .filter(|declaration| {
                        declaration.base.is_none() && declaration.params.params.is_empty()
                    })
                    .ok_or_else(fail)?;
                if fields.len() != declaration.fields.len() {
                    return Err(fail());
                }
                return Ok(Value::Record {
                    owner: *owner,
                    fields: self.conform_values(
                        fields.clone(),
                        declaration.fields.iter().map(|field| &field.ty),
                        Some(*owner),
                        depth + 1,
                        span,
                    )?,
                });
            }
        }
        source
            .bind_value(ty, value, receiver, false)
            .ok_or_else(fail)
    }

    pub(super) fn derived_conversion(
        &mut self,
        target: DefId,
        value: Value,
        depth: u32,
        span: Span,
    ) -> Result<Value, NotConstant> {
        let fail = || NotConstant {
            span,
            reason: Reason::Unsupported,
        };
        if depth > 64 {
            return Err(NotConstant {
                reason: Reason::RecursionLimit,
                ..fail()
            });
        }
        let source = self.environment.source().ok_or_else(fail)?;
        if let Some(projected) = source.project_derived(&value, target) {
            if let Value::Record { owner, fields } = projected.as_ref() {
                if source.derived_base(*owner).is_some() {
                    return self.derived_value(*owner, fields.clone(), depth + 1, span);
                }
            }
            return Ok(projected.into_owned());
        }
        // A common representation does not make two sibling newtypes convertible.
        let owner = source.value_type(&value).ok_or_else(fail)?;
        if !source.derives_from(target, owner) {
            return Err(fail());
        }
        let base = source.derived_base(target).ok_or_else(fail)?;
        let parent = source.type_id(base, None).ok_or_else(fail)?;
        let value = self.derived_conversion(parent, value, depth + 1, span)?;
        self.derived_value(target, Arc::from([value]), depth + 1, span)
    }

    fn derived_value(
        &mut self,
        owner: DefId,
        fields: Arc<[Value]>,
        depth: u32,
        span: Span,
    ) -> Result<Value, NotConstant> {
        let fail = || NotConstant {
            span,
            reason: Reason::Unsupported,
        };
        if fields.len() != 1 {
            return Err(fail());
        }
        let source = self.environment.source().ok_or_else(fail)?;
        let base = source.derived_base(owner).ok_or_else(fail)?;
        Ok(Value::Record {
            owner,
            fields: self.conform_values(fields, std::iter::once(base), Some(owner), depth, span)?,
        })
    }

    /// Keep the original allocation unless a nested target changes its labels.
    fn conform_values<'t>(
        &mut self,
        values: Arc<[Value]>,
        types: impl Iterator<Item = &'t Type>,
        receiver: Option<DefId>,
        depth: u32,
        span: Span,
    ) -> Result<Arc<[Value]>, NotConstant> {
        let mut changed: Option<Vec<Value>> = None;
        for (index, (value, ty)) in values.iter().zip(types).enumerate() {
            let bound = self.bind_declared(ty, value.clone(), receiver, depth, span)?;
            let same = match (value, &bound) {
                (
                    Value::Array {
                        values: a,
                        left: al,
                        descending: ad,
                        ..
                    },
                    Value::Array {
                        values: b,
                        left: bl,
                        descending: bd,
                        ..
                    },
                ) => al == bl && ad == bd && Arc::ptr_eq(a, b),
                (Value::Tuple(a), Value::Tuple(b)) => Arc::ptr_eq(a, b),
                (
                    Value::Record {
                        owner: a,
                        fields: af,
                    },
                    Value::Record {
                        owner: b,
                        fields: bf,
                    },
                ) => a == b && Arc::ptr_eq(af, bf),
                (Value::Real(a), Value::Real(b)) => a.to_bits() == b.to_bits(),
                _ => value == &bound,
            };
            if !same && changed.is_none() {
                changed = Some(values[..index].to_vec());
            }
            if let Some(values) = &mut changed {
                values.push(bound);
            }
        }
        Ok(changed.map(Arc::from).unwrap_or(values))
    }

    pub(super) fn aggregate_expression(
        &mut self,
        expression: &Expr,
        declared: Option<&Type>,
        receiver: Option<DefId>,
        depth: u32,
    ) -> Result<Value, NotConstant> {
        let fail = || NotConstant {
            span: expr_span(expression),
            reason: Reason::Unsupported,
        };
        if depth > 64 {
            return Err(NotConstant {
                reason: Reason::RecursionLimit,
                ..fail()
            });
        }
        let source = self.environment.source();
        let normalized = declared.and_then(|ty| source?.normalized_type(ty));
        match expression {
            Expr::StrLit { text, .. } => {
                let (element, index) = match normalized.as_deref() {
                    Some(Type::Indexed { base, index, .. }) => {
                        let element = self.contract(base, receiver, depth + 1)?;
                        if !matches!(element, Ty::Char)
                            && !matches!(&element, Ty::Named(owner)
                                if self.environment.enum_type(base, receiver) == Some(*owner))
                        {
                            return Err(fail());
                        }
                        (element, index.as_deref())
                    }
                    Some(_) => return Err(fail()),
                    None => (Ty::Char, None),
                };
                let count = text.chars().count();
                let (len, left, descending) =
                    self.array_bounds(index, Some(count), depth + 1, expr_span(expression))?;
                if usize::try_from(len).ok() != Some(count) {
                    return Err(fail());
                }
                let mut values = Vec::with_capacity(count);
                for character in text.chars() {
                    values.push(match &element {
                        Ty::Char => Value::Character(character),
                        Ty::Named(owner) => self
                            .environment
                            .enum_literal(character, *owner)
                            .ok_or_else(fail)?,
                        _ => return Err(fail()),
                    });
                }
                Ok(Value::Array {
                    element: Arc::new(element),
                    values: values.into(),
                    left,
                    descending,
                })
            }
            Expr::Array { elems, .. } => {
                let (base, index) = match normalized.as_deref() {
                    Some(Type::Indexed { base, index, .. }) => {
                        (Some(base.as_ref()), index.as_deref())
                    }
                    Some(_) => return Err(fail()),
                    None => (None, None),
                };
                let (len, left, descending) =
                    self.array_bounds(index, Some(elems.len()), depth + 1, expr_span(expression))?;
                if usize::try_from(len).ok() != Some(elems.len()) {
                    return Err(fail());
                }
                let values = elems
                    .iter()
                    .map(|value| match base {
                        Some(ty) => self.declared_expression(value, ty, receiver, depth + 1),
                        None => self.expression(value, None, depth + 1),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let element = match base {
                    Some(ty) => self.contract(ty, receiver, depth + 1)?,
                    None => Self::value_ty(values.first().ok_or_else(fail)?).ok_or_else(fail)?,
                };
                if values
                    .iter()
                    .any(|value| Self::value_ty(value).as_ref() != Some(&element))
                {
                    return Err(fail());
                }
                Ok(Value::Array {
                    element: Arc::new(element),
                    values: values.into(),
                    left,
                    descending,
                })
            }
            Expr::Tuple { elems, .. } => {
                let types = match normalized.as_deref() {
                    Some(Type::Tuple { elems: types, .. }) if types.len() == elems.len() => {
                        Some(types)
                    }
                    Some(_) => return Err(fail()),
                    None => None,
                };
                Ok(Value::Tuple(
                    elems
                        .iter()
                        .enumerate()
                        .map(|(index, value)| match types {
                            Some(types) => {
                                self.declared_expression(value, &types[index], receiver, depth + 1)
                            }
                            None => self.expression(value, None, depth + 1),
                        })
                        .collect::<Result<Vec<_>, _>>()?
                        .into(),
                ))
            }
            Expr::Construct {
                ty, args, spread, ..
            } => {
                let source = source.ok_or_else(fail)?;
                let owner = source
                    .type_id(ty.as_ref().or(declared).ok_or_else(fail)?, receiver)
                    .ok_or_else(fail)?;
                let structure = source
                    .structure(owner)
                    .filter(|s| s.base.is_none() && s.params.params.is_empty())
                    .ok_or_else(fail)?;
                let previous = spread
                    .as_deref()
                    .map(|value| self.expression(value, None, depth + 1))
                    .transpose()?;
                if previous.as_ref().is_some_and(|value| !matches!(value, Value::Record { owner: actual, .. } if *actual == owner)) { return Err(fail()); }
                let mut fields = Vec::with_capacity(structure.fields.len());
                let mut used = vec![false; args.len()];
                for (position, field) in structure.fields.iter().enumerate() {
                    let mut selected =
                        args.iter()
                            .enumerate()
                            .filter(|(index, arg)| match &arg.field {
                                Some(name) => name.text == field.name.text,
                                None => *index == position,
                            });
                    let value = if let Some((index, arg)) = selected.next() {
                        if selected.next().is_some() || used[index] {
                            return Err(fail());
                        }
                        used[index] = true;
                        self.declared_expression(
                            arg.value.as_ref().ok_or_else(fail)?,
                            &field.ty,
                            Some(owner),
                            depth + 1,
                        )?
                    } else if let Some(Value::Record { fields, .. }) = &previous {
                        fields.get(position).cloned().ok_or_else(fail)?
                    } else {
                        return Err(fail());
                    };
                    fields.push(value);
                }
                if used.iter().any(|used| !used) {
                    return Err(fail());
                }
                Ok(Value::Record {
                    owner,
                    fields: fields.into(),
                })
            }
            _ => Err(fail()),
        }
    }

    pub(super) fn project_field(&self, value: Value, field: &Ident) -> Option<Value> {
        let mut value = &value;
        loop {
            match value {
                Value::Tuple(values) => {
                    return values.get(field.text.parse::<usize>().ok()?).cloned()
                }
                Value::Record { owner, fields } => {
                    let source = self.environment.source()?;
                    if let Some(base) = source.derived_base(*owner) {
                        let parent = source.type_id(base, None)?;
                        let [base] = fields.as_ref() else {
                            return None;
                        };
                        if source.value_type(base) != Some(parent) {
                            return None;
                        }
                        value = base;
                        continue;
                    }
                    let position = source
                        .structure(*owner)?
                        .fields
                        .iter()
                        .position(|f| f.name.text == field.text)?;
                    return fields.get(position).cloned();
                }
                _ => return None,
            }
        }
    }
}
