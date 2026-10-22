//! Checked integer constant analysis, shared by widths, indices and patterns.
//!
//! This evaluates mathematical integer expressions, not packed values or
//! library-defined operators. Keep the wider analysis domain until the caller
//! checks its own bound; narrowing intermediates would change pattern coverage.

use crate::diag::Span;
use crate::syntax::ast::{expr_span, BinOp, Expr, Path, UnOp};

use super::helpers::unsigned_lit_text;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    Unsupported,
    UnknownConstant,
    Overflow,
    DivisionByZero,
    InvalidShift,
    RecursionLimit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NotConstant {
    pub span: Span,
    pub reason: Reason,
}

/// Evaluate an integer expression. The caller supplies resolver-selected
/// declarations, so this module need not know about checker or IR registries.
pub(crate) fn integer<'a>(
    expression: &Expr,
    constant: impl Fn(&Path) -> Option<&'a Expr>,
) -> Result<i128, NotConstant> {
    eval(expression, &constant, 0)
}

fn eval<'a>(
    expression: &Expr,
    constant: &impl Fn(&Path) -> Option<&'a Expr>,
    depth: u32,
) -> Result<i128, NotConstant> {
    let failure = |reason| NotConstant {
        span: expr_span(expression),
        reason,
    };
    // Bound both cyclic constants and adversarially deep expressions.
    if depth > 64 {
        return Err(failure(Reason::RecursionLimit));
    }
    let child = |e: &Expr| eval(e, constant, depth + 1);
    match expression {
        Expr::Int { text, .. } => unsigned_lit_text(text)
            .map(i128::from)
            .ok_or_else(|| failure(Reason::Overflow)),
        Expr::Path(path) => {
            let value = constant(path).ok_or_else(|| failure(Reason::UnknownConstant))?;
            child(value)
        }
        Expr::Unary {
            op: UnOp::Neg, rhs, ..
        } => child(rhs)?
            .checked_neg()
            .ok_or_else(|| failure(Reason::Overflow)),
        Expr::Binary { op, lhs, rhs, .. } => {
            if !matches!(
                op,
                BinOp::Add
                    | BinOp::Sub
                    | BinOp::Mul
                    | BinOp::Div
                    | BinOp::Rem
                    | BinOp::Shl
                    | BinOp::Shr
            ) {
                return Err(failure(Reason::Unsupported));
            }
            let (a, b) = (child(lhs)?, child(rhs)?);
            let value = match op {
                BinOp::Add => a.checked_add(b),
                BinOp::Sub => a.checked_sub(b),
                BinOp::Mul => a.checked_mul(b),
                BinOp::Div | BinOp::Rem if b == 0 => {
                    return Err(failure(Reason::DivisionByZero));
                }
                BinOp::Div => a.checked_div(b),
                BinOp::Rem => a.checked_rem(b),
                BinOp::Shl | BinOp::Shr => {
                    let shift = u32::try_from(b)
                        .ok()
                        .filter(|shift| *shift < i128::BITS)
                        .ok_or_else(|| failure(Reason::InvalidShift))?;
                    if matches!(op, BinOp::Shl) {
                        a.checked_shl(shift)
                    } else {
                        a.checked_shr(shift)
                    }
                }
                _ => unreachable!(),
            };
            value.ok_or_else(|| failure(Reason::Overflow))
        }
        _ => Err(failure(Reason::Unsupported)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::{DiagnosticSink, FileId};
    use crate::syntax::{ast::Item, parse_module};

    fn expression(text: &str) -> Expr {
        let source = format!("module constants; const VALUE: integer = {text};");
        let mut sink = DiagnosticSink::default();
        let module = parse_module(FileId(0), &source, &mut sink);
        assert!(!sink.has_errors(), "{text}: {:?}", sink.diagnostics());
        let Item::Const(value) = module.items.into_iter().next().unwrap() else {
            panic!("expected a constant");
        };
        value.value
    }

    fn literal(text: &str) -> Result<i128, NotConstant> {
        integer(&expression(text), |_| None)
    }

    #[test]
    fn arithmetic_keeps_the_analysis_domain() {
        for (text, expected) in [
            ("0xF_f + 0b1_0", 257),
            ("-(2 + 3) * 4", -20),
            ("17 / 3 + 17 % 3", 7),
            ("(3 << 5) >> 2", 24),
            ("-0x8000_0000_0000_0000", i128::from(i64::MIN)),
            ("0xffff_ffff_ffff_ffff", i128::from(u64::MAX)),
            ("(-0x8000_0000_0000_0000) / -1", 1i128 << 63),
            ("(-0x8000_0000_0000_0000) % -1", 0),
            ("(0x7fff_ffff_ffff_ffff + 1) - 1", i128::from(i64::MAX)),
        ] {
            assert_eq!(literal(text), Ok(expected), "{text}");
        }
    }

    #[test]
    fn failures_are_spanned_and_do_not_panic() {
        for (text, expected) in [
            ("5 / 0", Reason::DivisionByZero),
            ("5 % 0", Reason::DivisionByZero),
            ("1 << -1", Reason::InvalidShift),
            ("1 << 128", Reason::InvalidShift),
            ("1 >> 4294967296", Reason::InvalidShift),
            (
                "0xffff_ffff_ffff_ffff * 0xffff_ffff_ffff_ffff",
                Reason::Overflow,
            ),
            ("(-1 << 127) / -1", Reason::Overflow),
            ("(-1 << 127) % -1", Reason::Overflow),
            ("-(-1 << 127)", Reason::Overflow),
            ("RUNTIME", Reason::UnknownConstant),
            ("3 and 2", Reason::Unsupported),
            ("not 0", Reason::Unsupported),
            ("3 == 3", Reason::Unsupported),
            ("function(2)", Reason::Unsupported),
        ] {
            let expression = expression(text);
            let error = integer(&expression, |_| None).unwrap_err();
            assert_eq!(error.reason, expected, "{text}");
            assert_eq!(error.span, expr_span(&expression));
        }
    }

    #[test]
    fn declarations_are_supplied_by_identity_not_leaf_name() {
        let value = expression("6 * 7");
        let alias = expression("library::VALUE");
        let expr = expression("ALIAS + 1");
        let result = integer(&expr, |path| {
            let names: Vec<_> = path.segments.iter().map(|s| s.text.as_str()).collect();
            match names.as_slice() {
                ["ALIAS"] => Some(&alias),
                ["library", "VALUE"] => Some(&value),
                _ => None,
            }
        });
        assert_eq!(result, Ok(43));
        assert_eq!(
            integer(&expression("VALUE"), |_| None).unwrap_err().reason,
            Reason::UnknownConstant
        );
    }

    #[test]
    fn cyclic_declarations_stop() {
        let a = expression("B");
        let b = expression("A");
        let result = integer(&a, |path| match path.segments[0].text.as_str() {
            "A" => Some(&a),
            "B" => Some(&b),
            _ => None,
        });
        assert_eq!(result.unwrap_err().reason, Reason::RecursionLimit);
    }

    #[test]
    fn checker_literal_boundary_is_checked_after_evaluation() {
        use super::super::Checker;

        for (text, expected) in [
            ("-0x8000_0000_0000_0000", Some(i64::MIN)),
            ("(-0x8000_0000_0000_0000) / -1", None),
            ("(-0x8000_0000_0000_0000) % -1", Some(0)),
            ("0xffff_ffff_ffff_ffff", None),
            ("(0x7fff_ffff_ffff_ffff + 1) - 1", Some(i64::MAX)),
            ("-(2 + 3)", Some(-5)),
            ("5 / 0", None),
        ] {
            assert_eq!(
                Checker::const_literal(&expression(text)),
                expected,
                "{text}"
            );
        }
    }

    #[test]
    fn checker_keeps_qualified_constants_and_parameter_shadowing() {
        use super::super::Checker;

        let mut sink = DiagnosticSink::default();
        let modules: Vec<_> = [
            "module alpha; pub const VALUE: integer = 300;",
            "module beta; pub const VALUE: integer = 2;",
            "module client; const TOTAL: integer = alpha::VALUE + beta::VALUE;
             fn identity(VALUE: integer) -> integer { return VALUE; }",
        ]
        .iter()
        .enumerate()
        .map(|(i, source)| parse_module(FileId(i as u32), source, &mut sink))
        .collect();
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
        let mut checker = Checker::new(&mut sink, &resolved, &modules);
        checker.collect(&modules);
        let Item::Const(total) = &modules[2].items[0] else {
            panic!("expected constant")
        };
        assert_eq!(checker.const_int(&total.value), Some(302));
        assert_eq!(checker.constant_value(&total.value), Some(302));
        assert_eq!(Checker::const_literal(&total.value), None);
        let Item::Fn(function) = &modules[2].items[1] else {
            panic!("expected function")
        };
        let crate::syntax::ast::Stmt::Return {
            value: Some(value), ..
        } = &function.body.as_ref().unwrap().stmts[0]
        else {
            panic!("expected return")
        };
        assert_eq!(checker.const_int(value), None);
        assert_eq!(checker.constant_value(value), None);
    }

    #[test]
    fn failure_points_to_the_operand_that_could_not_be_evaluated() {
        let e = expression("1 + (2 / 0)");
        let Expr::Binary { rhs, .. } = &e else {
            unreachable!()
        };
        let error = integer(&e, |_| None).unwrap_err();
        assert_eq!(error.reason, Reason::DivisionByZero);
        assert_eq!(error.span, expr_span(rhs));
    }
}
