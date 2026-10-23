//! Shared constant expression and pure-function interpreter.
//!
//! Bindings and non-integer operators belong to the caller's semantic context;
//! traversal, checked arithmetic and control flow have one implementation.

use crate::diag::Span;
use crate::syntax::ast::{
    expr_span, stmt_span, BinOp, ElseBranch, Expr, FnDecl, Ident, IfStmt, Path, Stmt, UnOp,
};

use super::declarations::DeclarationIndex;
use super::helpers::unsigned_lit_text;

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

pub(crate) enum Binding<'a> {
    Value(i128),
    Expression(&'a Expr),
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
    Interpreter {
        environment,
        locals: Vec::new(),
    }
    .expression(expression, depth)
}

pub(crate) fn evaluate_statements(
    statements: &[Stmt],
    environment: &dyn Environment,
    depth: u32,
) -> Result<Option<i128>, NotConstant> {
    Interpreter {
        environment,
        locals: Vec::new(),
    }
    .statements(statements, depth)
}

struct Interpreter<'a> {
    environment: &'a dyn Environment,
    locals: Vec<Vec<(&'a str, i128)>>,
}

impl<'a> Interpreter<'a> {
    fn expression(&mut self, expression: &Expr, depth: u32) -> Result<i128, NotConstant> {
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
                .literal(text)
                .ok_or_else(|| fail(Reason::Overflow))?,
            Expr::Path(path) => {
                let local = environment.local_name(path).and_then(|name| {
                    self.locals
                        .iter()
                        .rev()
                        .flat_map(|scope| scope.iter())
                        .find(|(key, _)| *key == name)
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
                        Binding::Expression(value) => self.expression(value, depth + 1)?,
                    }
                }
            }
            Expr::SysAttr { base, attr, .. } => environment
                .attribute(base, attr)
                .ok_or_else(|| fail(Reason::Unsupported))?,
            Expr::Unary { op, rhs, .. } => {
                // The minimum signed literal has a positive magnitude outside
                // its domain; negate that magnitude before checking the result.
                if matches!(op, UnOp::Neg)
                    && matches!(environment.domain(), IntegerDomain::NativeWord)
                    && depth < 64
                    && matches!(rhs.as_ref(), Expr::Int { text, .. }
                        if unsigned_lit_text(text) == Some(i64::MIN.unsigned_abs()))
                {
                    return Ok(i128::from(i64::MIN));
                }
                let value = self.expression(rhs, depth + 1)?;
                match op {
                    UnOp::Neg => value.checked_neg().ok_or_else(|| fail(Reason::Overflow))?,
                    _ => environment
                        .unary(expression, value)
                        .ok_or_else(|| fail(Reason::Unsupported))?,
                }
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                let (a, b) = (
                    self.expression(lhs, depth + 1)?,
                    self.expression(rhs, depth + 1)?,
                );
                match op {
                    BinOp::Add => a.checked_add(b).ok_or_else(|| fail(Reason::Overflow))?,
                    BinOp::Sub => a.checked_sub(b).ok_or_else(|| fail(Reason::Overflow))?,
                    BinOp::Mul => a.checked_mul(b).ok_or_else(|| fail(Reason::Overflow))?,
                    BinOp::Div | BinOp::Rem if b == 0 => return Err(fail(Reason::DivisionByZero)),
                    BinOp::Div | BinOp::Rem
                        if matches!(environment.domain(), IntegerDomain::NativeWord)
                            && a == i128::from(i64::MIN)
                            && b == -1 =>
                    {
                        return Err(fail(Reason::Overflow));
                    }
                    BinOp::Div => a.checked_div(b).ok_or_else(|| fail(Reason::Overflow))?,
                    BinOp::Rem => a.checked_rem(b).ok_or_else(|| fail(Reason::Overflow))?,
                    BinOp::Shl | BinOp::Shr => {
                        let shift = u32::try_from(b)
                            .ok()
                            .filter(|shift| *shift < environment.domain().bits())
                            .ok_or_else(|| fail(Reason::InvalidShift))?;
                        let value = if matches!(op, BinOp::Shl) {
                            a.checked_shl(shift)
                        } else {
                            a.checked_shr(shift)
                        }
                        .ok_or_else(|| fail(Reason::Overflow))?;
                        // Kernel shifts discard bits outside their word, unlike
                        // checked addition; retain that ABI distinction.
                        match environment.domain() {
                            IntegerDomain::Analysis => value,
                            IntegerDomain::NativeWord => i128::from(value as i64),
                        }
                    }
                    _ => environment
                        .binary(expression, a, b)
                        .ok_or_else(|| fail(Reason::Unsupported))?,
                }
            }
            Expr::IfExpr {
                cond, then, els, ..
            } => {
                let value = self.expression(cond, depth + 1)?;
                let selected = environment
                    .condition(value)
                    .ok_or_else(|| fail(Reason::Unsupported))?;
                self.expression(if selected { then } else { els }, depth + 1)?
            }
            Expr::Call { callee, args, .. } => {
                if environment.transparent_conversion(callee) {
                    if args.len() != 1 {
                        return Err(fail(Reason::ArityMismatch));
                    }
                    return self.expression(&args[0], depth + 1);
                }
                let function = environment
                    .function(callee)
                    .ok_or_else(|| fail(Reason::Unsupported))?;
                let body = function
                    .body
                    .as_ref()
                    .ok_or_else(|| fail(Reason::Unsupported))?;
                let parameters = function
                    .params
                    .iter()
                    .filter(|parameter| !parameter.is_self);
                if parameters.clone().count() != args.len() {
                    return Err(fail(Reason::ArityMismatch));
                }
                let bindings = parameters
                    .zip(args)
                    .map(|(parameter, argument)| {
                        let name = parameter
                            .name
                            .as_ref()
                            .ok_or_else(|| fail(Reason::Unsupported))?;
                        Ok((name.text.as_str(), self.expression(argument, depth + 1)?))
                    })
                    .collect::<Result<Vec<_>, NotConstant>>()?;
                // Borrow module bindings; only this call's parameters allocate.
                self.locals.push(bindings);
                let result = self.statements(&body.stmts, depth + 1);
                self.locals.pop();
                result?.ok_or_else(|| fail(Reason::Unsupported))?
            }
            _ => return Err(fail(Reason::Unsupported)),
        };
        environment
            .domain()
            .check(value)
            .ok_or_else(|| fail(Reason::Overflow))
    }

    fn statements(&mut self, statements: &[Stmt], depth: u32) -> Result<Option<i128>, NotConstant> {
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
                            depth,
                        )
                        .map(Some);
                }
                Stmt::If(branch) => {
                    if let Some(value) = self.branch(branch, depth)? {
                        return Ok(Some(value));
                    }
                }
                _ => return Err(fail(Reason::Unsupported)),
            }
        }
        Ok(None)
    }

    fn branch(&mut self, branch: &IfStmt, depth: u32) -> Result<Option<i128>, NotConstant> {
        if depth > 64 {
            return Err(NotConstant {
                span: expr_span(&branch.cond),
                reason: Reason::RecursionLimit,
            });
        }
        let mut branch = branch;
        loop {
            let value = self.expression(&branch.cond, depth)?;
            let condition = self.environment.condition(value).ok_or(NotConstant {
                span: expr_span(&branch.cond),
                reason: Reason::Unsupported,
            })?;
            if condition {
                return self.statements(&branch.then.stmts, depth);
            }
            match branch.else_.as_deref() {
                Some(ElseBranch::Block(block)) => return self.statements(&block.stmts, depth),
                Some(ElseBranch::If(nested)) => branch = nested,
                None => return Ok(None),
            }
        }
    }
}

#[cfg(test)]
mod tests;
