//! Shared constant expression and pure-function interpreter.
//!
//! Bindings and non-integer operators belong to the caller's semantic context;
//! traversal, checked arithmetic and control flow have one implementation.

use crate::diag::Span;
use crate::syntax::ast::{
    expr_span, stmt_span, BinOp, ElseBranch, Expr, FnDecl, FnParam, Ident, IfStmt, Path, Stmt,
    Type, UnOp,
};

use super::helpers::unsigned_lit_text;
use crate::resolve::declarations::DeclarationIndex;
use crate::resolve::DefId;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Value {
    Integer(i128),
    Character(char),
    Enum { owner: DefId, discriminant: u64 },
}

impl Value {
    pub(crate) fn integer(self) -> Option<i128> {
        match self {
            Self::Integer(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) fn enum_owner(self) -> Option<DefId> {
        match self {
            Self::Enum { owner, .. } => Some(owner),
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
    TypedExpression {
        value: &'a Expr,
        enum_owner: Option<DefId>,
    },
}

/// A stage supplies declaration identities and bound values without upward
/// dependencies. Unsupported operations remain unknown, never fabricated.
pub(crate) trait Environment {
    fn binding(&self, path: &Path) -> Option<Binding<'_>>;

    fn domain(&self) -> IntegerDomain {
        IntegerDomain::Analysis
    }

    fn literal(&self, text: &str) -> Option<i128> {
        unsigned_lit_text(text).map(i128::from)
    }

    /// A module constant must not be captured by a caller's same-named local.
    fn local_name<'p>(&self, path: &'p Path) -> Option<&'p str> {
        match path.segments.as_slice() {
            [name] => Some(&name.text),
            _ => None,
        }
    }

    fn local_key<'p>(&self, path: &'p Path) -> Option<LocalKey<'p>> {
        self.local_name(path).map(LocalKey::Name)
    }

    fn parameter_key<'p>(&self, parameter: &'p FnParam) -> Option<LocalKey<'p>> {
        if parameter.is_self {
            Some(LocalKey::Name("self"))
        } else {
            parameter
                .name
                .as_ref()
                .map(|name| LocalKey::Name(&name.text))
        }
    }

    fn enum_type(&self, _ty: &Type, _receiver: Option<DefId>) -> Option<DefId> {
        None
    }

    fn enum_literal(&self, _ch: char, _owner: DefId) -> Option<Value> {
        None
    }

    fn unary_value(&self, expression: &Expr, value: Value) -> Option<Value> {
        self.unary(expression, value.integer()?).map(Value::Integer)
    }

    fn binary_value(&self, expression: &Expr, left: Value, right: Value) -> Option<Value> {
        self.binary(expression, left.integer()?, right.integer()?)
            .map(Value::Integer)
    }

    fn condition_value(&self, value: Value) -> Option<bool> {
        self.condition(value.integer()?)
    }

    fn operator_function(
        &self,
        _expression: &Expr,
        _left: Value,
        _right: Option<Value>,
    ) -> Option<&FnDecl> {
        None
    }

    fn condition_function(&self, _value: Value) -> Option<&FnDecl> {
        None
    }

    fn method_function(&self, _method: &Ident, _receiver: Value) -> Option<&FnDecl> {
        None
    }

    fn attribute(&self, _base: &Expr, _attr: &Ident) -> Option<i128> {
        None
    }

    fn function(&self, _callee: &Expr) -> Option<&FnDecl> {
        None
    }

    fn transparent_conversion(&self, _callee: &Expr) -> bool {
        false
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
    fn binding(&self, path: &Path) -> Option<Binding<'_>> {
        self.integer_constant_path(path)
            .map(|declaration| Binding::Expression(&declaration.value))
    }

    fn function(&self, callee: &Expr) -> Option<&FnDecl> {
        let Expr::Path(path) = callee else {
            return None;
        };
        self.integer_function_path(path)
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

pub(crate) fn evaluate_statements(
    statements: &[Stmt],
    environment: &dyn Environment,
    depth: u32,
) -> Result<Option<i128>, NotConstant> {
    let Some(value) = evaluate_value_statements(statements, environment, None, depth)? else {
        return Ok(None);
    };
    value.integer().map(Some).ok_or(NotConstant {
        span: stmt_span(&statements[0]),
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
    }
    .expression(expression, expected_enum, depth)
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
    }
    .invoke(function, arguments, depth, function.name.span)
}

struct Interpreter<'a> {
    environment: &'a dyn Environment,
    locals: Vec<Vec<(LocalKey<'a>, Value)>>,
}

impl<'a> Interpreter<'a> {
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
            Expr::Int { text, .. } => Value::Integer(
                environment
                    .literal(text)
                    .ok_or_else(|| fail(Reason::Overflow))?,
            ),
            Expr::CharLit { ch, .. } => match expected_enum {
                Some(owner) => environment
                    .enum_literal(*ch, owner)
                    .ok_or_else(|| fail(Reason::Unsupported))?,
                None => Value::Character(*ch),
            },
            Expr::Path(path) => {
                let local = environment.local_key(path).and_then(|key| {
                    self.locals
                        .iter()
                        .rev()
                        .flat_map(|scope| scope.iter())
                        .find(|(bound, _)| *bound == key)
                        .map(|(_, value)| *value)
                });
                if let Some(value) = local {
                    value
                } else {
                    match environment
                        .binding(path)
                        .ok_or_else(|| fail(Reason::UnknownConstant))?
                    {
                        Binding::Value(value) => value,
                        Binding::Expression(value) => self.expression(value, None, depth + 1)?,
                        Binding::TypedExpression { value, enum_owner } => {
                            self.expression(value, enum_owner, depth + 1)?
                        }
                    }
                }
            }
            Expr::SysAttr { base, attr, .. } => Value::Integer(
                environment
                    .attribute(base, attr)
                    .ok_or_else(|| fail(Reason::Unsupported))?,
            ),
            Expr::Unary { op, rhs, .. } => {
                if matches!(op, UnOp::Neg)
                    && matches!(environment.domain(), IntegerDomain::NativeWord)
                    && depth < 64
                    && matches!(rhs.as_ref(), Expr::Int { text, .. }
                        if unsigned_lit_text(text) == Some(i64::MIN.unsigned_abs()))
                {
                    return Ok(Value::Integer(i128::from(i64::MIN)));
                }
                let value = self.expression(rhs, None, depth + 1)?;
                if let Some(function) = environment.operator_function(expression, value, None) {
                    self.invoke(function, vec![value], depth + 1, expr_span(expression))?
                } else {
                    match (op, value) {
                        (UnOp::Neg, Value::Integer(value)) => Value::Integer(
                            value.checked_neg().ok_or_else(|| fail(Reason::Overflow))?,
                        ),
                        _ => environment
                            .unary_value(expression, value)
                            .ok_or_else(|| fail(Reason::Unsupported))?,
                    }
                }
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                // The typed operand, not the enclosing Bool result, supplies enum literal context.
                let (left, right) = if matches!(lhs.as_ref(), Expr::CharLit { .. }) {
                    let right = self.expression(rhs, None, depth + 1)?;
                    (self.expression(lhs, right.enum_owner(), depth + 1)?, right)
                } else {
                    let left = self.expression(lhs, None, depth + 1)?;
                    (left, self.expression(rhs, left.enum_owner(), depth + 1)?)
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
            Expr::Call { callee, args, .. } => {
                if environment.transparent_conversion(callee) {
                    if args.len() != 1 {
                        return Err(fail(Reason::ArityMismatch));
                    }
                    return self.expression(&args[0], expected_enum, depth + 1);
                }
                let (function, receiver) = if let Expr::Field { base, field, .. } = callee.as_ref()
                {
                    let receiver = self.expression(base, None, depth + 1)?;
                    (environment.method_function(field, receiver), Some(receiver))
                } else {
                    (environment.function(callee), None)
                };
                let function = function.ok_or_else(|| fail(Reason::Unsupported))?;
                let parameters = function
                    .params
                    .iter()
                    .filter(|parameter| !parameter.is_self);
                if parameters.clone().count() != args.len()
                    || function.params.iter().any(|parameter| parameter.is_self)
                        != receiver.is_some()
                {
                    return Err(fail(Reason::ArityMismatch));
                }
                let mut values = Vec::with_capacity(function.params.len());
                values.extend(receiver);
                for (parameter, argument) in parameters.zip(args) {
                    let expected = parameter.ty.as_ref().and_then(|ty| {
                        environment.enum_type(ty, receiver.and_then(Value::enum_owner))
                    });
                    values.push(self.expression(argument, expected, depth + 1)?);
                }
                self.invoke(function, values, depth + 1, expr_span(expression))?
            }
            _ => return Err(fail(Reason::Unsupported)),
        };
        match value {
            Value::Integer(value) => environment
                .domain()
                .check(value)
                .map(Value::Integer)
                .ok_or_else(|| fail(Reason::Overflow)),
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
        if let (Value::Integer(a), Value::Integer(b)) = (left, right) {
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
            .operator_function(expression, left, Some(right))
        {
            self.invoke(
                function,
                vec![left, right],
                depth + 1,
                expr_span(expression),
            )
        } else {
            self.environment
                .binary_value(expression, left, right)
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
            .find_map(|(parameter, value)| parameter.is_self.then(|| value.enum_owner()).flatten());
        let expected = function
            .ret
            .as_ref()
            .and_then(|ty| self.environment.enum_type(ty, receiver));
        let bindings = function
            .params
            .iter()
            .zip(arguments)
            .map(|(parameter, value)| {
                Ok((
                    self.environment
                        .parameter_key(parameter)
                        .ok_or_else(|| fail(Reason::Unsupported))?,
                    value,
                ))
            })
            .collect::<Result<Vec<_>, NotConstant>>()?;
        self.locals.push(bindings);
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
        if let Some(condition) = self.environment.condition_value(value) {
            return Ok(condition);
        }
        if let Some(function) = self.environment.condition_function(value) {
            let converted = self.invoke(function, vec![value], depth + 1, span)?;
            return self.condition(converted, depth + 1, span);
        }
        Err(NotConstant {
            span,
            reason: Reason::Unsupported,
        })
    }

    fn statements(
        &mut self,
        statements: &[Stmt],
        expected_enum: Option<DefId>,
        depth: u32,
    ) -> Result<Option<Value>, NotConstant> {
        for statement in statements {
            let fail = |reason| NotConstant {
                span: stmt_span(statement),
                reason,
            };
            if depth > 64 {
                return Err(fail(Reason::RecursionLimit));
            }
            match statement {
                Stmt::Return { value, .. } => {
                    return self
                        .expression(
                            value.as_ref().ok_or_else(|| fail(Reason::Unsupported))?,
                            expected_enum,
                            depth,
                        )
                        .map(Some)
                }
                Stmt::If(branch) => {
                    if let Some(value) = self.branch(branch, expected_enum, depth)? {
                        return Ok(Some(value));
                    }
                }
                _ => return Err(fail(Reason::Unsupported)),
            }
        }
        Ok(None)
    }

    fn branch(
        &mut self,
        branch: &IfStmt,
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
