//! Semantic trait signatures, after resolution checks method presence and arity.

use super::*;

impl<'a> Checker<'a> {
    pub(super) fn check_trait_signatures(&mut self, implementation: &ImplDecl) {
        let Some(contract) = implementation
            .trait_
            .as_ref()
            .and_then(|path| self.resolved.resolved(path.span))
            .and_then(|id| self.declarations.trait_decl(id))
        else {
            return;
        };
        let Ok(arguments) = self
            .declarations
            .trait_arguments(contract, &implementation.trait_args)
        else {
            return;
        };
        if !self.check_trait_value_arguments(contract, implementation, &arguments) {
            return;
        }
        for item in &implementation.items {
            let ImplItem::Fn(function) = item else {
                continue;
            };
            let Some(required) = contract
                .items
                .iter()
                .find(|method| method.name.text == function.name.text)
            else {
                continue;
            };
            let expected = required
                .params
                .iter()
                .filter(|parameter| !parameter.is_self);
            let actual = function
                .params
                .iter()
                .filter(|parameter| !parameter.is_self);
            // Resolution diagnoses invalid shapes; their type comparison would cascade.
            if required.params.iter().any(|p| p.is_self)
                != function.params.iter().any(|p| p.is_self)
                || expected.clone().count() != actual.clone().count()
            {
                continue;
            }
            for (position, (expected, actual)) in expected.zip(actual).enumerate() {
                let (Some(expected_type), Some(actual_type)) = (&expected.ty, &actual.ty) else {
                    continue;
                };
                let Some(expected_type) = self
                    .declarations
                    .instantiate_trait_type(&arguments, expected_type)
                else {
                    continue;
                };
                let expected_type = self.ast_ty(&expected_type);
                let Some(actual_type) = self.declarations.normalize_type(actual_type) else {
                    continue;
                };
                let actual_type = self.ast_ty(&actual_type);
                if !signature_types_match(&expected_type, &actual_type) {
                    self.sink.emit(
                        Diagnostic::error(format!(
                            "parameter {} of `{}` has type {}, but trait `{}` requires {}",
                            position + 1,
                            function.name.text,
                            self.ty_display(&actual_type),
                            contract.name.text,
                            self.ty_display(&expected_type)
                        ))
                        .with_code(codes::TYPE_MISMATCH)
                        .at(actual.span)
                        .label(expected.span, "trait parameter declared here"),
                    );
                }
            }
            match (&required.ret, &function.ret) {
                (Some(expected), Some(actual)) => {
                    let Some(expected_type) = self
                        .declarations
                        .instantiate_trait_type(&arguments, expected)
                    else {
                        continue;
                    };
                    let expected_type = self.ast_ty(&expected_type);
                    let Some(actual_type) = self.declarations.normalize_type(actual) else {
                        continue;
                    };
                    let actual_type = self.ast_ty(&actual_type);
                    if !signature_types_match(&expected_type, &actual_type) {
                        self.sink.emit(
                            Diagnostic::error(format!(
                                "`{}` returns {}, but trait `{}` requires {}",
                                function.name.text,
                                self.ty_display(&actual_type),
                                contract.name.text,
                                self.ty_display(&expected_type)
                            ))
                            .with_code(codes::TYPE_MISMATCH)
                            .at(type_span(actual))
                            .label(type_span(expected), "trait result declared here"),
                        );
                    }
                }
                (None, None) => {}
                (expected, actual) => {
                    let message = if expected.is_some() {
                        format!(
                            "`{}` must return a value as required by trait `{}`",
                            function.name.text, contract.name.text
                        )
                    } else {
                        format!(
                            "`{}` must not return a value: trait `{}` declares no result",
                            function.name.text, contract.name.text
                        )
                    };
                    self.sink.emit(
                        Diagnostic::error(message)
                            .with_code(codes::TYPE_MISMATCH)
                            .at(actual.as_ref().map(type_span).unwrap_or(function.name.span))
                            .label(required.name.span, "trait method declared here"),
                    );
                }
            }
        }
    }

    fn check_trait_value_arguments(
        &mut self,
        contract: &TraitDecl,
        implementation: &ImplDecl,
        arguments: &crate::resolve::declarations::TraitArguments<'_>,
    ) -> bool {
        // Header values see the impl's explicit parameters, not trait-local names.
        let sym = implementation
            .params
            .params
            .iter()
            .filter_map(|parameter| {
                parameter
                    .bound
                    .as_ref()
                    .map(|bound| (parameter.name.text.clone(), self.ast_ty(bound)))
            })
            .collect();
        let before = self.sink.error_count();
        for parameter in &contract.params.params {
            let Some(value) = self
                .resolved
                .declared(parameter.name.span)
                .and_then(|id| arguments.value(id))
            else {
                continue;
            };
            let Some(bound) = parameter
                .bound
                .as_ref()
                .and_then(|bound| self.declarations.instantiate_trait_type(arguments, bound))
            else {
                continue;
            };
            let expected = self.ast_ty(&bound);
            let errors = self.sink.error_count();
            self.check_expr(value, &sym);
            // Recovery-only data nested in a bound is still unknown, not a mismatch.
            if self.sink.error_count() != errors || !self.is_digital_ty(&expected) {
                continue;
            }
            if self.check_struct_literal_for_ty(&expected, value, &sym)
                || self.assignable(&expected, value, &sym)
            {
                self.check_value_range(&bound, value);
                continue;
            }
            let actual = self.type_of(value, &sym);
            if actual == Ty::Error {
                continue;
            }
            self.sink.emit(
                Diagnostic::error(format!(
                    "value argument for `{}` of trait `{}` has type {}, but {} is required",
                    parameter.name.text,
                    contract.name.text,
                    self.ty_display(&actual),
                    self.ty_display(&expected),
                ))
                .with_code(codes::TYPE_MISMATCH)
                .at(expr_span(value))
                .label(parameter.name.span, "trait value parameter declared here"),
            );
        }
        self.sink.error_count() == before
    }
}

/// Contracts do not admit assignment's integer/vector or nominal-family coercions.
fn signature_types_match(expected: &Ty, actual: &Ty) -> bool {
    match (expected, actual) {
        (Ty::Error, _) | (_, Ty::Error) => true,
        (
            Ty::Array {
                elem: left,
                family: lf,
                ..
            },
            Ty::Array {
                elem: right,
                family: rf,
                ..
            },
        ) => lf == rf && signature_types_match(left, right) && compatible(expected, actual),
        (Ty::Tuple(left), Ty::Tuple(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| signature_types_match(left, right))
        }
        _ => expected == actual,
    }
}
