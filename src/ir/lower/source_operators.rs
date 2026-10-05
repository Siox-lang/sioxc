//! Source-owned operator dispatch over already evaluated operands.

use super::expressions::bind_format_attrs;
use super::*;

pub(super) struct OperatorOperand {
    pub(super) value: Val,
    pub(super) family: Option<String>,
    pub(super) width: u32,
    pub(super) range: Option<(i64, i64)>,
    pub(super) layout: Option<SourceLayout>,
}

impl Lowering<'_> {
    pub(super) fn source_operator_operand(
        &self,
        expression: &ast::Expr,
        env: &HashMap<String, Val>,
        width: u32,
    ) -> OperatorOperand {
        OperatorOperand {
            family: self.operand_type_name(expression),
            range: self.operand_range(expression, env),
            layout: self.source_operand_layout(expression, env),
            width,
            value: self.bind_source_value(
                self.lower_val_env(expression, env),
                ast::expr_span(expression),
                None,
            ),
        }
    }

    /// Exact rhs family wins; kernel integer constants may use a Self rhs.
    /// Unknown rhs accepts a sole candidate, never a known mismatched one.
    pub(super) fn source_binary_operator(
        &self,
        op: &str,
        lhs: &str,
        rhs: Option<&str>,
        span: crate::diag::Span,
    ) -> Option<&ast::FnDecl> {
        let functions = self.op_impls.get(&(op.to_owned(), lhs.to_owned()))?;
        let declared = |function: &ast::FnDecl, argument: &Option<String>| {
            let family = argument.clone().or_else(|| {
                function
                    .params
                    .iter()
                    .find(|parameter| !parameter.is_self)
                    .and_then(|parameter| parameter.ty.as_ref())
                    .and_then(|ty| self.free_fns.type_head_key(ty))
            })?;
            Some(if family == "Self" {
                lhs.to_owned()
            } else {
                family
            })
        };
        let chosen = match rhs {
            Some(rhs) => functions
                .iter()
                .find(|(function, argument)| declared(function, argument).as_deref() == Some(rhs))
                .or_else(|| {
                    (rhs == "integer")
                        .then(|| {
                            functions.iter().find(|(function, argument)| {
                                declared(function, argument).as_deref() == Some(lhs)
                            })
                        })
                        .flatten()
                }),
            None => (functions.len() == 1).then(|| &functions[0]),
        };
        if chosen.is_none()
            && self
                .structs
                .get(lhs)
                .is_some_and(|st| !st.fields.is_empty())
        {
            self.bad_operators.borrow_mut().push((
                op.to_owned(),
                lhs.to_owned(),
                rhs.map(str::to_owned),
                span,
            ));
        }
        chosen.map(|(function, _)| *function)
    }

    pub(super) fn source_unary_operator(&self, op: &str, family: &str) -> Option<&ast::FnDecl> {
        self.op_impls
            .get(&(op.to_owned(), family.to_owned()))?
            .iter()
            .find(|(function, _)| function.params.iter().all(|parameter| parameter.is_self))
            .map(|(function, _)| *function)
    }

    /// Borrow the implementation body; bind values, not synthetic caller paths.
    /// Packed/scalar operands deliberately remain kernel words inside an impl.
    /// Struct shapes retain the information needed for aggregate field access.
    pub(super) fn inline_source_operator(
        &self,
        function: &ast::FnDecl,
        lhs: OperatorOperand,
        rhs: Option<OperatorOperand>,
    ) -> Option<Val> {
        let body = function.body.as_ref()?;
        let mut env = HashMap::new();
        let mut shapes = HashMap::new();
        let mut return_shapes = HashMap::new();
        let mut hidden = Vec::new();
        let mut bind = |name: &str, operand: OperatorOperand| {
            if let Some(layout) = operand.layout {
                if matches!(layout.kind, LayoutKind::Struct { .. }) {
                    shapes.insert(name.to_owned(), layout.clone());
                }
                return_shapes.insert(name.to_owned(), layout);
            }
            env.insert(name.to_owned(), operand.value);
            env.insert(
                format!("{name}::length"),
                Val::Scalar(Expr::Const(u64::from(operand.width))),
            );
            if let Some((left, right)) = operand.range {
                bind_format_attrs(&mut env, name, left, right);
            }
            hidden.push(name.to_owned());
        };
        bind("self", lhs);
        if let Some(rhs) = rhs {
            let name = &function
                .params
                .iter()
                .find(|parameter| !parameter.is_self)?
                .name
                .as_ref()?
                .text;
            bind(name, rhs);
        }
        let saved = hidden
            .into_iter()
            .map(|name| {
                let family = self.param_types.borrow_mut().remove(&name);
                let width = self.param_widths.borrow_mut().remove(&name);
                (name, family, width)
            })
            .collect::<Vec<_>>();
        let _shapes = self.source_shape_scope(
            shapes,
            self.source_function_return_layout(function, &return_shapes),
        );
        let result = self.inline_block(&body.stmts, &env);
        for (name, family, width) in saved {
            if let Some(family) = family {
                self.param_types.borrow_mut().insert(name.clone(), family);
            }
            if let Some(width) = width {
                self.param_widths.borrow_mut().insert(name, width);
            }
        }
        result
    }

    pub(super) fn native_bound_vector_logical(
        &self,
        op: &str,
        lhs: &OperatorOperand,
        rhs: &OperatorOperand,
        span: crate::diag::Span,
    ) -> Option<Expr> {
        if !matches!(op, "xor" | "nand" | "nor" | "xnor")
            || ![lhs, rhs].iter().all(|operand| {
                operand
                    .family
                    .as_ref()
                    .is_some_and(|family| self.out.array_element_of_family.contains_key(family))
            })
        {
            return None;
        }
        let (Val::Scalar(a), Val::Scalar(b)) = (&lhs.value, &rhs.value) else {
            return None;
        };
        let operation = match op {
            "xor" | "xnor" => BinOp::Xor,
            "nand" => BinOp::And,
            _ => BinOp::Or,
        };
        let inner = self
            .source_values
            .borrow_mut()
            .binary(operation, a, b, span);
        if op == "xor" {
            return Some(inner);
        }
        let mut ones = vec![0u64; (lhs.width as usize).max(1).div_ceil(64)];
        for bit in 0..lhs.width {
            ones[bit as usize / 64] |= 1u64 << (bit % 64);
        }
        Some(
            self.source_values
                .borrow_mut()
                .binary(BinOp::Xor, &inner, &words_const(ones), span),
        )
    }
}
