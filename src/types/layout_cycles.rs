//! Finite representation checks over borrowed, declaration-owned layouts.

use std::collections::VecDeque;

use super::*;

#[derive(Clone, Copy)]
struct Edge<'a> {
    target: DefId,
    field: Option<&'a str>,
    span: Span,
}

impl<'a> Checker<'a> {
    pub(super) fn check_struct_field_cycles(&mut self) {
        let mut structures: Vec<_> = self.declarations.structures().collect();
        structures.sort_by(|(left, _), (right, _)| {
            self.definition_key_str(*left)
                .cmp(&self.definition_key_str(*right))
        });

        // A parameter occupies storage only if the representation reaches it.
        // Forwarding through other generics requires a finite, monotone fixed point.
        let mut stored: HashMap<DefId, HashSet<DefId>> = HashMap::new();
        loop {
            let mut changed = false;
            for (owner, structure) in &structures {
                if structure.params.params.is_empty() {
                    continue;
                }
                let mut dependencies = Vec::new();
                for ty in structure
                    .base
                    .iter()
                    .chain(structure.fields.iter().map(|field| &field.ty))
                {
                    self.held_layout_types(ty, &stored, &mut dependencies);
                }
                for parameter in &structure.params.params {
                    let Some(id) = self.resolved.declared(parameter.name.span) else {
                        continue;
                    };
                    if dependencies.contains(&id) {
                        changed |= stored.entry(*owner).or_default().insert(id);
                    }
                }
            }
            if !changed {
                break;
            }
        }

        let mut graph: HashMap<DefId, Vec<Edge<'a>>> = HashMap::new();
        for (owner, structure) in &structures {
            for (ty, field, span) in structure
                .fields
                .iter()
                .map(|field| (&field.ty, Some(field.name.text.as_str()), field.name.span))
                .chain(
                    structure
                        .base
                        .iter()
                        .map(|base| (base, None, type_span(base))),
                )
            {
                let mut dependencies = Vec::new();
                self.held_layout_types(ty, &stored, &mut dependencies);
                graph.entry(*owner).or_default().extend(
                    dependencies
                        .into_iter()
                        .filter(|id| self.declarations.structure(*id).is_some())
                        .map(|target| Edge {
                            target,
                            field,
                            span,
                        }),
                );
            }
        }
        for (owner, _) in structures {
            // Resolution already diagnoses nominal base-only cycles.
            if self.cyclic_layout_base(owner) {
                continue;
            }
            let Some(edge) = closing_edge(owner, &graph) else {
                continue;
            };
            let Some(name) = self.definition_key(owner) else {
                continue;
            };
            let Some(through) = self.definition_key(edge.target) else {
                continue;
            };
            let path = if edge.target == owner {
                format!("`{name}` contains itself")
            } else {
                format!("`{name}` contains itself through `{through}`")
            };
            let location = match edge.field {
                Some(field) => format!("field `{field}`"),
                None => "the base representation".to_string(),
            };
            self.error_with_help(
                codes::TYPE_MISMATCH,
                edge.span,
                format!("{path}, so it has no finite layout"),
                format!(
                    "{location} would have to hold another `{name}`, without end; \
                         hardware has no indirection to break the cycle"
                ),
            );
        }
    }

    fn cyclic_layout_base(&self, mut owner: DefId) -> bool {
        let mut seen = HashSet::new();
        while seen.insert(owner) {
            let Some(base) = self
                .declarations
                .structure(owner)
                .and_then(|structure| structure.base.as_ref())
            else {
                return false;
            };
            let Some(base) = self.declarations.normalize_type(base) else {
                return false;
            };
            let mut head = base.as_ref();
            while let Type::Generic { base, .. } | Type::Indexed { base, .. } = head {
                head = base;
            }
            let Type::Path(path) = head else { return false };
            let Some(parent) = self.resolved.resolved(path.span) else {
                return false;
            };
            owner = parent;
        }
        true
    }

    /// Summarize one written representation without expanding recursive structs.
    fn held_layout_types(
        &self,
        ty: &Type,
        stored: &HashMap<DefId, HashSet<DefId>>,
        out: &mut Vec<DefId>,
    ) {
        let Some(ty) = self.declarations.normalize_type(ty) else {
            return;
        };
        match ty.as_ref() {
            Type::Path(path) => {
                if let Some(id) = self.resolved.resolved(path.span) {
                    if self.declarations.structure(id).is_some()
                        || self.resolved.kind_of(id) == Some(DefKind::Param)
                    {
                        out.push(id);
                    }
                }
            }
            Type::Indexed { base, .. } => self.held_layout_types(base, stored, out),
            Type::View { target, .. } => self.held_layout_types(target, stored, out),
            Type::Tuple { elems, .. } => {
                for elem in elems {
                    self.held_layout_types(elem, stored, out);
                }
            }
            Type::Generic { base, args, .. } => {
                let Some(owner) = self.declarations.scalar_type_id(base) else {
                    return;
                };
                let Some(structure) = self.declarations.structure(owner) else {
                    return;
                };
                out.push(owner);
                let Some(parameters) = stored.get(&owner) else {
                    return;
                };
                let Ok(arguments) = self.declarations.arguments(&structure.params, args) else {
                    return;
                };
                for parameter in &structure.params.params {
                    let Some(id) = self.resolved.declared(parameter.name.span) else {
                        continue;
                    };
                    if parameters.contains(&id) {
                        if let Some(ty) = arguments.type_argument(id) {
                            self.held_layout_types(ty, stored, out);
                        }
                    }
                }
            }
        }
    }
}

/// Breadth-first traversal retains the shortest cycle's first source edge.
fn closing_edge<'a>(start: DefId, graph: &HashMap<DefId, Vec<Edge<'a>>>) -> Option<Edge<'a>> {
    let mut seen = HashSet::new();
    let mut queue: VecDeque<_> = graph
        .get(&start)?
        .iter()
        .map(|edge| (edge.target, *edge))
        .collect();
    while let Some((current, first)) = queue.pop_front() {
        if current == start {
            return Some(first);
        }
        if !seen.insert(current) {
            continue;
        }
        if let Some(edges) = graph.get(&current) {
            queue.extend(edges.iter().map(|edge| (edge.target, first)));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::FileId;

    fn diagnostics(sources: &[&str]) -> Vec<Diagnostic> {
        let mut sink = DiagnosticSink::default();
        let modules: Vec<_> = sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                crate::syntax::parse_module(FileId(index as u32), source, &mut sink)
            })
            .collect();
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        assert!(!sink.has_errors(), "{:?}", sink.diagnostics());
        let mut checker = Checker::new(&mut sink, &resolved, &modules);
        checker.collect(&modules);
        checker.check_struct_field_cycles();
        drop(checker);
        sink.diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.severity == crate::diag::Severity::Error)
            .cloned()
            .collect()
    }

    #[test]
    fn finite_generic_layouts_do_not_store_unused_arguments_or_repeat_owner_cycles() {
        let sources = [
            "module model;
             pub struct Ignore<T> { pub value: integer }
             pub struct Store<T> { pub value: T }
             pub struct Relay<T> { pub value: Ignore<T> }
             pub struct Select<Unused, Stored> { pub value: Stored }
             pub struct RecursiveArgument<T> { pub ignored: Ignore<RecursiveArgument<T>> }
             pub struct Finite {
                 pub nested: Store<Store<integer>>,
                 pub phantom: Ignore<Finite>,
                 pub forwarded: Relay<Finite>,
                 pub selected: Select<Finite, integer>,
                 pub named: Select<Stored = integer, Unused = Finite>,
                 pub indirect: RecursiveArgument<Finite>
             }",
            "module same; pub struct Finite { pub value: model::Finite }",
        ];
        for sources in [sources, [sources[1], sources[0]]] {
            assert!(diagnostics(&sources).is_empty());
        }
    }

    #[test]
    fn generic_layout_cycles_follow_only_stored_parameter_contracts() {
        for representation in [
            "Store<Node>",
            "Store<Store<Node>>",
            "Relay<Node>",
            "Store<T = Node>",
            "Alias<Node>",
            "Select<integer, Node>",
            "Select<Stored = Node, Unused = integer>",
            "(integer, Node[2])",
        ] {
            let source = format!(
                "module model;
                struct Store<T> {{ value: T }}
                struct Relay<T> {{ value: Store<T> }}
                struct Select<Unused, Stored> {{ value: Stored }}
                type Alias<T> = Relay<T>;
                struct Node {{ next: {representation} }}"
            );
            let errors = diagnostics(&[&source]);
            assert_eq!(errors.len(), 1, "{representation}: {errors:?}");
            assert_eq!(errors[0].code, Some(codes::TYPE_MISMATCH));
            assert_eq!(
                errors[0].message,
                "`model::Node` contains itself, so it has no finite layout"
            );
            let span = errors[0].primary.unwrap();
            assert_eq!(&source[span.start as usize..span.end as usize], "next");
        }
    }

    #[test]
    fn borrowed_layout_cycles_preserve_nominal_scope_and_cross_base_storage() {
        let mut sources = [
            "module left; pub struct Node { pub next: right::Node }",
            "module right; pub struct Node { pub next: left::Node }",
            "module other; pub struct Node { pub value: integer }",
            "module base;
             pub struct Base { pub next: Derived }
             pub struct Derived(Base);",
        ];
        for _ in 0..2 {
            let errors = diagnostics(&sources);
            assert_eq!(errors.len(), 4, "{errors:?}");
            assert!(errors[0]
                .message
                .contains("`base::Base` contains itself through `base::Derived`"));
            assert!(errors[1]
                .message
                .contains("`base::Derived` contains itself through `base::Base`"));
            assert!(errors[1]
                .help
                .as_ref()
                .unwrap()
                .starts_with("the base representation"));
            assert!(errors[2]
                .message
                .contains("`left::Node` contains itself through `right::Node`"));
            assert!(errors[3]
                .message
                .contains("`right::Node` contains itself through `left::Node`"));
            sources.reverse();
        }
    }

    #[test]
    fn generic_base_arguments_can_store_the_owner_but_invalid_base_cycles_do_not_cascade() {
        let errors = diagnostics(&["module model;
            struct Store<T> { value: T }
            struct Node(Store<Node>);"]);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].message.contains("`model::Node` contains itself"));
        assert!(errors[0]
            .help
            .as_ref()
            .unwrap()
            .starts_with("the base representation"));

        let mut sink = DiagnosticSink::default();
        let modules = vec![crate::syntax::parse_module(
            FileId(0),
            "module invalid; struct Left(Right); struct Right(Left);",
            &mut sink,
        )];
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let errors = sink.error_count();
        assert_eq!(errors, 2, "{:?}", sink.diagnostics());
        let mut checker = Checker::new(&mut sink, &resolved, &modules);
        checker.collect(&modules);
        checker.check_struct_field_cycles();
        drop(checker);
        assert_eq!(
            sink.error_count(),
            errors,
            "do not duplicate resolver errors"
        );
    }
}
