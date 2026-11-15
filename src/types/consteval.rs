//! Shared constant expression and pure-function interpreter.
//!
//! Bindings and non-integer operators belong to the caller's semantic context;
//! traversal, checked arithmetic and control flow have one implementation.

use crate::diag::Span;
use crate::syntax::ast::{
    expr_span, pattern_span, stmt_span, BinOp, ConstDecl, ElseBranch, Expr, FnDecl, FnParam, Ident,
    IfStmt, ImplDecl, MatchArm, Path, Pattern, Stmt, Type, UnOp,
};

use super::helpers::{unsigned_lit_magnitude, unsigned_lit_text};
use super::Ty;
use crate::resolve::declarations::{AppliedArguments, DeclarationIndex};
use crate::resolve::DefId;
use std::sync::Arc;

mod aggregates;
mod source;
pub(crate) use source::Source;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    Unsupported,
    UnknownConstant,
    Overflow,
    DivisionByZero,
    InvalidShift,
    RecursionLimit,
    ArityMismatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NotConstant {
    pub span: Span,
    pub reason: Reason,
}

/// The checker's wider analysis domain is not a packed-value width limit.
/// NativeWord retains the existing kernel integer ABI's checked intermediates.
#[derive(Clone, Copy)]
pub(crate) enum IntegerDomain {
    Analysis,
    NativeWord,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RangeOrder {
    Forward,
    Reversed,
    Unordered,
}

impl IntegerDomain {
    fn check(self, value: i128) -> Option<i128> {
        match self {
            Self::Analysis => Some(value),
            Self::NativeWord => i64::try_from(value).ok().map(i128::from),
        }
    }

    fn bits(self) -> u32 {
        match self {
            Self::Analysis => i128::BITS,
            Self::NativeWord => i64::BITS,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Integer(i128),
    Natural(u64),
    Real(f64),
    Character(char),
    Enum {
        owner: DefId,
        discriminant: u64,
    },
    /// Source member identity is usable before its physical encoding is known.
    Variant {
        owner: DefId,
        variant: DefId,
    },
    Array {
        element: Arc<Ty>,
        values: Arc<[Value]>,
        left: i64,
        descending: bool,
    },
    Record {
        owner: DefId,
        /// Declaration order plus the nominal owner identifies a field.
        fields: Arc<[Value]>,
    },
    Tuple(Arc<[Value]>),
}

impl Value {
    pub(crate) fn integer(&self) -> Option<i128> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }

    pub(crate) fn enum_owner(&self) -> Option<DefId> {
        match self {
            Self::Enum { owner, .. } | Self::Variant { owner, .. } => Some(*owner),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalKey<'a> {
    Definition(DefId),
    Name(&'a str),
}

pub(crate) enum Binding<'a> {
    Value(Value),
    Expression(&'a Expr),
    TypedExpression { value: &'a Expr, ty: &'a Type },
}

pub(crate) struct SelectedFunction<'a> {
    function: &'a FnDecl,
    implementation: Option<&'a ImplDecl>,
    applied: Option<AppliedArguments<'a>>,
}

/// A stage supplies declaration identities and bound values without upward
/// dependencies. Unsupported operations remain unknown, never fabricated.
pub(crate) trait Environment {
    fn source(&self) -> Option<Source<'_, '_>> {
        None
    }

    fn binding(&self, path: &Path) -> Option<Binding<'_>> {
        self.source()?.binding(path)
    }

    fn domain(&self) -> IntegerDomain {
        IntegerDomain::Analysis
    }

    fn literal(&self, text: &str) -> Option<i128> {
        i128::try_from(unsigned_lit_magnitude(text)?).ok()
    }

    fn literal_value(&self, text: &str) -> Option<Value> {
        if text.contains('.') {
            text.replace('_', "").parse().ok().map(Value::Real)
        } else {
            self.literal(text).map(Value::Integer)
        }
    }

    /// A module constant must not be captured by a caller's same-named local.
    fn local_name<'p>(&self, path: &'p Path) -> Option<&'p str> {
        match path.segments.as_slice() {
            [name] => Some(&name.text),
            _ => None,
        }
    }

    fn local_key<'p>(&self, path: &'p Path) -> Option<LocalKey<'p>> {
        if let Some(source) = self.source() {
            return source.local_key(path);
        }
        self.local_name(path).map(LocalKey::Name)
    }

    fn parameter_key<'p>(&self, parameter: &'p FnParam) -> Option<LocalKey<'p>> {
        if let Some(source) = self.source() {
            return source.parameter_key(parameter);
        }
        if parameter.is_self {
            Some(LocalKey::Name("self"))
        } else {
            parameter
                .name
                .as_ref()
                .map(|name| LocalKey::Name(&name.text))
        }
    }

    fn enum_type(&self, ty: &Type, receiver: Option<DefId>) -> Option<DefId> {
        self.source()?.enum_type(ty, receiver)
    }

    fn declaration_key<'d>(&self, name: &'d Ident) -> Option<LocalKey<'d>> {
        match self.source() {
            Some(source) => source.declaration_key(name),
            None => Some(LocalKey::Name(&name.text)),
        }
    }

    fn enum_literal(&self, ch: char, owner: DefId) -> Option<Value> {
        self.source()?.enum_literal(ch, owner)
    }

    fn unary_value(&self, expression: &Expr, value: &Value) -> Option<Value> {
        if self.source().is_some() {
            return None;
        }
        self.unary(expression, value.integer()?).map(Value::Integer)
    }

    fn binary_value(&self, expression: &Expr, left: &Value, right: &Value) -> Option<Value> {
        if let Some(source) = self.source() {
            return source.binary_value(expression, left, right, false);
        }
        self.binary(expression, left.integer()?, right.integer()?)
            .map(Value::Integer)
    }

    fn condition_value(&self, value: &Value) -> Option<bool> {
        if let Some(source) = self.source() {
            return source.condition_value(value);
        }
        self.condition(value.integer()?)
    }

    fn operator_function(
        &self,
        expression: &Expr,
        left: &Value,
        right: Option<&Value>,
    ) -> Option<SelectedFunction<'_>> {
        self.source()?.operator_function(expression, left, right)
    }

    fn intrinsic_binary_value(
        &self,
        expression: &Expr,
        left: &Value,
        right: &Value,
    ) -> Option<Value> {
        if let Some(source) = self.source() {
            source.binary_value(expression, left, right, true)
        } else {
            self.binary_value(expression, left, right)
        }
    }

    fn condition_function(&self, value: &Value) -> Option<SelectedFunction<'_>> {
        self.source()?.condition_function(value)
    }

    fn method_function(&self, method: &Ident, receiver: &Value) -> Option<&FnDecl> {
        self.source()?.method_function(method, receiver)
    }

    fn attribute(&self, _base: &Expr, _attr: &Ident) -> Option<i128> {
        None
    }

    fn function(&self, callee: &Expr) -> Option<&FnDecl> {
        self.source()?.function(callee)
    }

    fn transparent_conversion(&self, callee: &Expr) -> bool {
        self.source()
            .is_some_and(|source| source.transparent_conversion(callee))
    }

    fn convert_value(&self, callee: &Expr, value: Value) -> Option<Value> {
        match self.source() {
            Some(source) => source.convert_value(callee, value),
            None => Some(value),
        }
    }

    fn unary(&self, _expression: &Expr, _value: i128) -> Option<i128> {
        None
    }

    fn binary(&self, _expression: &Expr, _left: i128, _right: i128) -> Option<i128> {
        None
    }

    fn condition(&self, _value: i128) -> Option<bool> {
        None
    }
}

impl Environment for DeclarationIndex<'_> {
    fn source(&self) -> Option<Source<'_, '_>> {
        Some(Source::new(self))
    }

    fn local_name<'p>(&self, path: &'p Path) -> Option<&'p str> {
        DeclarationIndex::local_name(self, path)
    }

    fn local_key<'p>(&self, path: &'p Path) -> Option<LocalKey<'p>> {
        self.local_id(path).map(LocalKey::Definition)
    }

    fn parameter_key<'p>(&self, parameter: &'p FnParam) -> Option<LocalKey<'p>> {
        self.parameter_id(parameter).map(LocalKey::Definition)
    }
}

struct ConstantExpressions<'a, F>(F, std::marker::PhantomData<&'a Expr>);

impl<'a, F: Fn(&Path) -> Option<&'a Expr>> Environment for ConstantExpressions<'a, F> {
    fn binding(&self, path: &Path) -> Option<Binding<'_>> {
        (self.0)(path).map(Binding::Expression)
    }
}

/// Integer analysis with resolver-selected constant declarations. Narrow only
/// at the consumer boundary, retaining the pattern checker's wider domain.
pub(crate) fn integer<'a>(
    expression: &Expr,
    constant: impl Fn(&Path) -> Option<&'a Expr>,
) -> Result<i128, NotConstant> {
    evaluate(
        expression,
        &ConstantExpressions(constant, std::marker::PhantomData),
        0,
    )
}

pub(crate) fn evaluate(
    expression: &Expr,
    environment: &dyn Environment,
    depth: u32,
) -> Result<i128, NotConstant> {
    evaluate_value(expression, environment, None, depth)?
        .integer()
        .ok_or(NotConstant {
            span: expr_span(expression),
            reason: Reason::Unsupported,
        })
}

pub(crate) fn evaluate_value(
    expression: &Expr,
    environment: &dyn Environment,
    expected_enum: Option<DefId>,
    depth: u32,
) -> Result<Value, NotConstant> {
    Interpreter {
        environment,
        locals: Vec::new(),
        operators: Vec::new(),
    }
    .expression(expression, expected_enum, depth)
}

/// Evaluate a constant at its declared type boundary, before physical projection.
pub(crate) fn evaluate_constant(
    declaration: &ConstDecl,
    environment: &dyn Environment,
    depth: u32,
) -> Result<Value, NotConstant> {
    Interpreter {
        environment,
        locals: Vec::new(),
        operators: Vec::new(),
    }
    .declared_expression(&declaration.value, &declaration.ty, None, depth)
}

/// Evaluate control flow without projecting a source Boolean into an integer.
pub(crate) fn evaluate_condition(
    expression: &Expr,
    environment: &dyn Environment,
    depth: u32,
) -> Result<bool, NotConstant> {
    let mut interpreter = Interpreter {
        environment,
        locals: Vec::new(),
        operators: Vec::new(),
    };
    let value = interpreter.expression(expression, None, depth)?;
    interpreter.condition(value, depth, expr_span(expression))
}

/// Compare borrowed bounds through the same source order as a match pattern.
pub(crate) fn evaluate_range_order(
    left: &Expr,
    right: &Expr,
    environment: &dyn Environment,
    expected_enum: Option<DefId>,
    depth: u32,
) -> Result<RangeOrder, NotConstant> {
    let mut interpreter = Interpreter {
        environment,
        locals: Vec::new(),
        operators: Vec::new(),
    };
    let a = interpreter.expression(left, expected_enum, depth)?;
    let b = interpreter.expression(right, expected_enum.or(a.enum_owner()), depth)?;
    interpreter.range_order(&a, &b, expr_span(left).to(expr_span(right)), depth)
}

pub(crate) fn evaluate_value_statements(
    statements: &[Stmt],
    environment: &dyn Environment,
    expected_enum: Option<DefId>,
    depth: u32,
) -> Result<Option<Value>, NotConstant> {
    Interpreter {
        environment,
        locals: Vec::new(),
        operators: Vec::new(),
    }
    .statements(statements, expected_enum, depth)
}

pub(crate) fn evaluate_function(
    function: &FnDecl,
    arguments: Vec<Value>,
    environment: &dyn Environment,
    depth: u32,
) -> Result<Value, NotConstant> {
    Interpreter {
        environment,
        locals: Vec::new(),
        operators: Vec::new(),
    }
    .invoke(function, arguments, depth, function.name.span)
}

/// Evaluate an existing call's borrowed parts without erasing argument types.
pub(crate) fn evaluate_call(
    callee: &Expr,
    arguments: &[Expr],
    environment: &dyn Environment,
    depth: u32,
) -> Result<Value, NotConstant> {
    Interpreter {
        environment,
        locals: Vec::new(),
        operators: Vec::new(),
    }
    .call(callee, arguments, depth, expr_span(callee))
}

struct LocalBinding<'a> {
    key: LocalKey<'a>,
    value: Option<Value>,
    ty: Option<&'a Type>,
}

struct LocalFrame<'a> {
    // A hoisted but uninitialized local must not fall through to a caller binding.
    bindings: Vec<LocalBinding<'a>>,
    receiver: Option<DefId>,
    return_type: Option<&'a Type>,
    applied: Option<AppliedArguments<'a>>,
}

struct Interpreter<'a> {
    environment: &'a dyn Environment,
    locals: Vec<LocalFrame<'a>>,
    operators: Vec<(&'a FnDecl, Option<&'a ImplDecl>)>,
}

impl<'a> Interpreter<'a> {
    fn active_operator(&self, selected: &SelectedFunction<'_>) -> bool {
        self.operators.iter().any(|(function, implementation)| {
            std::ptr::eq(*function, selected.function)
                && match (implementation, selected.implementation) {
                    (Some(active), Some(selected)) => std::ptr::eq(*active, selected),
                    (None, None) => true,
                    _ => false,
                }
        })
    }

    fn expression(
        &mut self,
        expression: &Expr,
        expected_enum: Option<DefId>,
        depth: u32,
    ) -> Result<Value, NotConstant> {
        let fail = |reason| NotConstant {
            span: expr_span(expression),
            reason,
        };
        if depth > 64 {
            return Err(fail(Reason::RecursionLimit));
        }
        let environment = self.environment;
        let value = match expression {
            Expr::Int { text, .. } => environment
                .literal_value(text)
                .ok_or_else(|| fail(Reason::Overflow))?,
            Expr::CharLit { ch, .. } => match expected_enum {
                Some(owner) => environment
                    .enum_literal(*ch, owner)
                    .ok_or_else(|| fail(Reason::Unsupported))?,
                None => Value::Character(*ch),
            },
            Expr::Path(path) => self.path_value(path, depth)?,
            Expr::Range { lo, hi, .. } => {
                let left = self.expression(lo, None, depth + 1)?;
                let right = self.expression(hi, None, depth + 1)?;
                environment
                    .source()
                    .and_then(|source| source.range_value(left, right))
                    .ok_or_else(|| fail(Reason::Unsupported))?
            }
            Expr::StrLit { .. }
            | Expr::Array { .. }
            | Expr::Tuple { .. }
            | Expr::Construct { .. } => self.aggregate_expression(expression, None, None, depth)?,
            Expr::Field { base, field, .. } => {
                let value = self.expression(base, None, depth + 1)?;
                self.project_field(value, field)
                    .ok_or_else(|| fail(Reason::Unsupported))?
            }
            Expr::Index { base, index, .. } => {
                let value = self.expression(base, None, depth + 1)?;
                let index = self
                    .expression(index, None, depth + 1)?
                    .integer()
                    .ok_or_else(|| fail(Reason::Unsupported))?;
                let Value::Array {
                    values,
                    left,
                    descending,
                    ..
                } = value
                else {
                    return Err(fail(Reason::Unsupported));
                };
                let offset = if descending {
                    i128::from(left).checked_sub(index)
                } else {
                    index.checked_sub(i128::from(left))
                };
                let offset = offset
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or_else(|| fail(Reason::Unsupported))?;
                values
                    .get(offset)
                    .cloned()
                    .ok_or_else(|| fail(Reason::Unsupported))?
            }
            Expr::SysAttr { base, attr, span } => match environment.attribute(base, attr) {
                Some(value) => Value::Integer(value),
                None => self.aggregate_attribute(base, attr, *span, depth + 1)?,
            },
            Expr::Unary { op, rhs, .. } => {
                if matches!(op, UnOp::Neg)
                    && matches!(environment.domain(), IntegerDomain::Analysis)
                    && depth < 64
                    && matches!(rhs.as_ref(), Expr::Int { text, .. }
                        if unsigned_lit_magnitude(text) == Some(i128::MIN.unsigned_abs()))
                {
                    return Ok(Value::Integer(i128::MIN));
                }
                if matches!(op, UnOp::Neg)
                    && matches!(environment.domain(), IntegerDomain::NativeWord)
                    && depth < 64
                    && matches!(rhs.as_ref(), Expr::Int { text, .. }
                        if unsigned_lit_text(text) == Some(i64::MIN.unsigned_abs()))
                {
                    return Ok(Value::Integer(i128::from(i64::MIN)));
                }
                let value = self.expression(rhs, None, depth + 1)?;
                if let Some(function) = environment.operator_function(expression, &value, None) {
                    if self.active_operator(&function) {
                        return Err(fail(Reason::Unsupported));
                    }
                    self.operators
                        .push((function.function, function.implementation));
                    let result = self.invoke_selected(
                        function,
                        vec![value],
                        depth + 1,
                        expr_span(expression),
                    );
                    self.operators.pop();
                    result?
                } else {
                    match (op, value) {
                        (UnOp::Neg, Value::Integer(value)) => Value::Integer(
                            value.checked_neg().ok_or_else(|| fail(Reason::Overflow))?,
                        ),
                        (UnOp::Neg, Value::Real(value)) => Value::Real(-value),
                        (_, value) => environment
                            .unary_value(expression, &value)
                            .ok_or_else(|| fail(Reason::Unsupported))?,
                    }
                }
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                // The typed operand, not the enclosing Bool result, supplies enum literal context.
                let (left, right) = if matches!(lhs.as_ref(), Expr::CharLit { .. })
                    || Self::numeric_literal_parts(lhs).is_some()
                {
                    let right = self.expression(rhs, None, depth + 1)?;
                    (self.contextual_expression(lhs, &right, depth + 1)?, right)
                } else {
                    let left = self.expression(lhs, None, depth + 1)?;
                    let right = self.contextual_expression(rhs, &left, depth + 1)?;
                    (left, right)
                };
                self.binary(expression, op, left, right, depth)?
            }
            Expr::IfExpr {
                cond, then, els, ..
            } => {
                let value = self.expression(cond, None, depth + 1)?;
                let selected = self.condition(value, depth + 1, expr_span(cond))?;
                self.expression(if selected { then } else { els }, expected_enum, depth + 1)?
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                let value = self.expression(scrutinee, None, depth + 1)?;
                let arm = self
                    .select_arm(arms, &value, expr_span(expression), depth + 1)?
                    .ok_or_else(|| fail(Reason::Unsupported))?;
                self.expression(
                    arm.value_expr().ok_or_else(|| fail(Reason::Unsupported))?,
                    expected_enum,
                    depth + 1,
                )?
            }
            Expr::Call { callee, args, .. } => {
                self.call(callee, args, depth, expr_span(expression))?
            }
            _ => return Err(fail(Reason::Unsupported)),
        };
        self.checked_value(value, expr_span(expression))
    }

    fn numeric_literal_parts(expression: &Expr) -> Option<(&str, bool)> {
        match expression {
            Expr::Int { text, .. } => Some((text, false)),
            Expr::Unary {
                op: UnOp::Neg, rhs, ..
            } => match rhs.as_ref() {
                Expr::Int { text, .. } => Some((text, true)),
                _ => None,
            },
            _ => None,
        }
    }

    /// A contextual literal is not a signed ABI word before its type is known.
    fn numeric_literal(
        &self,
        expression: &Expr,
        expected: &Ty,
    ) -> Option<Result<Value, NotConstant>> {
        let (text, negative) = Self::numeric_literal_parts(expression)?;
        let value = match expected {
            Ty::Natural => unsigned_lit_magnitude(text)
                .and_then(|value| u64::try_from(value).ok())
                .filter(|value| !negative || *value == 0)
                .map(Value::Natural),
            Ty::Real => {
                let value = if text.contains('.') {
                    text.replace('_', "").parse::<f64>().ok()
                } else {
                    unsigned_lit_magnitude(text).map(|value| value as f64)
                };
                value.map(|value| Value::Real(if negative { -value } else { value }))
            }
            _ => return None,
        };
        Some(value.ok_or(NotConstant {
            span: expr_span(expression),
            reason: Reason::Unsupported,
        }))
    }

    fn contextual_expression(
        &mut self,
        expression: &Expr,
        peer: &Value,
        depth: u32,
    ) -> Result<Value, NotConstant> {
        let expected = match peer {
            Value::Natural(_) => Some(Ty::Natural),
            Value::Real(_) => Some(Ty::Real),
            _ => None,
        };
        if let Some(value) = expected.and_then(|ty| self.numeric_literal(expression, &ty)) {
            return value;
        }
        self.expression(expression, peer.enum_owner(), depth)
    }

    fn path_value(&mut self, path: &Path, depth: u32) -> Result<Value, NotConstant> {
        let fail = || NotConstant {
            span: path.span,
            reason: Reason::UnknownConstant,
        };
        let environment = self.environment;
        let local = environment.local_key(path).and_then(|key| {
            self.locals
                .last()?
                .bindings
                .iter()
                .rev()
                .find(|binding| binding.key == key)
                .map(|binding| binding.value.clone())
        });
        if let Some(value) = local {
            value.ok_or_else(fail)
        } else {
            match environment.binding(path).ok_or_else(fail)? {
                Binding::Value(value) => Ok(value),
                Binding::Expression(value) => self.expression(value, None, depth + 1),
                Binding::TypedExpression { value, ty } => {
                    self.declared_expression(value, ty, None, depth + 1)
                }
            }
        }
    }

    fn select_arm<'m>(
        &mut self,
        arms: &'m [MatchArm],
        value: &Value,
        span: Span,
        depth: u32,
    ) -> Result<Option<&'m MatchArm>, NotConstant> {
        for arm in arms {
            if self.pattern(&arm.pattern, value, span, depth)? {
                return Ok(Some(arm));
            }
        }
        Ok(None)
    }

    fn pattern(
        &mut self,
        pattern: &Pattern,
        value: &Value,
        span: Span,
        depth: u32,
    ) -> Result<bool, NotConstant> {
        let span = pattern_span(pattern).unwrap_or(span);
        let fail = || NotConstant {
            span,
            reason: Reason::Unsupported,
        };
        if depth > 64 {
            return Err(NotConstant {
                span,
                reason: Reason::RecursionLimit,
            });
        }
        let environment = self.environment;
        let equivalent = |other| match environment.source() {
            Some(source) => source.equivalent(value, other),
            None => Some(value == other),
        };
        match pattern {
            Pattern::Wildcard => Ok(true),
            Pattern::Or { alts, .. } => {
                for alternative in alts {
                    if self.pattern(alternative, value, span, depth + 1)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Pattern::Path(path) => {
                let other = self.path_value(path, depth + 1)?;
                let other = self.checked_value(other, path.span)?;
                equivalent(&other).ok_or_else(fail)
            }
            Pattern::CharLit { ch, .. } => {
                let other = match value.enum_owner() {
                    Some(owner) => self.environment.enum_literal(*ch, owner).ok_or_else(fail)?,
                    None => Value::Character(*ch),
                };
                equivalent(&other).ok_or_else(fail)
            }
            Pattern::Range { lo, hi, .. } => match value {
                Value::Integer(_) => self.matches_bounds(
                    value,
                    Some(Value::Integer(i128::from(*lo))),
                    Some(Value::Integer(i128::from(*hi))),
                    span,
                    depth + 1,
                ),
                _ => Err(fail()),
            },
            Pattern::Bounds { lo, hi, .. } => {
                let low = lo
                    .as_deref()
                    .map(|bound| self.expression(bound, value.enum_owner(), depth + 1))
                    .transpose()?;
                let high = hi
                    .as_deref()
                    .map(|bound| self.expression(bound, value.enum_owner(), depth + 1))
                    .transpose()?;
                self.matches_bounds(value, low, high, span, depth + 1)
            }
            Pattern::BitPattern { .. } | Pattern::Tuple { .. } => Err(fail()),
        }
    }

    fn matches_bounds(
        &mut self,
        value: &Value,
        mut low: Option<Value>,
        mut high: Option<Value>,
        span: Span,
        depth: u32,
    ) -> Result<bool, NotConstant> {
        if let (Some(left), Some(right)) = (&low, &high) {
            match self.range_order(left, right, span, depth)? {
                RangeOrder::Forward => {}
                RangeOrder::Reversed => std::mem::swap(&mut low, &mut high),
                RangeOrder::Unordered => return Ok(false),
            }
        }
        if let Some(low) = low {
            if !self.less_equal(&low, value, span, depth)? {
                return Ok(false);
            }
        }
        match high {
            Some(high) => self.less_equal(value, &high, span, depth),
            None => Ok(true),
        }
    }

    fn range_order(
        &mut self,
        left: &Value,
        right: &Value,
        span: Span,
        depth: u32,
    ) -> Result<RangeOrder, NotConstant> {
        if self.less_equal(left, right, span, depth)? {
            Ok(RangeOrder::Forward)
        } else if self.less_equal(right, left, span, depth)? {
            Ok(RangeOrder::Reversed)
        } else {
            Ok(RangeOrder::Unordered)
        }
    }

    fn less_equal(
        &mut self,
        left: &Value,
        right: &Value,
        span: Span,
        depth: u32,
    ) -> Result<bool, NotConstant> {
        let fail = || NotConstant {
            span,
            reason: Reason::Unsupported,
        };
        let source = self.environment.source();
        let value = if let Some(function) =
            source.and_then(|source| source.binary_function(&BinOp::Le, left, right))
        {
            if self.active_operator(&function) {
                source
                    .unwrap()
                    .binary_values(&BinOp::Le, left, right, true)
                    .ok_or_else(fail)?
            } else {
                self.operators
                    .push((function.function, function.implementation));
                let result = self.invoke_selected(
                    function,
                    vec![left.clone(), right.clone()],
                    depth + 1,
                    span,
                );
                self.operators.pop();
                result?
            }
        } else {
            match (left, right) {
                (Value::Integer(left), Value::Integer(right)) => return Ok(left <= right),
                (Value::Character(left), Value::Character(right)) => return Ok(left <= right),
                _ => {}
            }
            source
                .ok_or_else(fail)?
                .binary_values(&BinOp::Le, left, right, false)
                .ok_or_else(fail)?
        };
        self.condition(value, depth + 1, span)
    }

    fn call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        depth: u32,
        span: Span,
    ) -> Result<Value, NotConstant> {
        let fail = |reason| NotConstant { span, reason };
        if depth > 64 {
            return Err(fail(Reason::RecursionLimit));
        }
        let environment = self.environment;
        if let Some(source) = environment.source() {
            let receiver = self.locals.last().and_then(|frame| frame.receiver);
            if let Some(target) = source.nominal_constructor(callee, receiver) {
                let [argument] = args else {
                    return Err(fail(Reason::ArityMismatch));
                };
                let value = self.expression(argument, None, depth + 1)?;
                if let Some(selected) = source.conversion_function(target, &value).map_err(fail)? {
                    if !self.active_operator(&selected) {
                        self.operators
                            .push((selected.function, selected.implementation));
                        let result = self.invoke_selected(selected, vec![value], depth + 1, span);
                        self.operators.pop();
                        return result;
                    }
                }
                return self.derived_conversion(target, value, depth + 1, span);
            }
        }
        if environment.transparent_conversion(callee) {
            if args.len() != 1 {
                return Err(fail(Reason::ArityMismatch));
            }
            let value = self.expression(&args[0], None, depth + 1)?;
            let value = environment
                .convert_value(callee, value)
                .ok_or_else(|| fail(Reason::Unsupported))?;
            return self.checked_value(value, span);
        }
        let (function, receiver) = if let Expr::Field { base, field, .. } = callee {
            let receiver = self.expression(base, None, depth + 1)?;
            (
                environment.method_function(field, &receiver),
                Some(receiver),
            )
        } else {
            (environment.function(callee), None)
        };
        let function = function.ok_or_else(|| fail(Reason::Unsupported))?;
        let parameters = function
            .params
            .iter()
            .filter(|parameter| !parameter.is_self);
        if parameters.clone().count() != args.len()
            || function.params.iter().any(|parameter| parameter.is_self) != receiver.is_some()
        {
            return Err(fail(Reason::ArityMismatch));
        }
        let mut values = Vec::with_capacity(function.params.len());
        let receiver_owner = receiver
            .as_ref()
            .and_then(|value| environment.source()?.value_type(value));
        values.extend(receiver);
        for (parameter, argument) in parameters.zip(args) {
            values.push(match parameter.ty.as_ref() {
                Some(ty) => self.declared_expression(argument, ty, receiver_owner, depth + 1)?,
                None => self.expression(argument, None, depth + 1)?,
            });
        }
        let value = self.invoke(function, values, depth + 1, span)?;
        self.checked_value(value, span)
    }

    fn checked_value(&self, value: Value, span: Span) -> Result<Value, NotConstant> {
        match value {
            Value::Integer(value) => self
                .environment
                .domain()
                .check(value)
                .map(Value::Integer)
                .ok_or(NotConstant {
                    span,
                    reason: Reason::Overflow,
                }),
            _ => Ok(value),
        }
    }

    fn binary(
        &mut self,
        expression: &Expr,
        op: &BinOp,
        left: Value,
        right: Value,
        depth: u32,
    ) -> Result<Value, NotConstant> {
        let fail = |reason| NotConstant {
            span: expr_span(expression),
            reason,
        };
        if let (Value::Integer(a), Value::Integer(b)) = (&left, &right) {
            let (a, b) = (*a, *b);
            let value = match op {
                BinOp::Add => a.checked_add(b).ok_or_else(|| fail(Reason::Overflow))?,
                BinOp::Sub => a.checked_sub(b).ok_or_else(|| fail(Reason::Overflow))?,
                BinOp::Mul => a.checked_mul(b).ok_or_else(|| fail(Reason::Overflow))?,
                BinOp::Div | BinOp::Rem if b == 0 => return Err(fail(Reason::DivisionByZero)),
                BinOp::Div | BinOp::Rem
                    if matches!(self.environment.domain(), IntegerDomain::NativeWord)
                        && a == i128::from(i64::MIN)
                        && b == -1 =>
                {
                    return Err(fail(Reason::Overflow))
                }
                BinOp::Div => a.checked_div(b).ok_or_else(|| fail(Reason::Overflow))?,
                BinOp::Rem => a.checked_rem(b).ok_or_else(|| fail(Reason::Overflow))?,
                BinOp::Shl | BinOp::Shr => {
                    let shift = u32::try_from(b)
                        .ok()
                        .filter(|shift| *shift < self.environment.domain().bits())
                        .ok_or_else(|| fail(Reason::InvalidShift))?;
                    let value = if matches!(op, BinOp::Shl) {
                        a.checked_shl(shift)
                    } else {
                        a.checked_shr(shift)
                    }
                    .ok_or_else(|| fail(Reason::Overflow))?;
                    match self.environment.domain() {
                        IntegerDomain::Analysis => value,
                        IntegerDomain::NativeWord => i128::from(value as i64),
                    }
                }
                _ => return self.semantic_binary(expression, left, right, depth),
            };
            return Ok(Value::Integer(value));
        }
        if let (Value::Natural(a), Value::Natural(b)) = (&left, &right) {
            let (a, b) = (*a, *b);
            let value = match op {
                BinOp::Add => a.checked_add(b),
                BinOp::Sub => a.checked_sub(b),
                BinOp::Mul => a.checked_mul(b),
                BinOp::Div | BinOp::Rem if b == 0 => return Err(fail(Reason::DivisionByZero)),
                BinOp::Div => a.checked_div(b),
                BinOp::Rem => a.checked_rem(b),
                BinOp::Shl | BinOp::Shr => {
                    let shift = u32::try_from(b)
                        .ok()
                        .filter(|shift| *shift < u64::BITS)
                        .ok_or_else(|| fail(Reason::InvalidShift))?;
                    if matches!(op, BinOp::Shl) {
                        a.checked_shl(shift)
                    } else {
                        a.checked_shr(shift)
                    }
                }
                _ => return self.semantic_binary(expression, left, right, depth),
            }
            .ok_or_else(|| fail(Reason::Overflow))?;
            return Ok(Value::Natural(value));
        }
        if let (Value::Real(a), Value::Real(b)) = (&left, &right) {
            let value = match op {
                BinOp::Add => a + b,
                BinOp::Sub => a - b,
                BinOp::Mul => a * b,
                BinOp::Div => a / b,
                BinOp::Rem => a % b,
                _ => return self.semantic_binary(expression, left, right, depth),
            };
            return Ok(Value::Real(value));
        }
        self.semantic_binary(expression, left, right, depth)
    }

    fn semantic_binary(
        &mut self,
        expression: &Expr,
        left: Value,
        right: Value,
        depth: u32,
    ) -> Result<Value, NotConstant> {
        if let Some(function) = self
            .environment
            .operator_function(expression, &left, Some(&right))
        {
            if self.active_operator(&function) {
                return self
                    .environment
                    .intrinsic_binary_value(expression, &left, &right)
                    .ok_or(NotConstant {
                        span: expr_span(expression),
                        reason: Reason::Unsupported,
                    });
            }
            self.operators
                .push((function.function, function.implementation));
            let result = self.invoke_selected(
                function,
                vec![left, right],
                depth + 1,
                expr_span(expression),
            );
            self.operators.pop();
            result
        } else {
            self.environment
                .binary_value(expression, &left, &right)
                .ok_or(NotConstant {
                    span: expr_span(expression),
                    reason: Reason::Unsupported,
                })
        }
    }

    fn invoke(
        &mut self,
        function: &'a FnDecl,
        arguments: Vec<Value>,
        depth: u32,
        span: Span,
    ) -> Result<Value, NotConstant> {
        self.invoke_selected(
            SelectedFunction {
                function,
                implementation: None,
                applied: None,
            },
            arguments,
            depth,
            span,
        )
    }

    fn invoke_selected(
        &mut self,
        selected: SelectedFunction<'a>,
        arguments: Vec<Value>,
        depth: u32,
        span: Span,
    ) -> Result<Value, NotConstant> {
        let function = selected.function;
        let fail = |reason| NotConstant { span, reason };
        if depth > 64 {
            return Err(fail(Reason::RecursionLimit));
        }
        if function.params.len() != arguments.len() {
            return Err(fail(Reason::ArityMismatch));
        }
        let body = function
            .body
            .as_ref()
            .ok_or_else(|| fail(Reason::Unsupported))?;
        let receiver = function
            .params
            .iter()
            .zip(&arguments)
            .find_map(|(parameter, value)| {
                parameter
                    .is_self
                    .then(|| self.environment.source()?.value_type(value))
                    .flatten()
            })
            .or_else(|| {
                self.environment
                    .source()?
                    .type_id(&selected.implementation?.target, None)
            });
        let expected = function
            .ret
            .as_ref()
            .and_then(|ty| self.environment.enum_type(ty, receiver));
        let bindings = function
            .params
            .iter()
            .zip(arguments)
            .map(|(parameter, value)| {
                let value = match parameter.ty.as_ref() {
                    Some(ty) => {
                        let ty = match &selected.applied {
                            Some(applied) => self
                                .environment
                                .source()
                                .and_then(|source| source.instantiated_type(applied, ty))
                                .ok_or_else(|| fail(Reason::Unsupported))?,
                            None => std::borrow::Cow::Borrowed(ty),
                        };
                        self.bind_concrete(&ty, value, receiver, depth, span)?
                    }
                    None => value,
                };
                Ok(LocalBinding {
                    key: self
                        .environment
                        .parameter_key(parameter)
                        .ok_or_else(|| fail(Reason::Unsupported))?,
                    value: Some(value),
                    ty: parameter.ty.as_ref(),
                })
            })
            .collect::<Result<Vec<_>, NotConstant>>()?;
        self.locals.push(LocalFrame {
            bindings,
            receiver,
            return_type: function.ret.as_ref(),
            applied: selected.applied,
        });
        let result = self.statements(&body.stmts, expected, depth);
        self.locals.pop();
        result?.ok_or_else(|| fail(Reason::Unsupported))
    }

    fn condition(&mut self, value: Value, depth: u32, span: Span) -> Result<bool, NotConstant> {
        if depth > 64 {
            return Err(NotConstant {
                span,
                reason: Reason::RecursionLimit,
            });
        }
        if let Some(condition) = self.environment.condition_value(&value) {
            return Ok(condition);
        }
        if let Some(function) = self.environment.condition_function(&value) {
            let converted = self.invoke_selected(function, vec![value], depth + 1, span)?;
            return self.condition(converted, depth + 1, span);
        }
        Err(NotConstant {
            span,
            reason: Reason::Unsupported,
        })
    }

    fn statements(
        &mut self,
        statements: &'a [Stmt],
        expected_enum: Option<DefId>,
        depth: u32,
    ) -> Result<Option<Value>, NotConstant> {
        let root = self.locals.is_empty();
        if root {
            self.locals.push(LocalFrame {
                bindings: Vec::new(),
                receiver: None,
                return_type: None,
                applied: None,
            });
        }
        let start = self.locals.last().unwrap().bindings.len();
        let result = self.run_statements(statements, expected_enum, depth);
        self.locals.last_mut().unwrap().bindings.truncate(start);
        if root {
            self.locals.pop();
        }
        result
    }

    fn run_statements(
        &mut self,
        statements: &'a [Stmt],
        expected_enum: Option<DefId>,
        depth: u32,
    ) -> Result<Option<Value>, NotConstant> {
        for statement in statements {
            if let Stmt::Let(declaration) = statement {
                let key =
                    self.environment
                        .declaration_key(&declaration.name)
                        .ok_or(NotConstant {
                            span: declaration.name.span,
                            reason: Reason::Unsupported,
                        })?;
                self.locals.last_mut().unwrap().bindings.push(LocalBinding {
                    key,
                    value: None,
                    ty: declaration.ty.as_ref(),
                });
            }
        }
        for statement in statements {
            let fail = |reason| NotConstant {
                span: stmt_span(statement),
                reason,
            };
            if depth > 64 {
                return Err(fail(Reason::RecursionLimit));
            }
            match statement {
                Stmt::Use(_) => {}
                Stmt::Let(declaration) => {
                    let key = self
                        .environment
                        .declaration_key(&declaration.name)
                        .ok_or_else(|| fail(Reason::Unsupported))?;
                    let expression = declaration
                        .value
                        .as_ref()
                        .ok_or_else(|| fail(Reason::Unsupported))?;
                    let value = match declaration.ty.as_ref() {
                        Some(ty) => self.declared_expression(
                            expression,
                            ty,
                            self.locals.last().unwrap().receiver,
                            depth,
                        )?,
                        None => self.expression(expression, None, depth)?,
                    };
                    self.locals
                        .last_mut()
                        .unwrap()
                        .bindings
                        .iter_mut()
                        .rev()
                        .find(|binding| binding.key == key)
                        .unwrap()
                        .value = Some(value);
                }
                Stmt::Assign {
                    target: Expr::Path(path),
                    value,
                    after: None,
                    ..
                } => {
                    let key = self
                        .environment
                        .local_key(path)
                        .ok_or_else(|| fail(Reason::Unsupported))?;
                    let binding = self
                        .locals
                        .last()
                        .unwrap()
                        .bindings
                        .iter()
                        .rev()
                        .find(|binding| binding.key == key)
                        .ok_or_else(|| fail(Reason::Unsupported))?;
                    let previous = binding
                        .value
                        .as_ref()
                        .ok_or_else(|| fail(Reason::Unsupported))?;
                    let value = match binding.ty {
                        Some(ty) => self.declared_expression(
                            value,
                            ty,
                            self.locals.last().unwrap().receiver,
                            depth,
                        )?,
                        None => {
                            let previous = previous.clone();
                            self.contextual_expression(value, &previous, depth)?
                        }
                    };
                    self.locals
                        .last_mut()
                        .unwrap()
                        .bindings
                        .iter_mut()
                        .rev()
                        .find(|binding| binding.key == key)
                        .unwrap()
                        .value = Some(value);
                }
                Stmt::Return { value, .. } => {
                    let value = value.as_ref().ok_or_else(|| fail(Reason::Unsupported))?;
                    return match self.locals.last().unwrap().return_type {
                        Some(ty) => self.declared_expression(
                            value,
                            ty,
                            self.locals.last().unwrap().receiver,
                            depth,
                        ),
                        None => self.expression(value, expected_enum, depth),
                    }
                    .map(Some);
                }
                Stmt::If(branch) => {
                    if let Some(value) = self.branch(branch, expected_enum, depth)? {
                        return Ok(Some(value));
                    }
                }
                Stmt::Match(branch) => {
                    let value = self.expression(&branch.scrutinee, None, depth + 1)?;
                    if let Some(arm) =
                        self.select_arm(&branch.arms, &value, branch.span, depth + 1)?
                    {
                        if let Some(value) =
                            self.statements(&arm.body.stmts, expected_enum, depth)?
                        {
                            return Ok(Some(value));
                        }
                    }
                }
                Stmt::Expr(expression) => {
                    self.expression(expression, None, depth)?;
                }
                _ => return Err(fail(Reason::Unsupported)),
            }
        }
        Ok(None)
    }

    fn branch(
        &mut self,
        branch: &'a IfStmt,
        expected_enum: Option<DefId>,
        depth: u32,
    ) -> Result<Option<Value>, NotConstant> {
        if depth > 64 {
            return Err(NotConstant {
                span: expr_span(&branch.cond),
                reason: Reason::RecursionLimit,
            });
        }
        let mut branch = branch;
        loop {
            let value = self.expression(&branch.cond, None, depth)?;
            if self.condition(value, depth, expr_span(&branch.cond))? {
                return self.statements(&branch.then.stmts, expected_enum, depth);
            }
            match branch.else_.as_deref() {
                Some(ElseBranch::Block(block)) => {
                    return self.statements(&block.stmts, expected_enum, depth)
                }
                Some(ElseBranch::If(nested)) => branch = nested,
                None => return Ok(None),
            }
        }
    }
}

#[cfg(test)]
mod tests;
