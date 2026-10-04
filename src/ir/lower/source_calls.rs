//! Borrowed procedure bodies and scoped canonical arguments.

use super::source_places::Place;
use super::*;

#[derive(Clone)]
pub(super) enum Binding {
    Value(Val),
    Place(Place),
}

#[derive(Clone)]
pub(super) struct Argument {
    pub(super) binding: Binding,
    layout: Option<SourceLayout>,
    family: Option<String>,
}

pub(super) struct SourceCallFrame {
    arguments: HashMap<String, Argument>,
    /// The nearest function shape scope must be ours, not a pure callee's.
    shape_depth: usize,
    /// Callee lexical locals can shadow a parameter without capturing caller locals.
    block_base: usize,
}

impl<'a> Lowering<'a> {
    fn active_source_call(&self) -> Option<&SourceCallFrame> {
        let frame = self.source_calls.last()?;
        let depth = self
            .source_shapes
            .borrow()
            .iter()
            .rposition(|shape| shape.function)?
            + 1;
        (depth == frame.shape_depth).then_some(frame)
    }

    pub(super) fn call_argument(&self, root: &str) -> Option<&Argument> {
        let frame = self.active_source_call()?;
        if self
            .block_scopes
            .borrow()
            .iter()
            .skip(frame.block_base)
            .any(|scope| scope.contains_key(root))
        {
            return None;
        }
        frame.arguments.get(root)
    }

    pub(super) fn source_call_block_base(&self) -> usize {
        self.active_source_call()
            .map_or(0, |frame| frame.block_base)
    }

    /// Literal scalar arguments remain available to elaboration-time loops.
    /// Follow captured value identities, never substituted argument syntax.
    pub(super) fn source_call_constants(
        &self,
        env: &HashMap<String, i64>,
    ) -> Option<HashMap<String, i64>> {
        let frame = self.active_source_call()?;
        let mut constants = env.clone();
        for (name, argument) in &frame.arguments {
            if self.call_argument(name).is_none() {
                continue;
            }
            constants.remove(name);
            if !argument.layout.as_ref().is_some_and(|layout| {
                matches!(
                    layout.kind,
                    LayoutKind::Scalar { width: 1..=64, .. }
                        | LayoutKind::Packed { width: 1..=64, .. }
                )
            }) {
                continue;
            }
            let value = match &argument.binding {
                Binding::Value(value) => value.clone(),
                Binding::Place(place) => self.read_place(place, place.layout.span),
            };
            let Val::Scalar(mut value) = value else {
                continue;
            };
            loop {
                match value {
                    Expr::Canonical { value: id, .. } => {
                        if self
                            .source_evaluated_width(&value)
                            .is_some_and(|width| width > 64)
                        {
                            break;
                        }
                        value = self.source_node(id);
                    }
                    Expr::Const(value) => {
                        constants.insert(name.clone(), value as i64);
                        break;
                    }
                    _ => break,
                }
            }
        }
        Some(constants)
    }

    pub(super) fn source_call_layout(&self, expression: &ast::Expr) -> Option<SourceLayout> {
        let (root, steps) = access_steps(expression)?;
        let mut layout = self.call_argument(&root)?.layout.clone()?;
        for step in steps {
            layout = match (step, &layout.kind) {
                (AccessStep::Field(name), LayoutKind::Struct { fields, .. }) => fields
                    .iter()
                    .find(|field| field.name == name)?
                    .layout
                    .clone(),
                (AccessStep::Index(_), LayoutKind::Array { element, .. }) => (**element).clone(),
                (AccessStep::Index(index), LayoutKind::Packed { .. }) => {
                    self.source_packed_result_layout(&layout, index)?
                }
                _ => return None,
            };
        }
        Some(layout)
    }

    pub(super) fn source_call_family(&self, expression: &ast::Expr) -> Option<String> {
        let (root, steps) = access_steps(expression)?;
        let argument = self.call_argument(&root)?;
        if steps.is_empty() {
            return argument.family.clone().or_else(|| {
                argument
                    .layout
                    .as_ref()
                    .and_then(Self::source_layout_family)
            });
        }
        Self::source_layout_family(&self.source_call_layout(expression)?)
    }

    pub(super) fn source_layout_family(layout: &SourceLayout) -> Option<String> {
        match &layout.kind {
            LayoutKind::Packed { family, .. } => Some(family.clone()),
            LayoutKind::Struct { name, view, .. } => Some(view.as_ref().unwrap_or(name).clone()),
            LayoutKind::Scalar {
                nominal: Some(name),
                ..
            } => Some(name.clone()),
            LayoutKind::Scalar {
                domain: ScalarDomain::Integer,
                ..
            } => Some("integer".to_owned()),
            LayoutKind::Scalar {
                domain: ScalarDomain::Real,
                ..
            } => Some("real".to_owned()),
            LayoutKind::Scalar {
                domain: ScalarDomain::Character,
                ..
            } => Some("Char".to_owned()),
            _ => None,
        }
    }

    pub(super) fn source_call_value(
        &self,
        expression: &ast::Expr,
        env: &HashMap<String, Val>,
    ) -> Option<Val> {
        let (root, _) = access_steps(expression)?;
        if env.contains_key(&root) {
            return None;
        }
        let argument = self.call_argument(&root)?;
        if let Binding::Place(place) = &argument.binding {
            let place = self.project_place(place.clone(), expression)?;
            return Some(self.read_place(&place, ast::expr_span(expression)));
        }
        let Binding::Value(value) = &argument.binding else {
            unreachable!();
        };
        self.source_access_value(expression, env, value.clone(), argument.layout.clone())
    }

    fn call_argument_binding(&self, expression: &ast::Expr, ty: Option<&ast::Type>) -> Argument {
        if let Some(place) = self.source_place(expression) {
            return Argument {
                layout: Some(place.layout.clone()),
                family: self.operand_type_name(expression),
                binding: Binding::Place(place),
            };
        }
        let (value, layout) = self.lower_source_argument(expression, ty, &HashMap::new());
        let value = match &layout {
            Some(layout) => {
                self.bind_value_layout(value, layout.clone(), ast::expr_span(expression))
            }
            None => self.bind_source_value(value, ast::expr_span(expression), None),
        };
        if let (Val::Scalar(Expr::Canonical { value, .. }), Some(meta)) =
            (&value, self.bit_string_meta(expression))
        {
            self.source_values
                .borrow_mut()
                .set_explicit_meta(*value, meta);
        }
        Argument {
            binding: Binding::Value(value),
            layout,
            family: self.operand_type_name(expression),
        }
    }

    /// Return false only when this is not a source procedure with a body.
    pub(super) fn lower_source_index_assign(
        &mut self,
        target: &ast::Expr,
        value: &ast::Expr,
        cond: Option<Expr>,
        pending: Option<&mut Vec<NextUpdate>>,
    ) -> bool {
        let ast::Expr::Index { base, index, .. } = target else {
            return false;
        };
        if !self
            .source_operand_layout(base, &HashMap::new())
            .is_some_and(|layout| matches!(layout.kind, LayoutKind::Struct { .. }))
        {
            return false;
        }
        let Some(index) = self.index_argument(index) else {
            return false;
        };
        self.lower_source_procedure(
            Some((base, "index_assign")),
            base,
            &[index, value.clone()],
            cond,
            pending,
        )
    }

    /// Return false only when this is not a source procedure with a body.
    pub(super) fn lower_source_procedure(
        &mut self,
        receiver: Option<(&ast::Expr, &str)>,
        callee: &ast::Expr,
        args: &[ast::Expr],
        cond: Option<Expr>,
        pending: Option<&mut Vec<NextUpdate>>,
    ) -> bool {
        let function = match receiver {
            Some((receiver, method)) => {
                let Some(family) = self.operand_type_name(receiver) else {
                    return false;
                };
                let input = args
                    .first()
                    .and_then(|argument| self.operand_type_name(argument));
                self.find_method(&family, method, input.as_deref())
            }
            None => self.free_fns.get(callee),
        };
        let Some(function) = function else {
            return false;
        };
        let Some(body) = &function.body else {
            return false;
        };
        if self.inline_depth.get() > 16 {
            self.depth_exceeded
                .borrow_mut()
                .push((function.name.text.clone(), ast::expr_span(callee)));
            return true;
        }
        // Build every binding in the caller before publishing the callee frame.
        let mut arguments = HashMap::new();
        if let Some((receiver, _)) = receiver {
            arguments.insert(
                "self".to_owned(),
                self.call_argument_binding(receiver, None),
            );
        }
        for (parameter, argument) in function
            .params
            .iter()
            .filter(|parameter| !parameter.is_self)
            .zip(args)
        {
            if let Some(name) = &parameter.name {
                arguments.insert(
                    name.text.clone(),
                    self.call_argument_binding(argument, parameter.ty.as_ref()),
                );
            }
        }
        let shapes = arguments
            .iter()
            .filter_map(|(name, argument)| Some((name.clone(), argument.layout.clone()?)))
            .collect();
        self.source_shapes
            .borrow_mut()
            .push(source_bindings::SourceShapeFrame {
                values: shapes,
                return_layout: None,
                function: true,
            });
        self.source_calls.push(SourceCallFrame {
            arguments,
            shape_depth: self.source_shapes.borrow().len(),
            block_base: self.block_scopes.borrow().len(),
        });
        self.inline_depth.set(self.inline_depth.get() + 1);
        match pending {
            Some(pending) => self.lower_event_block(body, cond, pending),
            None => self.lower_combinational_block(body, cond),
        }
        self.inline_depth.set(self.inline_depth.get() - 1);
        self.source_calls.pop();
        self.source_shapes.borrow_mut().pop();
        true
    }
}
