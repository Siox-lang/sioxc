//! Declaration-aware layout validation; resolution checks names and arity only.

use super::*;
use crate::resolve::declarations::ArgumentError;

struct LayoutIssue {
    span: Span,
    message: String,
    declaration: Option<Span>,
    width: Option<i128>,
}

impl<'a> Checker<'a> {
    pub(super) fn check_source_layouts(&mut self, modules: &[Module]) {
        crate::syntax::visit::read_roots::modules(
            modules,
            &mut |ty| self.check_type_layout(ty),
            &mut |_| {},
        );
    }

    fn check_type_layout(&mut self, ty: &Type) {
        let mut issues = Vec::new();
        self.collect_layout_issues(ty, &mut HashSet::new(), &mut issues);
        for issue in issues {
            let mut diagnostic = Diagnostic::error(issue.message)
                .with_code(codes::TYPE_MISMATCH)
                .at(issue.span);
            if let Some(declaration) = issue.declaration {
                diagnostic = diagnostic.label(declaration, "layout declared here");
            }
            self.sink.emit(diagnostic);
        }
    }

    fn layout_width_problem(&self, index: &Expr) -> Option<String> {
        if let Expr::Range { lo, hi, .. } = index {
            let (Some(left), Some(right)) = (self.const_int(lo), self.const_int(hi)) else {
                return None;
            };
            let Some(length) = left.abs_diff(right).checked_add(1) else {
                return Some(format!(
                    "range exceeds the compiler layout maximum of {}",
                    u32::MAX
                ));
            };
            return (length > u128::from(u32::MAX)).then(|| {
                format!(
                    "range contains {length} elements, exceeding the compiler layout maximum of {}",
                    u32::MAX
                )
            });
        }
        match self.const_int(index) {
            Some(width) if width < 0 => Some("a type width cannot be negative".to_string()),
            Some(0) => Some("a type width must be at least 1".to_string()),
            Some(width) if u32::try_from(width).is_err() => Some(format!(
                "type width {width} exceeds the compiler layout maximum of {}",
                u32::MAX
            )),
            None => {
                // A literal outside the analysis domain still exceeds a layout.
                if let Expr::Int { text, .. } = index {
                    if !text.contains('.') {
                        return Some(format!(
                            "type width {text} exceeds the compiler layout maximum of {}",
                            u32::MAX
                        ));
                    }
                }
                None
            }
            _ => None,
        }
    }

    fn collect_layout_issues(
        &self,
        ty: &Type,
        active: &mut HashSet<DefId>,
        issues: &mut Vec<LayoutIssue>,
    ) {
        match ty {
            Type::Path(_) => {}
            Type::Indexed { base, index, .. } => {
                self.collect_layout_issues(base, active, issues);
                if let Some(index) = index.as_deref() {
                    if let Some(message) = self.layout_width_problem(index) {
                        issues.push(LayoutIssue {
                            span: expr_span(index),
                            message,
                            declaration: None,
                            width: self.const_int(index),
                        });
                    }
                }
            }
            Type::Generic { base, args, span } => {
                self.collect_layout_issues(base, active, issues);
                let owner = match base.as_ref() {
                    Type::Path(path) => self.resolved.resolved(path.span),
                    _ => None,
                };
                let parameters = owner.and_then(|id| self.declarations.layout_params(id));
                if owner.is_some_and(|id| self.resolved.kind_of(id) == Some(DefKind::TypeAlias))
                    && !self.declarations.valid_alias_arguments(ty)
                {
                    return; // Alias normalization has already diagnosed this application.
                }
                if let (Some(owner), Some(parameters)) = (owner, parameters) {
                    // Arity already has a resolver diagnostic; do not cascade.
                    if args.len() != parameters.params.len() {
                        return;
                    }
                    self.collect_applied_layout(owner, parameters, args, *span, active, issues);
                } else {
                    for argument in args {
                        if let Some(ty) = argument.type_ref() {
                            self.collect_layout_issues(ty, active, issues);
                        }
                    }
                    // Alias targets retain their source identities and width expressions.
                    if let Some(std::borrow::Cow::Owned(normalized)) =
                        self.declarations.normalize_type(ty)
                    {
                        self.collect_layout_issues(&normalized, active, issues);
                    }
                }
            }
            Type::View { target, .. } => self.collect_layout_issues(target, active, issues),
            Type::Tuple { elems, .. } => {
                for elem in elems {
                    self.collect_layout_issues(elem, active, issues);
                }
            }
        }
    }

    fn collect_applied_layout(
        &self,
        owner: DefId,
        parameters: &Params,
        args: &[GenericArg],
        span: Span,
        active: &mut HashSet<DefId>,
        issues: &mut Vec<LayoutIssue>,
    ) {
        let name = self
            .resolved
            .def(owner)
            .map(|definition| definition.name.as_str())
            .unwrap_or("<type>");
        let arguments = match self.declarations.arguments(parameters, args) {
            Ok(arguments) => arguments,
            Err(error) => {
                let (message, span, parameter) = match error {
                    ArgumentError::UnknownName {
                        name: argument,
                        span,
                    } => (
                        format!("`{name}` has no template parameter named `{argument}`"),
                        span,
                        None,
                    ),
                    ArgumentError::Duplicate { parameter, span } => (
                        format!(
                            "template parameter `{}` of `{name}` is supplied more than once",
                            parameters.params[parameter].name.text
                        ),
                        span,
                        Some(parameter),
                    ),
                    ArgumentError::ExpectedType { parameter, span } => (
                        format!(
                            "template parameter `{}` of `{name}` requires a type argument",
                            parameters.params[parameter].name.text
                        ),
                        span,
                        Some(parameter),
                    ),
                    ArgumentError::ExpectedValue { parameter, span } => (
                        format!(
                            "template parameter `{}` of `{name}` requires a value argument",
                            parameters.params[parameter].name.text
                        ),
                        span,
                        Some(parameter),
                    ),
                    ArgumentError::Count { .. } | ArgumentError::Unresolved => return,
                };
                issues.push(LayoutIssue {
                    span,
                    message,
                    declaration: parameter.map(|position| parameters.params[position].name.span),
                    width: None,
                });
                return;
            }
        };
        // Argument order follows declarations, not the binding HashMap's iteration order.
        let before = issues.len();
        for parameter in &parameters.params {
            if let Some(ty) = self
                .resolved
                .declared(parameter.name.span)
                .and_then(|id| arguments.type_argument(id))
            {
                self.collect_layout_issues(ty, active, issues);
            }
        }
        if issues.len() != before {
            return;
        }
        // Check syntactically nested arguments before guarding declaration expansion.
        if !active.insert(owner) {
            return;
        }
        for source in self.declarations.layout_types(owner) {
            // Invalid declarations are diagnosed at their definition, not at every use.
            self.collect_layout_issues(source, active, issues);
            let invalid_declaration = issues.len() != before;
            issues.truncate(before);
            if invalid_declaration {
                continue;
            }
            let Some(instantiated) = self.declarations.instantiate_type(&arguments, source) else {
                continue;
            };
            self.collect_layout_issues(&instantiated, active, issues);
            if issues.len() != before {
                // Keep one application diagnostic, at the arguments that made it invalid.
                issues.truncate(before + 1);
                let issue = &mut issues[before];
                issue.declaration.get_or_insert(type_span(source));
                issue.span = span;
                issue.message = match issue.width {
                    Some(width) if width < 1 => format!(
                        "these arguments give `{name}` a width of {width}; a width must be at least 1"
                    ),
                    _ => format!("these arguments give `{name}` an invalid layout: {}", issue.message),
                };
                break;
            }
        }
        active.remove(&owner);
    }
}
