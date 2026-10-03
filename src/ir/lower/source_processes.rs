//! Typed source lowering into the canonical process IR.
//!
//! Process/CFG types, validation, test descriptors, and ownership live in
//! [`crate::ir::Design`]. Test stimulus enters from typed Siox AST;
//! hardware enters through the elaborated, normalized digital scheduler graph
//! so generic/generate/std semantics are not repeated. The remaining Phase 1
//! inversion derives those optimized digital forms from this canonical product.

use crate::elab::Hierarchy;
use crate::ir::{
    arena_constant_integer, integer_words_width, shifted_arena_width, Design, LayoutDirection,
    LayoutKind, ProcessActivation, ProcessAggregateField, ProcessAssignment, ProcessBinaryOp,
    ProcessBlock, ProcessBlockId, ProcessCfg, ProcessDisplayKind, ProcessFormatPart,
    ProcessHostValueOp, ProcessId, ProcessInstruction, ProcessIr, ProcessLocal, ProcessLocalId,
    ProcessMatchArm, ProcessNumber, ProcessPattern, ProcessRuntimeOp, ProcessSensitivity,
    ProcessSignalState, ProcessStorage, ProcessStorageBinding, ProcessStorageId, ProcessSuspendOp,
    ProcessTerminator, ProcessTest, ProcessUnaryOp, ProcessValue, ProcessValueId, ProcessValueKind,
    ProcessValueMatchArm, SignalId,
};
use crate::resolve::Resolved;
use crate::syntax::ast::{self, ElseBranch, ImplItem, Stmt};
use crate::syntax::Module;
use crate::testbench::TestPlan;
use crate::types::Typed;

#[derive(Clone)]
struct ConstantSuffix {
    target: String,
    parameter: String,
    domain: SuffixDomain,
    body: ast::Block,
}

#[derive(Clone, Copy)]
enum SuffixDomain {
    Integer,
    Real,
}

struct LoweringContext<'a> {
    modules: &'a [Module],
    resolved: &'a Resolved,
    typed: &'a Typed,
    design: &'a Design,
    hierarchy: &'a Hierarchy,
    root_path: &'a str,
    process_ir: &'a mut ProcessIr,
    suffixes: &'a std::collections::HashMap<String, Vec<ConstantSuffix>>,
    constants: &'a std::collections::HashMap<crate::resolve::DefId, &'a ast::Expr>,
    constant_stack: std::collections::HashSet<crate::resolve::DefId>,
    functions: &'a crate::ir::FunctionIndex<'a>,
    constant_integers: &'a std::collections::HashMap<String, i64>,
    value_bindings: Vec<std::collections::HashMap<crate::resolve::DefId, ProcessValueId>>,
    inline_self_values: Vec<Option<ProcessValueId>>,
    inline_return_types: Vec<Option<crate::types::Ty>>,
    inline_functions: std::collections::HashSet<crate::diag::Span>,
}

/// Source constants indexed by resolver identity. Module and impl-scoped
/// declarations use the same arena aliasing rule; keeping both here also lets
/// two test entities declare the same leaf name without colliding.
fn source_constants<'a>(
    modules: &'a [Module],
    resolved: &Resolved,
) -> std::collections::HashMap<crate::resolve::DefId, &'a ast::Expr> {
    let mut constants = std::collections::HashMap::new();
    for item in modules.iter().flat_map(|module| &module.items) {
        match item {
            ast::Item::Const(constant) => {
                if let Some(definition) = resolved.declared(constant.name.span) {
                    constants.insert(definition, &constant.value);
                }
            }
            ast::Item::Impl(implementation) => {
                for constant in implementation.items.iter().filter_map(|item| match item {
                    ImplItem::Const(constant) => Some(constant),
                    _ => None,
                }) {
                    if let Some(definition) = resolved.declared(constant.name.span) {
                        constants.insert(definition, &constant.value);
                    }
                }
            }
            _ => {}
        }
    }
    constants
}

/// Functions whose bodies may be evaluated while their arguments are constant
/// or inlined symbolically into a caller's Process value graph.
fn process_functions<'a>(
    modules: &'a [Module],
    resolved: &'a Resolved,
) -> crate::ir::FunctionIndex<'a> {
    let mut functions = crate::ir::FunctionIndex::new(resolved);
    let trait_declarations = modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            ast::Item::Trait(declaration) => {
                Some((functions.trait_decl_key(&declaration.name), declaration))
            }
            _ => None,
        })
        .collect::<std::collections::HashMap<_, _>>();
    for item in modules.iter().flat_map(|module| &module.items) {
        match item {
            ast::Item::Fn(function) => functions.insert_free(function),
            ast::Item::ExternBlock { fns, .. } => {
                for function in fns {
                    functions.insert_free(function);
                }
            }
            _ => {}
        }
    }
    for implementation in modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            ast::Item::Impl(implementation) => Some(implementation),
            _ => None,
        })
    {
        functions.insert_operator_impl(implementation);
        functions.insert_conversion_impl(implementation);
        let Some(owner) = functions.type_head_key(&implementation.target) else {
            continue;
        };
        for item in &implementation.items {
            if let ast::ImplItem::Fn(function) = item {
                functions.insert_associated(format!("{owner}::{}", function.name.text), function);
            }
        }
    }
    let inherited = modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            ast::Item::Impl(implementation) => Some(implementation),
            _ => None,
        })
        .filter_map(|implementation| {
            let owner = functions.type_head_key(&implementation.target)?;
            let trait_key = functions.trait_path_key(implementation.trait_.as_ref()?)?;
            let declaration = *trait_declarations.get(&trait_key)?;
            if matches!(trait_key.as_str(), "Eq" | "Ord") {
                for function in declaration.items.iter().filter(|f| f.body.is_some()) {
                    functions.insert_comparison_default(implementation, function);
                }
            }
            Some((owner, declaration))
        })
        .flat_map(|(owner, declaration)| {
            declaration
                .items
                .iter()
                .filter(|function| function.body.is_some())
                .map(move |function| (owner.clone(), function))
        })
        .collect::<Vec<_>>();
    for (owner, function) in inherited {
        functions.insert_associated_default(format!("{owner}::{}", function.name.text), function);
    }
    functions
}

/// Fold module integer constants to seed const-evaluable function calls. The
/// fixed point makes declaration order irrelevant and stops naturally when a
/// rejected cycle or non-integer constant cannot make progress.
fn module_constant_integers(
    modules: &[Module],
    functions: &crate::ir::FunctionIndex<'_>,
) -> std::collections::HashMap<String, i64> {
    let constants = modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            ast::Item::Const(constant) => Some(constant),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut values = std::collections::HashMap::new();
    loop {
        let previous = values.len();
        for constant in &constants {
            let key = functions.constant_decl_key(constant);
            if values.contains_key(&key) {
                continue;
            }
            if let Some(value) = crate::ir::eval_const_fns(&constant.value, &values, functions, 0) {
                values.insert(key, value);
            }
        }
        if values.len() == previous {
            return values;
        }
    }
}

fn type_leaf(ty: &ast::Type) -> Option<&str> {
    match ty {
        ast::Type::Path(path) => path.segments.last().map(|segment| segment.text.as_str()),
        ast::Type::Generic { base, .. } | ast::Type::Indexed { base, .. } => type_leaf(base),
        ast::Type::View { target, .. } => type_leaf(target),
    }
}

/// Nominal type supplied by a declaration when expression-type persistence is
/// intentionally incomplete for constructor syntax. The declaration is the
/// authoritative context for a `let`; retaining it prevents the temporary
/// adapter from replacing a successfully checked newtype with `Ty::Error`.
fn declared_nominal_type(ty: Option<&ast::Type>, resolved: &Resolved) -> Option<crate::types::Ty> {
    let ast::Type::Path(path) = ty? else {
        return None;
    };
    let definition = resolved.resolved(path.span)?;
    matches!(
        resolved.def(definition)?.kind,
        crate::resolve::DefKind::Struct | crate::resolve::DefKind::Enum
    )
    .then_some(crate::types::Ty::Named(definition))
}

/// Recover the concrete scalar/nominal portion of a function signature when
/// the expression type table intentionally has no entry for an imported call.
/// Indexed/generic substitution remains the type checker's job; this fallback
/// exists so an inlined result retains an explicit `integer`, `real`, `Char`,
/// enum, or struct identity in backend-independent Process IR.
fn declared_process_type(ty: &ast::Type, resolved: &Resolved) -> Option<crate::types::Ty> {
    let ast::Type::Path(path) = ty else {
        return None;
    };
    match path.segments.last()?.text.as_str() {
        "integer" => Some(crate::types::Ty::Integer),
        "real" => Some(crate::types::Ty::Real),
        "Char" => Some(crate::types::Ty::Char),
        _ => {
            let definition = resolved.resolved(path.span)?;
            matches!(
                resolved.def(definition)?.kind,
                crate::resolve::DefKind::Struct | crate::resolve::DefKind::Enum
            )
            .then_some(crate::types::Ty::Named(definition))
        }
    }
}

/// Recover a concrete checked type from a function/local declaration while
/// source Process lowering still has access to source syntax. Stage 4 does
/// not persist a type for every contextual aggregate literal, so call
/// arguments such as `[1, 2]` and `{ .a = 1 }` must inherit the signature's
/// recursive shape before that signature disappears from Process IR.
fn process_declared_type(
    ty: &ast::Type,
    context: &LoweringContext<'_>,
) -> Option<crate::types::Ty> {
    match ty {
        ast::Type::Path(path) => {
            let name = context
                .resolved
                .resolved(path.span)
                .and_then(|definition| context.resolved.qualified_name(definition))
                .unwrap_or_else(|| path_name(path));
            let exact = context
                .design
                .array_element_of_family
                .keys()
                .find(|family| family.as_str() == name)
                .cloned();
            let family = exact.or_else(|| {
                let leaf = name.rsplit("::").next()?;
                let mut candidates = context
                    .design
                    .array_element_of_family
                    .keys()
                    .filter(|family| family.rsplit("::").next() == Some(leaf));
                let first = candidates.next()?.clone();
                candidates.next().is_none().then_some(first)
            });
            family.map(|family| {
                let element = context
                    .design
                    .array_element_of_family
                    .get(&family)
                    .and_then(|element| nominal_type_from_name(element, context.resolved))
                    .unwrap_or(crate::types::Ty::Error);
                crate::types::Ty::Array {
                    elem: Box::new(element),
                    len: 0,
                    family: Some(family),
                }
            })
        }
        .or_else(|| declared_process_type(ty, context.resolved)),
        ast::Type::Indexed { base, index, .. } => {
            let base = process_declared_type(base, context)?;
            let length = match index.as_deref() {
                None => 0,
                Some(ast::Expr::Range { lo, hi, .. }) => {
                    let left = crate::ir::eval_const_fns(
                        lo,
                        context.constant_integers,
                        context.functions,
                        0,
                    )?;
                    let right = crate::ir::eval_const_fns(
                        hi,
                        context.constant_integers,
                        context.functions,
                        0,
                    )?;
                    u32::try_from(left.abs_diff(right).checked_add(1)?).ok()?
                }
                Some(index) => u32::try_from(crate::ir::eval_const_fns(
                    index,
                    context.constant_integers,
                    context.functions,
                    0,
                )?)
                .ok()?,
            };
            match base {
                crate::types::Ty::Array {
                    elem,
                    len: 0,
                    family: Some(family),
                } => Some(crate::types::Ty::Array {
                    elem,
                    len: length,
                    family: Some(family),
                }),
                element => Some(crate::types::Ty::Array {
                    elem: Box::new(element),
                    len: length,
                    family: None,
                }),
            }
        }
        ast::Type::Generic { base, .. } => process_declared_type(base, context),
        ast::Type::View { target, .. } => process_declared_type(target, context),
    }
}

/// The condition type, `core`'s `bool` lang item.
fn bool_type(resolved: &Resolved) -> Option<crate::types::Ty> {
    resolved.lang("bool").map(crate::types::Ty::Named)
}

fn nominal_type_from_name(name: &str, resolved: &Resolved) -> Option<crate::types::Ty> {
    let type_definition = |definition: &&crate::resolve::DefInfo| {
        matches!(
            definition.kind,
            crate::resolve::DefKind::Builtin
                | crate::resolve::DefKind::Struct
                | crate::resolve::DefKind::View
                | crate::resolve::DefKind::Enum
                | crate::resolve::DefKind::Entity
                | crate::resolve::DefKind::TypeAlias
        )
    };
    let exact = resolved
        .defs()
        .iter()
        .enumerate()
        .filter(|(_, definition)| type_definition(definition))
        .find_map(|(index, _)| {
            let definition = crate::resolve::DefId(u32::try_from(index).ok()?);
            (resolved.qualified_name(definition).as_deref() == Some(name)).then_some(definition)
        });
    let definition = exact.or_else(|| {
        let leaf = name.rsplit("::").next()?;
        let mut candidates = resolved
            .defs()
            .iter()
            .enumerate()
            .filter(|(_, definition)| definition.name == leaf && type_definition(definition))
            .filter_map(|(index, _)| u32::try_from(index).ok().map(crate::resolve::DefId));
        let first = candidates.next()?;
        candidates.next().is_none().then_some(first)
    })?;
    Some(crate::types::Ty::Named(definition))
}

/// Recover the checked value shape from the declaration-owned layout. This is
/// more authoritative than an initializer expression for contextual literals:
/// `let bits: Bit[3..0] = "1010"` has a string token on the right but an array
/// of `Bit` values in storage.
fn process_type_from_layout(
    layout: &crate::ir::SourceLayout,
    resolved: &Resolved,
) -> Option<crate::types::Ty> {
    match &layout.kind {
        LayoutKind::Scalar {
            domain, nominal, ..
        } => nominal
            .as_deref()
            .and_then(|name| nominal_type_from_name(name, resolved))
            .or_else(|| match domain {
                crate::ir::ScalarDomain::Integer => Some(crate::types::Ty::Integer),
                crate::ir::ScalarDomain::Real => Some(crate::types::Ty::Real),
                crate::ir::ScalarDomain::Character => Some(crate::types::Ty::Char),
                crate::ir::ScalarDomain::Enum(name) => nominal_type_from_name(name, resolved),
                crate::ir::ScalarDomain::Bits => None,
            }),
        LayoutKind::Packed {
            width,
            family,
            range,
            element_enum,
        } => Some(crate::types::Ty::Array {
            elem: Box::new(
                element_enum
                    .as_deref()
                    .and_then(|name| nominal_type_from_name(name, resolved))
                    .unwrap_or(crate::types::Ty::Error),
            ),
            len: range
                .and_then(|range| range.len())
                .and_then(|length| u32::try_from(length).ok())
                .unwrap_or(*width),
            family: Some(family.clone()),
        }),
        LayoutKind::Array { range, element } => Some(crate::types::Ty::Array {
            elem: Box::new(process_type_from_layout(element, resolved)?),
            len: range
                .and_then(|range| range.len())
                .and_then(|length| u32::try_from(length).ok())
                .unwrap_or(0),
            family: None,
        }),
        LayoutKind::Struct { name, .. } => nominal_type_from_name(name, resolved),
        LayoutKind::Opaque { .. } => None,
    }
}

/// Apply a checked array length to an otherwise unconstrained declaration
/// layout. The type checker infers `let s: string = "hello"` as `Char[5]`;
/// retaining `Char[]` here would discard that fixed native storage shape after
/// semantic analysis had already established it.
fn process_layout_with_type(
    layout: &crate::ir::SourceLayout,
    ty: Option<&crate::types::Ty>,
) -> crate::ir::SourceLayout {
    let mut concrete = layout.clone();
    if let (LayoutKind::Array { range, element }, Some(crate::types::Ty::Array { elem, len, .. })) =
        (&mut concrete.kind, ty)
    {
        if range.is_none() && *len != 0 {
            *range = Some(crate::ir::LayoutRange {
                left: 0,
                right: i64::from(*len) - 1,
            });
        }
        **element = process_layout_with_type(element, Some(elem));
    }
    concrete
}

/// Compare source layouts as representations rather than diagnostic anchors.
/// Equal types commonly acquire different use-site spans; those spans must not
/// make an otherwise unique aggregate shape look ambiguous.
fn process_layout_same_shape(
    left: &crate::ir::SourceLayout,
    right: &crate::ir::SourceLayout,
) -> bool {
    match (&left.kind, &right.kind) {
        (
            LayoutKind::Scalar {
                width: left_width,
                domain: left_domain,
                nominal: left_nominal,
                value_range: left_range,
            },
            LayoutKind::Scalar {
                width: right_width,
                domain: right_domain,
                nominal: right_nominal,
                value_range: right_range,
            },
        ) => {
            left_width == right_width
                && left_domain == right_domain
                && left_nominal == right_nominal
                && left_range == right_range
        }
        (
            LayoutKind::Packed {
                width: left_width,
                family: left_family,
                range: left_range,
                element_enum: left_element,
            },
            LayoutKind::Packed {
                width: right_width,
                family: right_family,
                range: right_range,
                element_enum: right_element,
            },
        ) => {
            left_width == right_width
                && left_family == right_family
                && left_range == right_range
                && left_element == right_element
        }
        (
            LayoutKind::Array {
                range: left_range,
                element: left_element,
            },
            LayoutKind::Array {
                range: right_range,
                element: right_element,
            },
        ) => left_range == right_range && process_layout_same_shape(left_element, right_element),
        (
            LayoutKind::Struct {
                name: left_name,
                view: left_view,
                fields: left_fields,
            },
            LayoutKind::Struct {
                name: right_name,
                view: right_view,
                fields: right_fields,
            },
        ) => {
            left_name == right_name
                && left_view == right_view
                && left_fields.len() == right_fields.len()
                && left_fields.iter().zip(right_fields).all(|(left, right)| {
                    left.name == right.name
                        && left.direction == right.direction
                        && process_layout_same_shape(&left.layout, &right.layout)
                })
        }
        (
            LayoutKind::Opaque {
                name: left_name,
                width: left_width,
            },
            LayoutKind::Opaque {
                name: right_name,
                width: right_width,
            },
        ) => left_name == right_name && left_width == right_width,
        _ => false,
    }
}

/// Build or recover the canonical recursive representation of a checked type.
/// Arrays and packed families are fully described by `Ty`; named structs reuse
/// an already elaborated declaration layout after proving all candidates share
/// one representation.
fn process_layout_for_type(
    ty: &crate::types::Ty,
    span: crate::diag::Span,
    context: &LoweringContext<'_>,
) -> Option<crate::ir::SourceLayout> {
    let kind = match ty {
        crate::types::Ty::Integer => LayoutKind::Scalar {
            width: 64,
            domain: crate::ir::ScalarDomain::Integer,
            nominal: None,
            value_range: None,
        },
        crate::types::Ty::Real => LayoutKind::Scalar {
            width: 64,
            domain: crate::ir::ScalarDomain::Real,
            nominal: None,
            value_range: None,
        },
        crate::types::Ty::Char => LayoutKind::Scalar {
            width: 32,
            domain: crate::ir::ScalarDomain::Character,
            nominal: None,
            value_range: None,
        },
        crate::types::Ty::Array {
            elem: _,
            len,
            family: Some(family),
        } => {
            let element_enum = context
                .design
                .array_element_of_family
                .get(family)
                .or_else(|| {
                    family
                        .rsplit("::")
                        .next()
                        .and_then(|leaf| context.design.array_element_of_family.get(leaf))
                })
                .cloned();
            LayoutKind::Packed {
                width: *len,
                family: family.clone(),
                range: (*len != 0).then(|| crate::ir::LayoutRange {
                    left: 0,
                    right: i64::from(*len) - 1,
                }),
                element_enum,
            }
        }
        crate::types::Ty::Array {
            elem,
            len,
            family: None,
        } => LayoutKind::Array {
            range: (*len != 0).then(|| crate::ir::LayoutRange {
                left: 0,
                right: i64::from(*len) - 1,
            }),
            element: Box::new(process_layout_for_type(elem, span, context)?),
        },
        crate::types::Ty::Named(definition) => {
            let info = context.resolved.def(*definition)?;
            if info.kind == crate::resolve::DefKind::Enum {
                let qualified = context.resolved.qualified_name(*definition);
                let key = qualified
                    .filter(|name| context.design.enum_syms.contains_key(name))
                    .or_else(|| {
                        context
                            .design
                            .enum_syms
                            .contains_key(&info.name)
                            .then(|| info.name.clone())
                    })?;
                let highest = context
                    .design
                    .enum_syms
                    .get(&key)?
                    .keys()
                    .copied()
                    .max()
                    .unwrap_or(0);
                LayoutKind::Scalar {
                    width: (u64::BITS - highest.leading_zeros()).max(1),
                    domain: crate::ir::ScalarDomain::Enum(key.clone()),
                    nominal: Some(key),
                    value_range: None,
                }
            } else if info.kind == crate::resolve::DefKind::Struct {
                let mut candidates = context
                    .process_ir
                    .storages
                    .iter()
                    .filter(|storage| storage.ty.as_ref() == Some(ty))
                    .filter_map(|storage| storage.layout.as_ref())
                    .chain(context.design.source_layouts.values().filter(|layout| {
                        process_type_from_layout(layout, context.resolved).as_ref() == Some(ty)
                    }));
                let first = candidates.next()?.clone();
                return candidates
                    .all(|candidate| process_layout_same_shape(&first, candidate))
                    .then_some(first);
            } else {
                return None;
            }
        }
        crate::types::Ty::Void | crate::types::Ty::Error => return None,
    };
    Some(crate::ir::SourceLayout { span, kind })
}

fn process_aggregate_layout_for_type(
    ty: &crate::types::Ty,
    span: crate::diag::Span,
    context: &LoweringContext<'_>,
) -> Option<crate::ir::SourceLayout> {
    let aggregate = matches!(ty, crate::types::Ty::Array { .. })
        || matches!(ty, crate::types::Ty::Named(definition)
            if context.resolved.kind_of(*definition) == Some(crate::resolve::DefKind::Struct));
    aggregate.then(|| process_layout_for_type(ty, span, context))?
}

fn constant_suffixes(
    modules: &[Module],
    resolved: &Resolved,
) -> std::collections::HashMap<String, Vec<ConstantSuffix>> {
    let mut suffixes = std::collections::HashMap::<String, Vec<ConstantSuffix>>::new();
    for implementation in modules.iter().flat_map(|module| &module.items) {
        let ast::Item::Impl(implementation) = implementation else {
            continue;
        };
        let Some(trait_path) = &implementation.trait_ else {
            continue;
        };
        let suffix = resolved.resolved(trait_path.span);
        if suffix.and_then(|definition| resolved.lang_of(definition)) != Some("suffix") {
            continue;
        }
        let Some(ast::GenericArg::Positional(ast::Expr::StrLit { text: symbol, .. })) =
            implementation.trait_args.first()
        else {
            continue;
        };
        let Some(target) = type_leaf(&implementation.target) else {
            continue;
        };
        for item in &implementation.items {
            let ast::ImplItem::Fn(function) = item else {
                continue;
            };
            let Some(body) = &function.body else { continue };
            let Some(parameter) = function.params.iter().find(|parameter| !parameter.is_self)
            else {
                continue;
            };
            let domain = match parameter.ty.as_ref().and_then(type_leaf) {
                Some("integer") => SuffixDomain::Integer,
                Some("real") => SuffixDomain::Real,
                _ => continue,
            };
            let Some(parameter_name) = parameter.name.as_ref() else {
                continue;
            };
            suffixes
                .entry(symbol.clone())
                .or_default()
                .push(ConstantSuffix {
                    target: target.to_string(),
                    parameter: parameter_name.text.clone(),
                    domain,
                    body: body.clone(),
                });
        }
    }
    suffixes
}

fn integer_literal_u64(text: &str) -> Option<u64> {
    let ProcessNumber::Integer(words) = parse_number(text, None) else {
        return None;
    };
    words
        .get(1..)
        .is_none_or(|rest| rest.iter().all(|word| *word == 0))
        .then(|| words.first().copied().unwrap_or(0))
}

fn eval_suffix_expr(expression: &ast::Expr, suffix: &ConstantSuffix, input: u64) -> Option<u64> {
    match expression {
        ast::Expr::Int { text, .. } => integer_literal_u64(text),
        ast::Expr::Path(path)
            if path.segments.len() == 1 && path.segments[0].text == suffix.parameter =>
        {
            Some(input)
        }
        ast::Expr::Call { callee, args, .. }
            if callee_name(callee) == suffix.target && args.len() == 1 =>
        {
            eval_suffix_expr(&args[0], suffix, input)
        }
        ast::Expr::IfExpr {
            cond, then, els, ..
        } => {
            if eval_suffix_expr(cond, suffix, input)? != 0 {
                eval_suffix_expr(then, suffix, input)
            } else {
                eval_suffix_expr(els, suffix, input)
            }
        }
        ast::Expr::Unary {
            op: ast::UnOp::Not,
            rhs,
            ..
        } => Some(u64::from(eval_suffix_expr(rhs, suffix, input)? == 0)),
        ast::Expr::Unary {
            op: ast::UnOp::Neg, ..
        } => None,
        ast::Expr::Binary { op, lhs, rhs, .. } => {
            let left = eval_suffix_expr(lhs, suffix, input)?;
            let right = eval_suffix_expr(rhs, suffix, input)?;
            match op {
                ast::BinOp::Add => left.checked_add(right),
                ast::BinOp::Sub => left.checked_sub(right),
                ast::BinOp::Mul => left.checked_mul(right),
                ast::BinOp::Div => left.checked_div(right),
                ast::BinOp::Shl => u32::try_from(right)
                    .ok()
                    .and_then(|shift| left.checked_shl(shift)),
                ast::BinOp::Shr => u32::try_from(right)
                    .ok()
                    .and_then(|shift| left.checked_shr(shift)),
                ast::BinOp::Eq => Some(u64::from(left == right)),
                ast::BinOp::Ne => Some(u64::from(left != right)),
                ast::BinOp::Lt => Some(u64::from(left < right)),
                ast::BinOp::Le => Some(u64::from(left <= right)),
                ast::BinOp::Gt => Some(u64::from(left > right)),
                ast::BinOp::Ge => Some(u64::from(left >= right)),
                ast::BinOp::And => Some(u64::from(left != 0 && right != 0)),
                ast::BinOp::Or => Some(u64::from(left != 0 || right != 0)),
                ast::BinOp::Custom { .. } => None,
            }
        }
        _ => None,
    }
}

fn eval_suffix_block(block: &ast::Block, suffix: &ConstantSuffix, input: u64) -> Option<u64> {
    for statement in &block.stmts {
        match statement {
            ast::Stmt::Return {
                value: Some(value), ..
            } => return eval_suffix_expr(value, suffix, input),
            ast::Stmt::If(branch) => {
                let selected = if eval_suffix_expr(&branch.cond, suffix, input)? != 0 {
                    Some(&branch.then)
                } else {
                    match branch.else_.as_deref() {
                        Some(ast::ElseBranch::Block(block)) => Some(block),
                        _ => None,
                    }
                };
                if let Some(value) =
                    selected.and_then(|block| eval_suffix_block(block, suffix, input))
                {
                    return Some(value);
                }
            }
            _ => return None,
        }
    }
    None
}

fn real_literal(text: &str) -> Option<f64> {
    text.replace('_', "").parse().ok()
}

fn eval_real_suffix_expr(
    expression: &ast::Expr,
    suffix: &ConstantSuffix,
    input: f64,
) -> Option<f64> {
    match expression {
        ast::Expr::Int { text, .. } => real_literal(text),
        ast::Expr::Path(path)
            if path.segments.len() == 1 && path.segments[0].text == suffix.parameter =>
        {
            Some(input)
        }
        ast::Expr::Call { callee, args, .. }
            if callee_name(callee) == suffix.target && args.len() == 1 =>
        {
            eval_real_suffix_expr(&args[0], suffix, input)
        }
        ast::Expr::IfExpr {
            cond, then, els, ..
        } => {
            if eval_real_suffix_expr(cond, suffix, input)? != 0.0 {
                eval_real_suffix_expr(then, suffix, input)
            } else {
                eval_real_suffix_expr(els, suffix, input)
            }
        }
        ast::Expr::Unary {
            op: ast::UnOp::Neg,
            rhs,
            ..
        } => Some(-eval_real_suffix_expr(rhs, suffix, input)?),
        ast::Expr::Unary {
            op: ast::UnOp::Not,
            rhs,
            ..
        } => Some(f64::from(u8::from(
            eval_real_suffix_expr(rhs, suffix, input)? == 0.0,
        ))),
        ast::Expr::Binary { op, lhs, rhs, .. } => {
            let left = eval_real_suffix_expr(lhs, suffix, input)?;
            let right = eval_real_suffix_expr(rhs, suffix, input)?;
            match op {
                ast::BinOp::Add => Some(left + right),
                ast::BinOp::Sub => Some(left - right),
                ast::BinOp::Mul => Some(left * right),
                ast::BinOp::Div => Some(left / right),
                ast::BinOp::Eq => Some(f64::from(u8::from(left == right))),
                ast::BinOp::Ne => Some(f64::from(u8::from(left != right))),
                ast::BinOp::Lt => Some(f64::from(u8::from(left < right))),
                ast::BinOp::Le => Some(f64::from(u8::from(left <= right))),
                ast::BinOp::Gt => Some(f64::from(u8::from(left > right))),
                ast::BinOp::Ge => Some(f64::from(u8::from(left >= right))),
                ast::BinOp::And => Some(f64::from(u8::from(left != 0.0 && right != 0.0))),
                ast::BinOp::Or => Some(f64::from(u8::from(left != 0.0 || right != 0.0))),
                ast::BinOp::Shl | ast::BinOp::Shr | ast::BinOp::Custom { .. } => None,
            }
        }
        _ => None,
    }
}

fn eval_real_suffix_block(block: &ast::Block, suffix: &ConstantSuffix, input: f64) -> Option<f64> {
    for statement in &block.stmts {
        match statement {
            ast::Stmt::Return {
                value: Some(value), ..
            } => return eval_real_suffix_expr(value, suffix, input),
            ast::Stmt::If(branch) => {
                let selected = if eval_real_suffix_expr(&branch.cond, suffix, input)? != 0.0 {
                    Some(&branch.then)
                } else {
                    match branch.else_.as_deref() {
                        Some(ast::ElseBranch::Block(block)) => Some(block),
                        _ => None,
                    }
                };
                if let Some(value) =
                    selected.and_then(|block| eval_real_suffix_block(block, suffix, input))
                {
                    return Some(value);
                }
            }
            _ => return None,
        }
    }
    None
}

fn normalized_suffix(
    text: &str,
    symbol: &str,
    context: &LoweringContext<'_>,
) -> Option<ProcessNumber> {
    let [suffix] = context.suffixes.get(symbol)?.as_slice() else {
        return None;
    };
    match suffix.domain {
        SuffixDomain::Integer => {
            let input = integer_literal_u64(text)?;
            Some(ProcessNumber::Integer(vec![eval_suffix_block(
                &suffix.body,
                suffix,
                input,
            )?]))
        }
        SuffixDomain::Real => {
            let input = real_literal(text)?;
            Some(ProcessNumber::Real(
                eval_real_suffix_block(&suffix.body, suffix, input)?.to_bits(),
            ))
        }
    }
}

/// Fill the canonical process product from normalized hardware plus an
/// optional native-test plan.
///
/// One explicit test process becomes one CFG. Legacy impl-scope test statements
/// remain one implicit foreground process so their existing sequential/`await`
/// behavior is preserved until the syntax is retired. Hardware scheduler units
/// become CFGs after elaboration, including for non-test compiler outputs.
pub fn lower(
    modules: &[Module],
    resolved: &Resolved,
    typed: &Typed,
    hierarchy: &Hierarchy,
    plan: Option<&TestPlan>,
    design: &mut Design,
) {
    let mut process_ir = ProcessIr::default();
    let suffixes = constant_suffixes(modules, resolved);
    let constants = source_constants(modules, resolved);
    let functions = process_functions(modules, resolved);
    let constant_integers = module_constant_integers(modules, &functions);

    for test in plan.into_iter().flat_map(|plan| &plan.tests) {
        let root_path = hierarchy.root_path(test.root);
        let items = crate::testbench::implementation_items(modules, resolved, test.entity);
        let mut test_processes = Vec::new();
        let mut legacy_items = Vec::new();

        register_test_storages(
            &items,
            modules,
            resolved,
            typed,
            hierarchy,
            test.root,
            &root_path,
            design,
            &mut process_ir,
        );

        // Legacy bare testbench statements and declarations form one
        // source-ordered implicit process. Keep a
        // declaration before the first statement as reset state, but do not
        // hoist a later initializer across an earlier statement/await. DUT
        // instance declarations are not Process storage and stay outside this
        // legacy ordering rule.
        let mut saw_legacy_statement = false;
        let mut ordered_initializers = std::collections::HashSet::new();
        for item in &items {
            match item {
                ImplItem::Stmt(statement) if !crate::testbench::is_clock_statement(statement) => {
                    saw_legacy_statement = true;
                }
                ImplItem::Let(declaration)
                    if saw_legacy_statement && declaration.value.is_some() =>
                {
                    let Some(definition) = resolved.declared(declaration.name.span) else {
                        continue;
                    };
                    if process_ir.storages.iter().any(|storage| {
                        storage.owner == test.root && storage.source == Some(definition)
                    }) {
                        ordered_initializers.insert(definition);
                    }
                }
                _ => {}
            }
        }

        // Initializers execute before any process starts. They use the same
        // value lowering with an empty lexical scope: persistent storage and
        // declarations resolve normally, while process locals cannot appear.
        let initializer_process = ProcessCfg {
            id: ProcessId(u32::MAX),
            root: test.root,
            owner: test.root,
            label: Some(format!("{root_path}::<initializers>")),
            span: test.span,
            activation: ProcessActivation::TimeZero,
            entry: ProcessBlockId(0),
            locals: Vec::new(),
            blocks: Vec::new(),
        };
        let initializers = items
            .iter()
            .filter_map(|item| match item {
                ImplItem::Let(declaration) => Some((
                    resolved.declared(declaration.name.span)?,
                    declaration.value.as_ref()?,
                )),
                _ => None,
            })
            .filter(|(definition, _)| !ordered_initializers.contains(definition))
            .collect::<Vec<_>>();
        {
            let mut context = LoweringContext {
                modules,
                resolved,
                typed,
                design,
                hierarchy,
                root_path: &root_path,
                process_ir: &mut process_ir,
                suffixes: &suffixes,
                constants: &constants,
                constant_stack: std::collections::HashSet::new(),
                functions: &functions,
                constant_integers: &constant_integers,
                value_bindings: Vec::new(),
                inline_self_values: Vec::new(),
                inline_return_types: Vec::new(),
                inline_functions: std::collections::HashSet::new(),
            };
            for (definition, initializer) in initializers {
                let Some(storage) = context
                    .process_ir
                    .storages
                    .iter()
                    .find(|storage| {
                        storage.owner == test.root && storage.source == Some(definition)
                    })
                    .map(|storage| storage.id)
                else {
                    continue;
                };
                let target = context.process_ir.storages[storage.0 as usize].ty.clone();
                let value = value_ref_with_type(
                    initializer,
                    &initializer_process,
                    &mut context,
                    target.as_ref(),
                );
                context.process_ir.storages[storage.0 as usize].initializer = Some(value);
            }
        }

        for item in &items {
            match item {
                ImplItem::Process(process) => {
                    let id = ProcessId(process_ir.processes.len() as u32);
                    let label = process
                        .label
                        .as_ref()
                        .map(|label| format!("{root_path}::{}", label.text));
                    let activation = process_activation(
                        &process.body.stmts,
                        &root_path,
                        design,
                        resolved,
                        test.root,
                        &process_ir,
                    );
                    let lowered = {
                        let mut context = LoweringContext {
                            modules,
                            resolved,
                            typed,
                            design,
                            hierarchy,
                            root_path: &root_path,
                            process_ir: &mut process_ir,
                            suffixes: &suffixes,
                            constants: &constants,
                            constant_stack: std::collections::HashSet::new(),
                            functions: &functions,
                            constant_integers: &constant_integers,
                            value_bindings: Vec::new(),
                            inline_self_values: Vec::new(),
                            inline_return_types: Vec::new(),
                            inline_functions: std::collections::HashSet::new(),
                        };
                        lower_process(
                            id,
                            test.root,
                            label,
                            process.span,
                            activation,
                            &process.body.stmts,
                            &mut context,
                        )
                    };
                    process_ir.processes.push(lowered);
                    test_processes.push(id);
                }
                ImplItem::Stmt(statement) if crate::testbench::is_clock_statement(statement) => {
                    // Legacy impl-scope syntax still denotes a concurrent
                    // clock process. Keeping it in the foreground statement
                    // list would make Process IR lose the scheduling boundary.
                    // Give it an ordinary reactive CFG now.
                    let id = ProcessId(process_ir.processes.len() as u32);
                    let statements = std::slice::from_ref(statement);
                    let activation = process_activation(
                        statements,
                        &root_path,
                        design,
                        resolved,
                        test.root,
                        &process_ir,
                    );
                    let lowered = {
                        let mut context = LoweringContext {
                            modules,
                            resolved,
                            typed,
                            design,
                            hierarchy,
                            root_path: &root_path,
                            process_ir: &mut process_ir,
                            suffixes: &suffixes,
                            constants: &constants,
                            constant_stack: std::collections::HashSet::new(),
                            functions: &functions,
                            constant_integers: &constant_integers,
                            value_bindings: Vec::new(),
                            inline_self_values: Vec::new(),
                            inline_return_types: Vec::new(),
                            inline_functions: std::collections::HashSet::new(),
                        };
                        lower_process(
                            id,
                            test.root,
                            Some(format!("{root_path}::<clock:{}>", id.0)),
                            ast::stmt_span(statement),
                            activation,
                            statements,
                            &mut context,
                        )
                    };
                    process_ir.processes.push(lowered);
                    test_processes.push(id);
                }
                ImplItem::Stmt(_) => legacy_items.push(*item),
                ImplItem::Let(declaration)
                    if instance_has_runtime_connections(declaration, resolved) =>
                {
                    legacy_items.push(*item);
                }
                ImplItem::Let(declaration)
                    if resolved
                        .declared(declaration.name.span)
                        .is_some_and(|definition| ordered_initializers.contains(&definition)) =>
                {
                    legacy_items.push(*item);
                }
                ImplItem::Const(_)
                | ImplItem::Fn(_)
                | ImplItem::ModeField { .. }
                | ImplItem::Let(_)
                | ImplItem::AttrBinding(_) => {}
            }
        }

        if !legacy_items.is_empty() {
            let id = ProcessId(process_ir.processes.len() as u32);
            let span = legacy_items
                .first()
                .map(|item| match item {
                    ImplItem::Stmt(statement) => ast::stmt_span(statement),
                    ImplItem::Let(declaration) => declaration.span,
                    _ => test.span,
                })
                .unwrap_or(test.span);
            let lowered = {
                let mut context = LoweringContext {
                    modules,
                    resolved,
                    typed,
                    design,
                    hierarchy,
                    root_path: &root_path,
                    process_ir: &mut process_ir,
                    suffixes: &suffixes,
                    constants: &constants,
                    constant_stack: std::collections::HashSet::new(),
                    functions: &functions,
                    constant_integers: &constant_integers,
                    value_bindings: Vec::new(),
                    inline_self_values: Vec::new(),
                    inline_return_types: Vec::new(),
                    inline_functions: std::collections::HashSet::new(),
                };
                lower_legacy_process(
                    id,
                    test.root,
                    Some(format!("{root_path}::<legacy>")),
                    span,
                    ProcessActivation::TimeZero,
                    &legacy_items,
                    &mut context,
                )
            };
            process_ir.processes.push(lowered);
            test_processes.push(id);
        }

        process_ir.tests.push(ProcessTest {
            entity: test.entity,
            root: test.root,
            qualified_name: test.qualified_name.clone(),
            span: test.span,
            processes: test_processes,
        });
    }

    crate::ir::import_hardware_processes(hierarchy, design, &mut process_ir);

    design.process_ir = process_ir;
}

/// Register persistent state declared by one test root and connect each
/// flattened storage projection to the DUT port leaves elaboration produced.
#[allow(clippy::too_many_arguments)]
fn register_test_storages(
    items: &[&ImplItem],
    modules: &[Module],
    resolved: &Resolved,
    typed: &Typed,
    hierarchy: &Hierarchy,
    root: crate::elab::InstanceId,
    root_path: &str,
    design: &Design,
    process_ir: &mut ProcessIr,
) {
    for item in items {
        let ImplItem::Let(declaration) = item else {
            continue;
        };
        let name = &declaration.name.text;
        let Some(layout) = design.source_layouts.get(&format!("{root_path}.{name}")) else {
            // Entity instance declarations deliberately have no testbench
            // storage layout and are represented by the hierarchy instead.
            continue;
        };
        let id = ProcessStorageId(process_ir.storages.len() as u32);
        let ty = declared_nominal_type(declaration.ty.as_ref(), resolved)
            .or_else(|| {
                declaration
                    .value
                    .as_ref()
                    .and_then(|value| typed.expr_type(ast::expr_span(value)))
                    .filter(|ty| !matches!(ty, crate::types::Ty::Error))
                    .cloned()
            })
            .or_else(|| process_type_from_layout(layout, resolved));
        let layout = process_layout_with_type(layout, ty.as_ref());
        process_ir.storages.push(ProcessStorage {
            id,
            owner: root,
            name: name.clone(),
            source: resolved.declared(declaration.name.span),
            span: declaration.span,
            ty,
            layout: Some(layout),
            initializer: None,
            bindings: testbench_bindings(
                name, modules, resolved, hierarchy, root, root_path, design,
            ),
        });
    }
}

/// Collect every direct DUT port leaf connected to one testbench storage
/// object. Fan-out deliberately retains several bindings with one projection.
fn testbench_bindings(
    storage: &str,
    modules: &[Module],
    resolved: &Resolved,
    hierarchy: &Hierarchy,
    root: crate::elab::InstanceId,
    root_path: &str,
    design: &Design,
) -> Vec<ProcessStorageBinding> {
    let mut bindings = Vec::new();
    for &child_id in &hierarchy.instance(root).children {
        let child = hierarchy.instance(child_id);
        for connection in &child.connections {
            let Some(storage_prefix) = storage_projection(&connection.signal, storage) else {
                continue;
            };
            let port_path = format!("{root_path}.{}.{}", child.name, connection.port);
            for (index, signal) in design.signals.iter().enumerate() {
                let port_projection = if signal.path == port_path {
                    ""
                } else if let Some(suffix) = signal.path.strip_prefix(&port_path) {
                    if !suffix.starts_with('.') && !suffix.starts_with('[') {
                        continue;
                    }
                    suffix
                } else {
                    continue;
                };
                let Ok(index) = u32::try_from(index) else {
                    continue;
                };
                if is_representation_signal(design, index) {
                    continue;
                }
                let Some(direction) = port_direction(
                    modules,
                    resolved,
                    child.entity_id,
                    &connection.port,
                    port_projection,
                    &port_path,
                    design,
                ) else {
                    continue;
                };
                bindings.push(ProcessStorageBinding {
                    projection: format!("{storage_prefix}{port_projection}"),
                    signal: SignalId(index),
                    direction,
                });
            }
        }
    }
    bindings.sort_by(|left, right| {
        left.projection
            .cmp(&right.projection)
            .then_with(|| left.signal.0.cmp(&right.signal.0))
    });
    bindings
        .dedup_by(|left, right| left.projection == right.projection && left.signal == right.signal);
    bindings
}

/// Suffix of a connected source path relative to a storage root.
fn storage_projection<'a>(connected: &'a str, storage: &str) -> Option<&'a str> {
    if connected == storage {
        return Some("");
    }
    connected
        .strip_prefix(storage)
        .filter(|suffix| suffix.starts_with('.') || suffix.starts_with('['))
}

/// Direction of one flattened child port leaf.
fn port_direction(
    modules: &[Module],
    resolved: &Resolved,
    entity: crate::resolve::DefId,
    port: &str,
    projection: &str,
    port_path: &str,
    design: &Design,
) -> Option<LayoutDirection> {
    let declaration = modules
        .iter()
        .flat_map(|module| &module.items)
        .find_map(|item| {
            let ast::Item::Entity(declaration) = item else {
                return None;
            };
            (resolved.declared(declaration.name.span) == Some(entity)).then_some(declaration)
        })?;
    let port = declaration
        .ports
        .iter()
        .find(|candidate| candidate.name.text == port)?;
    if let Some(direction) = port.dir {
        return Some(lower_direction(direction));
    }
    let layout = design.source_layouts.get(port_path)?;
    layout_direction(layout, projection)
}

/// Follow a flattened field/index suffix through a source layout to the view
/// field that supplies its direction.
fn layout_direction(layout: &crate::ir::SourceLayout, projection: &str) -> Option<LayoutDirection> {
    match &layout.kind {
        LayoutKind::Struct { fields, .. } => {
            let field_path = projection.strip_prefix('.')?;
            let boundary = field_path.find(['.', '[']).unwrap_or(field_path.len());
            let (name, rest) = field_path.split_at(boundary);
            let field = fields.iter().find(|field| field.name == name)?;
            field
                .direction
                .clone()
                .or_else(|| layout_direction(&field.layout, rest))
        }
        LayoutKind::Array { element, .. } => {
            let rest = projection.strip_prefix('[')?.split_once(']')?.1;
            layout_direction(element, rest)
        }
        LayoutKind::Scalar { .. } | LayoutKind::Packed { .. } | LayoutKind::Opaque { .. } => None,
    }
}

/// Convert source port direction into the frontend-independent IR spelling.
fn lower_direction(direction: ast::Direction) -> LayoutDirection {
    match direction {
        ast::Direction::In => LayoutDirection::In,
        ast::Direction::Out => LayoutDirection::Out,
        ast::Direction::Inout => LayoutDirection::InOut,
    }
}

/// Whether a signal is an internal metavalue representation leaf rather than
/// source-visible storage.
fn is_representation_signal(design: &Design, id: u32) -> bool {
    design.meta_of.values().any(|companion| *companion == id)
        || design.metavalue_temps.contains(&id)
}

/// How a test process is activated: a canonical clock process becomes
/// reactive on the signal it toggles, everything else starts at time zero.
fn process_activation(
    statements: &[Stmt],
    root_path: &str,
    design: &Design,
    resolved: &Resolved,
    owner: crate::elab::InstanceId,
    process_ir: &ProcessIr,
) -> ProcessActivation {
    if !crate::testbench::is_clock_process(statements) {
        return ProcessActivation::TimeZero;
    }
    let Stmt::Assign { target, .. } = &statements[0] else {
        unreachable!("is_clock_process accepted a non-assignment")
    };
    if let Some(storage) = assignment_base(target)
        .and_then(|path| resolved.resolved(path.span))
        .and_then(|definition| {
            process_ir
                .storages
                .iter()
                .find(|storage| storage.owner == owner && storage.source == Some(definition))
                .map(|storage| storage.id)
        })
    {
        return ProcessActivation::Reactive {
            sensitivity: vec![ProcessSensitivity::Storage(storage)],
        };
    }
    let target = crate::syntax::pretty::expr_string(target);
    let qualified = format!("{root_path}.{target}");
    let sensitivity = design
        .signals
        .iter()
        .position(|signal| signal.path == qualified || signal.path == target)
        .and_then(|index| u32::try_from(index).ok())
        .map(crate::ir::SignalId)
        .map(ProcessSensitivity::Signal)
        .into_iter()
        .collect();
    ProcessActivation::Reactive { sensitivity }
}

/// Lower one process body into a control-flow graph, returning the finished
/// [`ProcessCfg`].
fn lower_process(
    id: ProcessId,
    owner: crate::elab::InstanceId,
    label: Option<String>,
    span: crate::diag::Span,
    activation: ProcessActivation,
    statements: &[Stmt],
    context: &mut LoweringContext<'_>,
) -> ProcessCfg {
    let mut process = ProcessCfg {
        id,
        root: owner,
        owner,
        label,
        span,
        activation,
        entry: ProcessBlockId(0),
        locals: Vec::new(),
        blocks: vec![ProcessBlock::empty(ProcessBlockId(0))],
    };
    lower_statements(statements, context, &mut process, ProcessBlockId(0));
    process
}

/// Lower the legacy implicit test process without moving a
/// declaration initializer across an earlier bare statement. Explicit
/// `process` blocks never use this path; their impl-level state is initialized
/// before independently scheduled processes start.
fn lower_legacy_process(
    id: ProcessId,
    owner: crate::elab::InstanceId,
    label: Option<String>,
    span: crate::diag::Span,
    activation: ProcessActivation,
    items: &[&ImplItem],
    context: &mut LoweringContext<'_>,
) -> ProcessCfg {
    let mut process = ProcessCfg {
        id,
        root: owner,
        owner,
        label,
        span,
        activation,
        entry: ProcessBlockId(0),
        locals: Vec::new(),
        blocks: vec![ProcessBlock::empty(ProcessBlockId(0))],
    };
    let mut current = Some(ProcessBlockId(0));
    for item in items {
        let Some(block) = current else { break };
        current = match item {
            ImplItem::Stmt(statement) => lower_statement(statement, context, &mut process, block),
            ImplItem::Let(declaration) if declaration_is_entity(declaration, context.resolved) => {
                lower_ordered_instance_connections(declaration, context, &mut process, block)
            }
            ImplItem::Let(declaration) => {
                lower_ordered_storage_initializer(declaration, context, &mut process, block)
            }
            _ => Some(block),
        };
    }
    process
}

/// Resolve the declaration's annotated type through generic/index wrappers.
fn declaration_type_definition(
    ty: &ast::Type,
    resolved: &Resolved,
) -> Option<crate::resolve::DefId> {
    match ty {
        ast::Type::Path(path) => resolved.resolved(path.span),
        ast::Type::Generic { base, .. } | ast::Type::Indexed { base, .. } => {
            declaration_type_definition(base, resolved)
        }
        ast::Type::View { target, .. } => declaration_type_definition(target, resolved),
    }
}

fn declaration_is_entity(declaration: &ast::LetDecl, resolved: &Resolved) -> bool {
    declaration
        .ty
        .as_ref()
        .and_then(|ty| declaration_type_definition(ty, resolved))
        .is_some_and(|definition| {
            resolved.kind_of(definition) == Some(crate::resolve::DefKind::Entity)
        })
}

/// Whether a connection expression is independent of testbench runtime
/// storage. Those values already enter the canonical hardware driver graph;
/// only the remaining expressions belong in the source-ordered test process.
fn process_connection_is_static(expression: &ast::Expr, resolved: &Resolved) -> bool {
    match expression {
        ast::Expr::Int { .. }
        | ast::Expr::SuffixLit { .. }
        | ast::Expr::BitStrLit { .. }
        | ast::Expr::CharLit { .. }
        | ast::Expr::StrLit { .. } => true,
        ast::Expr::Path(path) => resolved
            .resolved(path.span)
            .and_then(|definition| resolved.kind_of(definition))
            .is_some_and(|kind| {
                matches!(
                    kind,
                    crate::resolve::DefKind::Const
                        | crate::resolve::DefKind::EnumVariant
                        | crate::resolve::DefKind::Param
                )
            }),
        ast::Expr::Field { base, .. } | ast::Expr::SysAttr { base, .. } => {
            process_connection_is_static(base, resolved)
        }
        ast::Expr::Index { base, index, .. } => {
            process_connection_is_static(base, resolved)
                && process_connection_is_static(index, resolved)
        }
        ast::Expr::Range { lo, hi, .. } => {
            process_connection_is_static(lo, resolved) && process_connection_is_static(hi, resolved)
        }
        ast::Expr::PartialRange { lo, hi, .. } => {
            lo.as_deref()
                .is_none_or(|bound| process_connection_is_static(bound, resolved))
                && hi
                    .as_deref()
                    .is_none_or(|bound| process_connection_is_static(bound, resolved))
        }
        ast::Expr::Unary { rhs, .. } => process_connection_is_static(rhs, resolved),
        ast::Expr::Binary { lhs, rhs, .. } => {
            process_connection_is_static(lhs, resolved)
                && process_connection_is_static(rhs, resolved)
        }
        ast::Expr::IfExpr {
            cond, then, els, ..
        } => {
            process_connection_is_static(cond, resolved)
                && process_connection_is_static(then, resolved)
                && process_connection_is_static(els, resolved)
        }
        ast::Expr::Match {
            scrutinee, arms, ..
        } => {
            process_connection_is_static(scrutinee, resolved)
                && arms.iter().all(|arm| {
                    arm.value_expr()
                        .is_some_and(|value| process_connection_is_static(value, resolved))
                })
        }
        ast::Expr::Concat { parts, .. } => parts
            .iter()
            .all(|part| process_connection_is_static(part, resolved)),
        ast::Expr::Array { elems, .. } => elems
            .iter()
            .all(|element| process_connection_is_static(element, resolved)),
        ast::Expr::Construct {
            args, spread: None, ..
        } => args.iter().all(|field| {
            field
                .value
                .as_ref()
                .is_some_and(|value| process_connection_is_static(value, resolved))
        }),
        ast::Expr::Call { .. }
        | ast::Expr::Construct {
            spread: Some(_), ..
        } => false,
    }
}

fn instance_has_runtime_connections(declaration: &ast::LetDecl, resolved: &Resolved) -> bool {
    if !declaration_is_entity(declaration, resolved) {
        return false;
    }
    let values = match declaration.value.as_ref() {
        Some(ast::Expr::Construct { args, .. }) => args
            .iter()
            .filter_map(|argument| argument.value.as_ref())
            .collect::<Vec<_>>(),
        Some(ast::Expr::Concat { parts, .. }) => parts.iter().collect(),
        _ => return false,
    };
    values.into_iter().any(|value| {
        !process_connection_is_place(value) && !process_connection_is_static(value, resolved)
    })
}

fn process_connection_is_place(expression: &ast::Expr) -> bool {
    match expression {
        ast::Expr::Path(_) => true,
        ast::Expr::Field { base, .. } | ast::Expr::Index { base, .. } => {
            process_connection_is_place(base)
        }
        _ => false,
    }
}

/// Execute value-carrying DUT connections at the declaration's source-order
/// position. Plain names remain persistent storage bindings and static values
/// are canonical hardware drivers; this path covers expressions such as
/// `.a = source + 1` that depend on current testbench state.
fn lower_ordered_instance_connections(
    declaration: &ast::LetDecl,
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let Some(child) = context
        .hierarchy
        .instance(process.owner)
        .children
        .iter()
        .map(|child| context.hierarchy.instance(*child))
        .find(|child| child.name == declaration.name.text)
    else {
        return Some(block);
    };
    let Some(value) = declaration.value.as_ref() else {
        return Some(block);
    };
    let pairs = match value {
        ast::Expr::Construct { args, .. } => args
            .iter()
            .enumerate()
            .filter_map(|(index, argument)| {
                let value = argument.value.as_ref()?;
                let port = argument
                    .field
                    .as_ref()
                    .map(|field| field.text.clone())
                    .or_else(|| {
                        child
                            .connections
                            .get(index)
                            .map(|connection| connection.port.clone())
                    })?;
                Some((port, value))
            })
            .collect::<Vec<_>>(),
        ast::Expr::Concat { parts, .. } => parts
            .iter()
            .enumerate()
            .filter_map(|(index, value)| Some((child.connections.get(index)?.port.clone(), value)))
            .collect(),
        _ => return Some(block),
    };
    let mut wrote = false;
    for (port, value) in pairs {
        if process_connection_is_place(value)
            || process_connection_is_static(value, context.resolved)
        {
            continue;
        }
        let path = format!("{}.{}.{}", context.root_path, declaration.name.text, port);
        if !matches!(
            port_direction(
                context.modules,
                context.resolved,
                child.entity_id,
                &port,
                "",
                &path,
                context.design,
            ),
            Some(LayoutDirection::In | LayoutDirection::InOut)
        ) {
            continue;
        }
        let Some((index, signal)) = context
            .design
            .signals
            .iter()
            .enumerate()
            .find(|(_, signal)| signal.path == path)
        else {
            continue;
        };
        let Ok(index) = u32::try_from(index) else {
            continue;
        };
        let signal_id = SignalId(index);
        if is_representation_signal(context.design, signal_id.0) {
            continue;
        }
        let target_type = context
            .design
            .source_layouts
            .get(&signal.path)
            .and_then(|layout| process_type_from_layout(layout, context.resolved))
            .or_else(|| {
                context
                    .typed
                    .expr_type(ast::expr_span(value))
                    .filter(|ty| !matches!(ty, crate::types::Ty::Error))
                    .cloned()
            });
        let width = context.design.signal_width(signal_id);
        let target = push_value(
            declaration.span,
            target_type.clone(),
            width,
            ProcessValueKind::Signal {
                signals: vec![signal_id],
                state: ProcessSignalState::Current,
            },
            context,
        );
        let assigned = value_ref_with_type(value, process, context, target_type.as_ref());
        process.blocks[block.0 as usize]
            .instructions
            .push(ProcessInstruction::Assign {
                semantics: ProcessAssignment::StagedSignal,
                driver_context: Some(process.id.0),
                target,
                value: assigned,
                span: ast::expr_span(value),
            });
        wrote = true;
    }
    if !wrote {
        return Some(block);
    }
    let resume = process.push_block();
    process.blocks[block.0 as usize].terminator = ProcessTerminator::Suspend {
        operation: ProcessSuspendOp::Settle,
        arguments: Vec::new(),
        resume,
        span: declaration.span,
    };
    Some(resume)
}

fn lower_ordered_storage_initializer(
    declaration: &ast::LetDecl,
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let definition = context.resolved.declared(declaration.name.span)?;
    let (storage, ty, drives_design) = context
        .process_ir
        .storages
        .iter()
        .find(|storage| storage.owner == process.owner && storage.source == Some(definition))
        .map(|storage| {
            (
                storage.id,
                storage.ty.clone(),
                storage.bindings.iter().any(|binding| {
                    matches!(
                        binding.direction,
                        LayoutDirection::In | LayoutDirection::InOut
                    )
                }),
            )
        })?;
    let initializer = declaration.value.as_ref()?;
    let target_kind = ProcessValueKind::Storage(storage);
    let target_width = source_value_width(&target_kind, ty.as_ref(), process, context);
    let target = push_value(
        declaration.name.span,
        ty.clone(),
        target_width,
        target_kind,
        context,
    );
    let value = value_ref_with_type(initializer, process, context, ty.as_ref());
    process.blocks[block.0 as usize]
        .instructions
        .push(ProcessInstruction::Assign {
            semantics: ProcessAssignment::ImmediateStorage,
            driver_context: None,
            target,
            value,
            span: declaration.span,
        });
    if !drives_design {
        return Some(block);
    }
    let resume = process.push_block();
    process.blocks[block.0 as usize].terminator = ProcessTerminator::Suspend {
        operation: ProcessSuspendOp::Settle,
        arguments: Vec::new(),
        resume,
        span: declaration.span,
    };
    Some(resume)
}

/// Returns the still-open tail block. `None` means control terminated and
/// following statements in the source block are unreachable.
fn lower_statements(
    statements: &[Stmt],
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    entry: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let mut current = Some(entry);
    for statement in statements {
        let Some(block) = current else { break };
        current = lower_statement(statement, context, process, block);
    }
    current
}

/// Lower one statement into `block`, returning the block that execution
/// continues in -- the same one for straight-line statements, a new join
/// block for anything that branches or suspends.
fn lower_statement(
    statement: &Stmt,
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    match statement {
        Stmt::Use(_) => Some(block),
        Stmt::Let(declaration) => {
            let local = push_local(process, declaration, context);
            let target = process.locals[local.0 as usize].ty.clone();
            let initializer = declaration
                .value
                .as_ref()
                .map(|value| value_ref_with_type(value, process, context, target.as_ref()));
            process.blocks[block.0 as usize]
                .instructions
                .push(ProcessInstruction::Declare {
                    local,
                    initializer,
                    span: declaration.span,
                });
            Some(block)
        }
        Stmt::Assign {
            target,
            value,
            after,
            span,
            ..
        } => {
            let source_semantics = assignment_semantics(target, process, context);
            let target_type = context.typed.expr_type(ast::expr_span(target)).cloned();
            let source_target = target;
            let target = value_ref(source_target, process, context);
            let semantics =
                process_place_assignment(target, context.process_ir).unwrap_or(source_semantics);
            let settle = after.is_none()
                && matches!(process.activation, ProcessActivation::TimeZero)
                && (assignment_drives_design_value(target, context.process_ir)
                    || assignment_drives_design_source(source_target, process, context));
            let value = value_ref_with_type(value, process, context, target_type.as_ref());
            let instruction = match after {
                Some(delay) => ProcessInstruction::Schedule {
                    driver_context: matches!(
                        semantics,
                        ProcessAssignment::StagedSignal | ProcessAssignment::PerPlace
                    )
                    .then_some(process.id.0),
                    target,
                    value,
                    delay: value_ref(delay, process, context),
                    span: *span,
                },
                None => ProcessInstruction::Assign {
                    semantics,
                    driver_context: matches!(
                        semantics,
                        ProcessAssignment::StagedSignal | ProcessAssignment::PerPlace
                    )
                    .then_some(process.id.0),
                    target,
                    value,
                    span: *span,
                },
            };
            process.blocks[block.0 as usize]
                .instructions
                .push(instruction);
            if settle {
                let resume = process.push_block();
                process.blocks[block.0 as usize].terminator = ProcessTerminator::Suspend {
                    operation: ProcessSuspendOp::Settle,
                    arguments: Vec::new(),
                    resume,
                    span: *span,
                };
                Some(resume)
            } else {
                Some(block)
            }
        }
        Stmt::Expr(ast::Expr::Call {
            callee, args, span, ..
        }) => lower_call(callee, args, *span, context, process, block),
        Stmt::Expr(expression) => {
            let argument = value_ref(expression, process, context);
            process.blocks[block.0 as usize]
                .instructions
                .push(ProcessInstruction::Runtime {
                    operation: ProcessRuntimeOp::Call("<expression>".to_string()),
                    arguments: vec![argument],
                    format: None,
                    span: ast::expr_span(expression),
                });
            Some(block)
        }
        Stmt::If(statement) => lower_if(statement, context, process, block),
        Stmt::Match(statement) => lower_match(statement, context, process, block),
        Stmt::For {
            var,
            range,
            body,
            span,
            ..
        } => lower_for(var, range, body, *span, context, process, block),
        Stmt::Return { value, span } => {
            process.blocks[block.0 as usize].terminator = ProcessTerminator::Return {
                value: value
                    .as_ref()
                    .map(|value| value_ref(value, process, context)),
                span: Some(*span),
            };
            None
        }
    }
}

/// Whether a write updates a process local immediately or stages a signal
/// write for the next delta. Classification uses the target's resolved
/// declaration, so a local shadowing a signal name still writes the local.
fn assignment_semantics(
    target: &ast::Expr,
    process: &ProcessCfg,
    context: &LoweringContext<'_>,
) -> ProcessAssignment {
    if matches!(target, ast::Expr::Concat { .. }) {
        // A concat is never one place, even when every leaf has the same
        // storage class. The RHS must be evaluated once and split before any
        // leaf write becomes visible; PerPlace preserves that invariant while
        // letting each local/storage/signal keep its own publication timing.
        return ProcessAssignment::PerPlace;
    }
    let path = assignment_base(target);
    let target = path.and_then(|path| context.resolved.resolved(path.span));
    let is_local = target.is_some_and(|target| {
        process
            .locals
            .iter()
            .any(|local| local.source == Some(target))
    });
    if is_local {
        ProcessAssignment::ImmediateLocal
    } else if path
        .and_then(|path| testbench_storage(path, process.owner, context))
        .is_some()
    {
        ProcessAssignment::ImmediateStorage
    } else {
        ProcessAssignment::StagedSignal
    }
}

/// The path at the base of an assignment target, looking through field and
/// index access. `None` when the target is not a place.
fn assignment_base(target: &ast::Expr) -> Option<&ast::Path> {
    match target {
        ast::Expr::Path(path) => Some(path),
        ast::Expr::Field { base, .. } | ast::Expr::Index { base, .. } => assignment_base(base),
        _ => None,
    }
}

/// Whether an immediate foreground assignment drives at least one DUT input.
/// The fixed runtime must publish that storage and reach a reactive fixed
/// point before the next source statement observes connected outputs.
fn assignment_drives_design_source(
    target: &ast::Expr,
    process: &ProcessCfg,
    context: &LoweringContext<'_>,
) -> bool {
    if let ast::Expr::Concat { parts, .. } = target {
        return parts
            .iter()
            .any(|part| assignment_drives_design_source(part, process, context));
    }
    // A direct instance-port place is already a flattened hardware signal,
    // rather than testbench storage with an explicit binding. A foreground
    // write to it still needs the same commit/reactive fixed point before the
    // next source statement observes a dependent output.
    if signal_reference(target, process, context).is_some() {
        return true;
    }
    assignment_base(target)
        .and_then(|path| testbench_storage(path, process.owner, context))
        .and_then(|storage| context.process_ir.storages.get(storage.0 as usize))
        .is_some_and(|storage| {
            storage.bindings.iter().any(|binding| {
                matches!(
                    binding.direction,
                    LayoutDirection::In | LayoutDirection::InOut
                )
            })
        })
}

/// Classify an already-lowered assignment place. This is authoritative for
/// an inlined procedure, where `self` or a formal parameter aliases the
/// caller's local/storage place and the callee AST no longer names that root.
fn process_place_assignment(
    target: ProcessValueId,
    process_ir: &ProcessIr,
) -> Option<ProcessAssignment> {
    match &process_ir.values.get(target.0 as usize)?.kind {
        ProcessValueKind::Local { .. } => Some(ProcessAssignment::ImmediateLocal),
        ProcessValueKind::Storage(_) => Some(ProcessAssignment::ImmediateStorage),
        ProcessValueKind::Signal { .. } => Some(ProcessAssignment::StagedSignal),
        ProcessValueKind::Field { base, .. }
        | ProcessValueKind::Index { base, .. }
        | ProcessValueKind::PackedSlice { base, .. } => process_place_assignment(*base, process_ir),
        ProcessValueKind::Concat(_) => Some(ProcessAssignment::PerPlace),
        _ => None,
    }
}

/// Whether a lowered place ultimately aliases a DUT input. This complements
/// the source-level check for ordinary code and preserves settle semantics
/// when a procedure receiver/parameter hides the caller's storage root.
fn assignment_drives_design_value(target: ProcessValueId, process_ir: &ProcessIr) -> bool {
    match &process_ir
        .values
        .get(target.0 as usize)
        .map(|value| &value.kind)
    {
        Some(ProcessValueKind::Signal { .. }) => true,
        Some(ProcessValueKind::Storage(storage)) => process_ir
            .storages
            .get(storage.0 as usize)
            .is_some_and(|storage| {
                storage.bindings.iter().any(|binding| {
                    matches!(
                        binding.direction,
                        LayoutDirection::In | LayoutDirection::InOut
                    )
                })
            }),
        Some(ProcessValueKind::Field { base, .. })
        | Some(ProcessValueKind::Index { base, .. })
        | Some(ProcessValueKind::PackedSlice { base, .. }) => {
            assignment_drives_design_value(*base, process_ir)
        }
        Some(ProcessValueKind::Concat(parts)) => parts
            .iter()
            .any(|part| assignment_drives_design_value(*part, process_ir)),
        _ => false,
    }
}

/// Classify the scheduler meaning before source syntax is discarded. A
/// nominal `std::sim::time` value is authoritative; the suffix check keeps
/// bare frontend fixtures (which intentionally omit std) deterministic.
fn await_is_time(argument: &ast::Expr, context: &LoweringContext<'_>) -> bool {
    if matches!(argument, ast::Expr::SuffixLit { .. }) {
        return true;
    }
    context
        .typed
        .expr_type(ast::expr_span(argument))
        .is_some_and(|ty| {
            matches!(ty, crate::types::Ty::Named(definition)
                if context.resolved.lang_of(*definition) == Some("time"))
        })
}

/// Edge waits differ from level conditions in one important way: they must
/// suspend before testing the event predicate, even if the process itself was
/// resumed during the current event. The CFG shape records that distinction;
/// the fixed scheduler only needs a generic "wake after state change" record.
fn await_is_event(argument: &ast::Expr) -> bool {
    match argument {
        ast::Expr::SysAttr { attr, .. } => {
            matches!(attr.text.as_str(), "event" | "rising" | "falling")
        }
        ast::Expr::Call { callee, .. } => matches!(
            callee.as_ref(),
            ast::Expr::Field { field, .. }
                if matches!(field.text.as_str(), "edge" | "rising" | "falling")
        ),
        _ => false,
    }
}

/// Lower a call: either a runtime operation (`assert!`, `print!`) or an
/// ordinary named call that lowering did not inline.
fn lower_call(
    callee: &ast::Expr,
    arguments: &[ast::Expr],
    span: crate::diag::Span,
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let name = callee_name(callee);
    let lowered_arguments = arguments
        .iter()
        .map(|argument| value_ref(argument, process, context))
        .collect::<Vec<_>>();
    match name.as_str() {
        "await" => {
            if arguments
                .first()
                .is_some_and(|argument| await_is_time(argument, context))
            {
                let resume = process.push_block();
                process.blocks[block.0 as usize].terminator = ProcessTerminator::Suspend {
                    operation: ProcessSuspendOp::AwaitTime,
                    arguments: lowered_arguments,
                    resume,
                    span,
                };
                return Some(resume);
            }

            let condition = *lowered_arguments.first()?;
            let check = process.push_block();
            let wait = process.push_block();
            let settle = process.push_block();
            let resume = process.push_block();
            process.blocks[block.0 as usize].terminator =
                ProcessTerminator::Goto(if arguments.first().is_some_and(await_is_event) {
                    wait
                } else {
                    check
                });
            process.blocks[check.0 as usize].terminator = ProcessTerminator::Branch {
                condition,
                then_block: settle,
                else_block: wait,
            };
            process.blocks[wait.0 as usize].terminator = ProcessTerminator::Suspend {
                operation: ProcessSuspendOp::AwaitCondition,
                arguments: Vec::new(),
                resume: check,
                span,
            };
            process.blocks[settle.0 as usize].terminator = ProcessTerminator::Suspend {
                operation: ProcessSuspendOp::Settle,
                arguments: Vec::new(),
                resume,
                span,
            };
            Some(resume)
        }
        "stop" => {
            process.blocks[block.0 as usize].terminator = ProcessTerminator::Stop { span };
            None
        }
        "finish" => {
            process.blocks[block.0 as usize].terminator = ProcessTerminator::Finish { span };
            None
        }
        _ => {
            if let Some(tail) =
                inline_process_procedure_call(callee, arguments, context, process, block)
            {
                return Some(tail);
            }
            let operation = match name.as_str() {
                "assert" => ProcessRuntimeOp::Assert,
                "warn" => ProcessRuntimeOp::Warn,
                "print" => ProcessRuntimeOp::Print,
                "seed" if builtin_callee_is(callee, "seed", context) => ProcessRuntimeOp::Seed,
                _ => ProcessRuntimeOp::Call(name),
            };
            let format = lower_process_format(&operation, arguments, &lowered_arguments, context);
            process.blocks[block.0 as usize]
                .instructions
                .push(ProcessInstruction::Runtime {
                    operation,
                    arguments: lowered_arguments,
                    format,
                    span,
                });
            Some(block)
        }
    }
}

/// Inline a side-effecting, no-return Siox call into the caller's Process CFG.
///
/// Phase 1 procedures are intentionally handled at the same typed lowering
/// boundary as value-returning functions: receiver and formal parameters are
/// aliases for caller places, so assignments become ordinary canonical
/// Process writes. For now this accepts straight-line assignment bodies. More
/// general call CFGs remain fail-closed instead of introducing an interpreter
/// or a second runtime call convention.
fn inline_process_procedure_call(
    callee: &ast::Expr,
    arguments: &[ast::Expr],
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let first_value = context.process_ir.values.len();
    let original_process = process.clone();
    let (function, receiver) = match callee {
        ast::Expr::Field { base, field, .. } => {
            let receiver = value_ref(base, process, context);
            let owner = context
                .process_ir
                .values
                .get(receiver.0 as usize)
                .and_then(|value| value.ty.as_ref())
                .and_then(|ty| process_type_key(ty, context))?;
            (
                context
                    .functions
                    .get_associated(&owner, &field.text)?
                    .clone(),
                Some(receiver),
            )
        }
        _ => (context.functions.get(callee)?.clone(), None),
    };
    let body = function.body.as_ref()?;
    if function.ret.is_some()
        || function.params.iter().any(|parameter| parameter.is_self) != receiver.is_some()
        || !body
            .stmts
            .iter()
            .all(|statement| matches!(statement, Stmt::Assign { .. }))
    {
        truncate_process_values(context, first_value);
        return None;
    }
    let parameters = function
        .params
        .iter()
        .filter(|parameter| !parameter.is_self)
        .collect::<Vec<_>>();
    if parameters.len() != arguments.len() {
        truncate_process_values(context, first_value);
        return None;
    }

    let mut bindings = std::collections::HashMap::new();
    for (parameter, argument) in parameters.into_iter().zip(arguments) {
        let Some(name) = parameter.name.as_ref() else {
            truncate_process_values(context, first_value);
            return None;
        };
        let Some(definition) = context.resolved.declared(name.span) else {
            truncate_process_values(context, first_value);
            return None;
        };
        let parameter_type = parameter
            .ty
            .as_ref()
            .and_then(|ty| process_declared_type(ty, context));
        let argument = value_ref_with_type(argument, process, context, parameter_type.as_ref());
        bindings.insert(definition, argument);
    }
    if !context.inline_functions.insert(function.span) {
        truncate_process_values(context, first_value);
        return None;
    }

    context.value_bindings.push(bindings);
    context.inline_self_values.push(receiver);
    context.inline_return_types.push(None);
    let tail = lower_statements(&body.stmts, context, process, block);
    context.inline_return_types.pop();
    context.inline_self_values.pop();
    context.value_bindings.pop();
    context.inline_functions.remove(&function.span);

    match tail {
        Some(tail) => Some(tail),
        None => {
            *process = original_process;
            truncate_process_values(context, first_value);
            None
        }
    }
}

fn lower_process_format(
    operation: &ProcessRuntimeOp,
    arguments: &[ast::Expr],
    lowered: &[ProcessValueId],
    context: &LoweringContext<'_>,
) -> Option<Vec<ProcessFormatPart>> {
    let message_index = match operation {
        ProcessRuntimeOp::Print => 0,
        ProcessRuntimeOp::Assert | ProcessRuntimeOp::Warn => 1,
        ProcessRuntimeOp::Seed | ProcessRuntimeOp::Call(_) => return None,
    };
    let ast::Expr::StrLit { text, .. } = arguments.get(message_index)? else {
        return None;
    };
    let mut values = arguments.iter().zip(lowered).skip(message_index + 1);
    crate::syntax::format::parts(text)
        .into_iter()
        .map(|part| match part {
            crate::syntax::format::FormatPart::Text(text) => Some(ProcessFormatPart::Text(text)),
            crate::syntax::format::FormatPart::Placeholder => {
                let (expression, value) = values.next()?;
                Some(ProcessFormatPart::Value {
                    value: *value,
                    kind: process_display_kind(expression, *value, context)?,
                })
            }
        })
        .collect()
}

/// Recover the recursive declaration layout of an arena projection while the
/// typed source lowerer is constructing Process IR. Expression
/// typing does not retain a standalone type for every field/index expression,
/// but the storage/local declaration does retain the authoritative layout.
fn process_value_source_layout(
    value: ProcessValueId,
    process_ir: &ProcessIr,
) -> Option<&crate::ir::SourceLayout> {
    if let Some(layout) = process_ir
        .value_layouts
        .get(value.0 as usize)
        .and_then(Option::as_ref)
    {
        return Some(layout);
    }
    let value = process_ir.values.get(value.0 as usize)?;
    match &value.kind {
        ProcessValueKind::Storage(storage) => {
            process_ir.storages.get(storage.0 as usize)?.layout.as_ref()
        }
        ProcessValueKind::StorageState {
            storage,
            state: ProcessSignalState::Old,
        } => process_ir.storages.get(storage.0 as usize)?.layout.as_ref(),
        ProcessValueKind::Local { process, local } => process_ir
            .processes
            .get(process.0 as usize)?
            .locals
            .get(local.0 as usize)?
            .layout
            .as_ref(),
        ProcessValueKind::Field { base, field } => {
            let layout = process_value_source_layout(*base, process_ir)?;
            let LayoutKind::Struct { fields, .. } = &layout.kind else {
                return None;
            };
            fields
                .iter()
                .find(|candidate| candidate.name == *field)
                .map(|candidate| &candidate.layout)
        }
        ProcessValueKind::Index { base, .. } => {
            let layout = process_value_source_layout(*base, process_ir)?;
            let LayoutKind::Array { element, .. } = &layout.kind else {
                return None;
            };
            Some(element)
        }
        ProcessValueKind::Select {
            then_value,
            else_value,
            ..
        } => {
            let then_layout = process_value_source_layout(*then_value, process_ir)?;
            (process_value_source_layout(*else_value, process_ir) == Some(then_layout))
                .then_some(then_layout)
        }
        _ => None,
    }
}

/// Resolve the written endpoints of a packed slice while source syntax still
/// distinguishes a full from a partial range. Missing endpoints inherit the
/// packed declaration's own left/right bounds.
fn packed_slice_bounds(
    index: &ast::Expr,
    layout: &crate::ir::SourceLayout,
    context: &LoweringContext<'_>,
) -> Option<(i64, i64)> {
    let LayoutKind::Packed {
        range: Some(declared),
        ..
    } = &layout.kind
    else {
        return None;
    };
    let evaluate = |expression: &ast::Expr| {
        crate::ir::eval_const_fns(expression, context.constant_integers, context.functions, 0)
    };
    match index {
        ast::Expr::Range { lo, hi, .. } => Some((evaluate(lo)?, evaluate(hi)?)),
        ast::Expr::PartialRange { lo, hi, .. } => {
            let left = match lo.as_deref() {
                Some(left) => evaluate(left)?,
                None => declared.left,
            };
            let right = match hi.as_deref() {
                Some(right) => evaluate(right)?,
                None => declared.right,
            };
            Some((left, right))
        }
        _ => None,
    }
}

fn packed_slice_layout(
    layout: &crate::ir::SourceLayout,
    left: i64,
    right: i64,
    span: crate::diag::Span,
) -> Option<crate::ir::SourceLayout> {
    let LayoutKind::Packed {
        family,
        element_enum,
        ..
    } = &layout.kind
    else {
        return None;
    };
    Some(crate::ir::SourceLayout {
        span,
        kind: LayoutKind::Packed {
            width: u32::try_from(left.abs_diff(right).checked_add(1)?).ok()?,
            family: family.clone(),
            range: Some(crate::ir::LayoutRange { left, right }),
            element_enum: element_enum.clone(),
        },
    })
}

/// Preserve the declared domain on a source-level runtime index. The digital
/// lowering uses the same equality-set predicate: it works for negative
/// labels, unsigned index values, and either range direction without making a
/// backend reinterpret the source type as signed or unsigned.
fn checked_process_index(
    index: ProcessValueId,
    span: crate::diag::Span,
    range: crate::ir::LayoutRange,
    context: &mut LoweringContext<'_>,
) -> ProcessValueId {
    if arena_constant_integer(index, &context.process_ir.values).is_some()
        || matches!(
            context
                .process_ir
                .values
                .get(index.0 as usize)
                .map(|value| &value.kind),
            Some(ProcessValueKind::Range { .. })
        )
    {
        return index;
    }
    let Some((ty, width)) = context
        .process_ir
        .values
        .get(index.0 as usize)
        .map(|value| (value.ty.clone(), value.bit_width))
    else {
        return index;
    };
    let Some(width) = width else {
        return index;
    };

    let mut valid = None;
    let mut label = range.left;
    loop {
        let literal = push_value(
            span,
            ty.clone(),
            Some(width),
            ProcessValueKind::Number(ProcessNumber::Integer(vec![label as u64])),
            context,
        );
        let equal = push_value(
            span,
            None,
            Some(1),
            ProcessValueKind::Binary {
                operation: ProcessBinaryOp::Eq,
                left: index,
                right: literal,
            },
            context,
        );
        valid = Some(match valid {
            None => equal,
            Some(previous) => push_value(
                span,
                None,
                Some(1),
                ProcessValueKind::Binary {
                    operation: ProcessBinaryOp::Or,
                    left: previous,
                    right: equal,
                },
                context,
            ),
        });
        if label == range.right {
            break;
        }
        let Some(next) = (if range.ascending() {
            label.checked_add(1)
        } else {
            label.checked_sub(1)
        }) else {
            return index;
        };
        label = next;
    }

    push_value(
        span,
        ty,
        Some(width),
        ProcessValueKind::CheckedIndex {
            index,
            valid: valid.expect("a layout range contains at least one label"),
            left: range.left,
            right: range.right,
            span,
        },
        context,
    )
}

fn process_display_kind(
    expression: &ast::Expr,
    value: ProcessValueId,
    context: &LoweringContext<'_>,
) -> Option<ProcessDisplayKind> {
    let usable = |ty: &&crate::types::Ty| !matches!(ty, crate::types::Ty::Error);
    let process_value = context.process_ir.values.get(value.0 as usize)?;
    if matches!(process_value.kind, ProcessValueKind::String(_)) {
        return Some(ProcessDisplayKind::String);
    }
    let ty = context
        .typed
        .expr_type(ast::expr_span(expression))
        .filter(usable)
        .cloned()
        .or_else(|| process_value_type(value, context));
    match ty.as_ref() {
        Some(ty) => process_display_kind_for_type(ty, context),
        None => process_display_kind_from_value(process_value, context),
    }
}

fn process_display_kind_for_type(
    ty: &crate::types::Ty,
    context: &LoweringContext<'_>,
) -> Option<ProcessDisplayKind> {
    match ty {
        crate::types::Ty::Integer => Some(ProcessDisplayKind::Signed),
        crate::types::Ty::Real => Some(ProcessDisplayKind::Real),
        crate::types::Ty::Char => Some(ProcessDisplayKind::Character),
        crate::types::Ty::Array { elem, family, .. } => {
            if family.is_none() && matches!(elem.as_ref(), crate::types::Ty::Char) {
                Some(ProcessDisplayKind::String)
            } else if family.as_deref().and_then(|name| name.rsplit("::").next()) == Some("signed")
            {
                Some(ProcessDisplayKind::Signed)
            } else {
                Some(ProcessDisplayKind::Unsigned)
            }
        }
        crate::types::Ty::Named(definition) => {
            let info = context.resolved.def(*definition)?;
            if info.kind != crate::resolve::DefKind::Enum {
                return None;
            }
            let qualified = context.resolved.qualified_name(*definition);
            let key = qualified
                .filter(|name| context.design.enum_syms.contains_key(name))
                .or_else(|| {
                    context
                        .design
                        .enum_syms
                        .contains_key(&info.name)
                        .then(|| info.name.clone())
                })?;
            Some(ProcessDisplayKind::Enum(key))
        }
        crate::types::Ty::Void | crate::types::Ty::Error => None,
    }
}

/// Preserve presentation semantics when the typed AST has no entry for a
/// derived value synthesized while inlining constants or operators.
fn process_display_kind_from_value(
    value: &ProcessValue,
    context: &LoweringContext<'_>,
) -> Option<ProcessDisplayKind> {
    match &value.kind {
        ProcessValueKind::Number(ProcessNumber::Real(_)) => Some(ProcessDisplayKind::Real),
        ProcessValueKind::Number(ProcessNumber::Integer(_))
        | ProcessValueKind::BitString { .. }
        | ProcessValueKind::Char(_) => Some(ProcessDisplayKind::Unsigned),
        ProcessValueKind::String(_) => Some(ProcessDisplayKind::String),
        ProcessValueKind::Unary { operation, operand } => process_value_type(*operand, context)
            .as_ref()
            .and_then(|ty| process_display_kind_for_type(ty, context))
            .or(match operation {
                ProcessUnaryOp::Neg | ProcessUnaryOp::RealToInteger => {
                    Some(ProcessDisplayKind::Signed)
                }
                ProcessUnaryOp::IntegerToReal => Some(ProcessDisplayKind::Real),
                ProcessUnaryOp::Not => Some(ProcessDisplayKind::Unsigned),
            }),
        ProcessValueKind::Binary { operation, .. } => match operation {
            ProcessBinaryOp::FloatAdd
            | ProcessBinaryOp::FloatSub
            | ProcessBinaryOp::FloatMul
            | ProcessBinaryOp::FloatDiv => Some(ProcessDisplayKind::Real),
            ProcessBinaryOp::Eq
            | ProcessBinaryOp::Ne
            | ProcessBinaryOp::Lt
            | ProcessBinaryOp::Le
            | ProcessBinaryOp::Gt
            | ProcessBinaryOp::Ge
            | ProcessBinaryOp::SignedLt
            | ProcessBinaryOp::SignedLe
            | ProcessBinaryOp::SignedGt
            | ProcessBinaryOp::SignedGe
            | ProcessBinaryOp::FloatEq
            | ProcessBinaryOp::FloatNe
            | ProcessBinaryOp::FloatLt
            | ProcessBinaryOp::FloatLe
            | ProcessBinaryOp::FloatGt
            | ProcessBinaryOp::FloatGe => Some(ProcessDisplayKind::Unsigned),
            ProcessBinaryOp::SignedAdd
            | ProcessBinaryOp::SignedSub
            | ProcessBinaryOp::SignedMul
            | ProcessBinaryOp::SignedDiv
            | ProcessBinaryOp::ArithmeticShr => Some(ProcessDisplayKind::Signed),
            _ => Some(ProcessDisplayKind::Unsigned),
        },
        _ => None,
    }
}

/// The source enum that owns a resolved variant. Variant paths lower to their
/// elaborated number, so this must be captured before the `DefId` disappears.
fn enum_variant_type(
    definition: crate::resolve::DefId,
    resolved: &Resolved,
) -> Option<crate::types::Ty> {
    let variant = resolved.def(definition)?;
    (variant.kind == crate::resolve::DefKind::EnumVariant)
        .then_some(variant.parent?)
        .map(crate::types::Ty::Named)
}

/// Result type of a layout attribute. Attributes are compiler primitives, but
/// their Boolean values and symbols remain owned by `core::primitive::Bool`.
fn process_attribute_type(
    attribute: &str,
    base: ProcessValueId,
    context: &LoweringContext<'_>,
) -> Option<crate::types::Ty> {
    match attribute {
        "old" => process_value_type(base, context),
        "event" | "ascending" => bool_type(context.resolved),
        "left" | "right" | "high" | "low" | "length" => Some(crate::types::Ty::Integer),
        _ => None,
    }
}

/// Element type selected by intrinsic indexing. Prefer the concrete recursive
/// layout, then fall back to the base value's checked array type for unsized
/// values such as a runtime `string`.
fn process_index_type(
    base: ProcessValueId,
    context: &LoweringContext<'_>,
) -> Option<crate::types::Ty> {
    process_value_source_layout(base, context.process_ir)
        .and_then(|layout| match &layout.kind {
            LayoutKind::Array { element, .. } => {
                process_type_from_layout(element, context.resolved)
            }
            LayoutKind::Packed { element_enum, .. } => element_enum
                .as_deref()
                .and_then(|name| nominal_type_from_name(name, context.resolved)),
            _ => None,
        })
        .or_else(|| match process_value_type(base, context)? {
            crate::types::Ty::Array { elem, .. } => Some(*elem),
            _ => None,
        })
}

/// Recover a result type from already-lowered children when the typed AST has
/// no standalone entry for a derived expression.
fn process_kind_type(
    kind: &ProcessValueKind,
    context: &LoweringContext<'_>,
) -> Option<crate::types::Ty> {
    match kind {
        ProcessValueKind::StorageState {
            storage,
            state: ProcessSignalState::Old,
        } => process_value_type_for_storage(*storage, context),
        ProcessValueKind::StorageState {
            state: ProcessSignalState::Event,
            ..
        }
        | ProcessValueKind::Signal {
            state: ProcessSignalState::Event,
            ..
        } => bool_type(context.resolved),
        ProcessValueKind::Definition(definition) => {
            enum_variant_type(*definition, context.resolved)
        }
        ProcessValueKind::Attribute { base, attribute } => {
            process_attribute_type(attribute, *base, context)
        }
        ProcessValueKind::Index { base, .. } => process_index_type(*base, context),
        ProcessValueKind::Select {
            then_value,
            else_value,
            ..
        } => {
            let then_type = process_value_type(*then_value, context)?;
            (process_value_type(*else_value, context).as_ref() == Some(&then_type))
                .then_some(then_type)
        }
        _ => None,
    }
}

fn process_value_type_for_storage(
    storage: ProcessStorageId,
    context: &LoweringContext<'_>,
) -> Option<crate::types::Ty> {
    context
        .process_ir
        .storages
        .get(storage.0 as usize)
        .and_then(|storage| {
            storage.ty.clone().or_else(|| {
                storage
                    .layout
                    .as_ref()
                    .and_then(|layout| process_type_from_layout(layout, context.resolved))
            })
        })
}

/// Recover the source type of an already-lowered Process value from its own
/// annotation or its canonical storage/signal layout. The temporary typed AST
/// omits types for projections such as `instance.port`; every consumer must
/// use the same recovery rule so character literals, formatting, and operator
/// selection cannot disagree about the value's enum domain.
/// The type an operand is known by. That is its checked type, except inside
/// an inlined generic body: the checker types `v` in `fn abs<T>(v: T)` as the
/// abstract `T` (each call is checked, not the body), so there the operand is
/// lowered and takes the type of the value it is bound to. Without this a
/// signed argument compared unsigned and dispatched no operator impl.
fn operand_type(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<crate::types::Ty> {
    let checked = context
        .typed
        .expr_type(ast::expr_span(expression))
        .filter(|ty| is_concrete_type(ty, context))
        .cloned();
    if checked.is_some() || context.value_bindings.is_empty() {
        return checked;
    }
    let first_value = context.process_ir.values.len();
    let value = value_ref(expression, process, context);
    let ty = process_value_type(value, context);
    truncate_process_values(context, first_value);
    ty
}

/// Whether a checked type says something: not an error, and not a generic
/// body's abstract type parameter.
fn is_concrete_type(ty: &crate::types::Ty, context: &LoweringContext<'_>) -> bool {
    match ty {
        crate::types::Ty::Error => false,
        crate::types::Ty::Named(id) => {
            context.resolved.kind_of(*id) != Some(crate::resolve::DefKind::Param)
        }
        _ => true,
    }
}

fn process_value_type(
    value: ProcessValueId,
    context: &LoweringContext<'_>,
) -> Option<crate::types::Ty> {
    let usable = |ty: &&crate::types::Ty| !matches!(ty, crate::types::Ty::Error);
    let process_value = context.process_ir.values.get(value.0 as usize)?;
    process_value
        .ty
        .as_ref()
        .filter(usable)
        .cloned()
        .or_else(|| match &process_value.kind {
            ProcessValueKind::Storage(storage) => process_value_type_for_storage(*storage, context),
            ProcessValueKind::StorageState { storage, state } => match state {
                ProcessSignalState::Old => process_value_type_for_storage(*storage, context),
                ProcessSignalState::Event => bool_type(context.resolved),
                ProcessSignalState::Current => None,
            },
            ProcessValueKind::Local { process, local } => context
                .process_ir
                .processes
                .get(process.0 as usize)
                .and_then(|process| process.locals.get(local.0 as usize))
                .and_then(|local| {
                    local.ty.clone().or_else(|| {
                        local
                            .layout
                            .as_ref()
                            .and_then(|layout| process_type_from_layout(layout, context.resolved))
                    })
                }),
            ProcessValueKind::Signal {
                state: ProcessSignalState::Event,
                ..
            } => bool_type(context.resolved),
            ProcessValueKind::Signal { signals, .. } => {
                let [signal] = signals.as_slice() else {
                    return None;
                };
                let path = &context.design.signals.get(signal.0 as usize)?.path;
                context
                    .design
                    .source_layouts
                    .get(path)
                    .and_then(|layout| process_type_from_layout(layout, context.resolved))
            }
            kind => process_kind_type(kind, context).or_else(|| {
                process_value_source_layout(value, context.process_ir)
                    .and_then(|layout| process_type_from_layout(layout, context.resolved))
            }),
        })
}

/// Lower an `if` chain into a two-way branch plus a join block, returning
/// the join.
fn lower_if(
    statement: &ast::IfStmt,
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let then_block = process.push_block();
    let else_block = process.push_block();
    process.blocks[block.0 as usize].terminator = ProcessTerminator::Branch {
        condition: value_ref(&statement.cond, process, context),
        then_block,
        else_block,
    };

    let then_tail = lower_statements(&statement.then.stmts, context, process, then_block);
    let else_tail = match statement.else_.as_deref() {
        Some(ElseBranch::Block(block)) => {
            lower_statements(&block.stmts, context, process, else_block)
        }
        Some(ElseBranch::If(statement)) => lower_if(statement, context, process, else_block),
        None => Some(else_block),
    };

    if then_tail.is_none() && else_tail.is_none() {
        return None;
    }
    let join = process.push_block();
    if let Some(tail) = then_tail {
        process.blocks[tail.0 as usize].terminator = ProcessTerminator::Goto(join);
    }
    if let Some(tail) = else_tail {
        process.blocks[tail.0 as usize].terminator = ProcessTerminator::Goto(join);
    }
    Some(join)
}

/// Lower a `match` into one block per arm plus a join block. A statement
/// match with no wildcard arm can fall through, so the terminator keeps a
/// continuation block for that case.
fn lower_match(
    statement: &ast::MatchStmt,
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let scrutinee_type = context
        .typed
        .expr_type(ast::expr_span(&statement.scrutinee))
        .cloned();
    let mut arms = Vec::with_capacity(statement.arms.len());
    for arm in &statement.arms {
        arms.push(ProcessMatchArm {
            pattern: lower_pattern(&arm.pattern, scrutinee_type.as_ref(), context),
            block: process.push_block(),
            span: arm.span,
        });
    }
    let exhaustive = statement
        .arms
        .iter()
        .any(|arm| pattern_has_wildcard(&arm.pattern));
    let fallback = (!exhaustive).then(|| process.push_block());
    let scrutinee = value_ref(&statement.scrutinee, process, context);
    process.blocks[block.0 as usize].terminator = ProcessTerminator::Match {
        scrutinee,
        arms: arms.clone(),
        fallback,
    };

    let mut tails = Vec::new();
    for (source, lowered) in statement.arms.iter().zip(&arms) {
        if let Some(tail) = lower_statements(&source.body.stmts, context, process, lowered.block) {
            tails.push(tail);
        }
    }
    if let Some(fallback) = fallback {
        tails.push(fallback);
    }
    if tails.is_empty() {
        return None;
    }

    let join = process.push_block();
    for tail in tails {
        process.blocks[tail.0 as usize].terminator = ProcessTerminator::Goto(join);
    }
    Some(join)
}

/// Convert an AST pattern into its process-IR form.
fn lower_pattern(
    pattern: &ast::Pattern,
    scrutinee_type: Option<&crate::types::Ty>,
    context: &LoweringContext<'_>,
) -> ProcessPattern {
    match pattern {
        ast::Pattern::Wildcard => ProcessPattern::Wildcard,
        ast::Pattern::Path(path) => {
            let definition = context.resolved.resolved(path.span);
            definition
                .and_then(|definition| definition_number(definition, context))
                .map_or_else(
                    || ProcessPattern::Path {
                        definition,
                        segments: path
                            .segments
                            .iter()
                            .map(|segment| segment.text.clone())
                            .collect(),
                    },
                    ProcessPattern::Number,
                )
        }
        ast::Pattern::BitPattern { text, .. } => crate::syntax::bit_pattern_mask(text).map_or_else(
            || ProcessPattern::BitPattern(text.clone()),
            |(mask, value)| ProcessPattern::BitMask { mask, value },
        ),
        ast::Pattern::Or { alts, .. } => ProcessPattern::Or(
            alts.iter()
                .map(|pattern| lower_pattern(pattern, scrutinee_type, context))
                .collect(),
        ),
        ast::Pattern::Range { lo, hi, .. } => ProcessPattern::Range {
            left: *lo,
            right: *hi,
        },
        ast::Pattern::CharLit { ch, .. } => character_number(*ch, scrutinee_type, context)
            .map_or(ProcessPattern::Char(*ch), ProcessPattern::Number),
    }
}

/// Whether a pattern matches everything, so a match needs no fall-through
/// continuation. An `Or` counts when any alternative does.
fn pattern_has_wildcard(pattern: &ast::Pattern) -> bool {
    match pattern {
        ast::Pattern::Wildcard => true,
        ast::Pattern::Or { alts, .. } => alts.iter().any(pattern_has_wildcard),
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
/// Lower a `for` loop into a dedicated header block plus body and exit
/// blocks. The header exists so the body's back-edge does not re-enter
/// instructions that ran before the loop.
fn lower_for(
    variable: &ast::Ident,
    iterable: &ast::Expr,
    body: &ast::Block,
    span: crate::diag::Span,
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let local = ProcessLocalId(process.locals.len() as u32);
    let ty = if matches!(iterable, ast::Expr::Range { .. }) {
        Some(crate::types::Ty::Integer)
    } else {
        match context.typed.expr_type(ast::expr_span(iterable)) {
            Some(crate::types::Ty::Array { elem, .. }) => Some((**elem).clone()),
            _ => None,
        }
    };
    process.locals.push(ProcessLocal {
        id: local,
        name: variable.text.clone(),
        source: context.resolved.declared(variable.span),
        span: variable.span,
        ty,
        layout: None,
    });

    let iterable = value_ref(iterable, process, context);
    // Keep the loop control on a dedicated header. Reusing `block` here makes
    // the body back-edge replay every instruction that appeared before the
    // loop in that source block.
    let header = process.push_block();
    let body_block = process.push_block();
    let exit = process.push_block();
    process.blocks[block.0 as usize].terminator = ProcessTerminator::Goto(header);
    process.blocks[header.0 as usize].terminator = ProcessTerminator::For {
        local,
        iterable,
        body: body_block,
        exit,
        span,
    };
    if let Some(tail) = lower_statements(&body.stmts, context, process, body_block) {
        process.blocks[tail.0 as usize].terminator = ProcessTerminator::Goto(header);
    }
    Some(exit)
}

/// Add a local to the process, recording its resolved declaration so that
/// equal spellings in nested scopes stay distinct.
fn process_local_layout(
    declaration: &ast::LetDecl,
    ty: Option<&crate::types::Ty>,
    context: &LoweringContext<'_>,
) -> Option<crate::ir::SourceLayout> {
    let mut layout = process_layout_for_type(ty?, declaration.span, context)?;
    let Some(ast::Type::Indexed {
        index: Some(index), ..
    }) = declaration.ty.as_ref()
    else {
        return Some(layout);
    };
    let ast::Expr::Range { lo, hi, .. } = index.as_ref() else {
        return Some(layout);
    };
    let left = crate::ir::eval_const_fns(lo, context.constant_integers, context.functions, 0)?;
    let right = crate::ir::eval_const_fns(hi, context.constant_integers, context.functions, 0)?;
    if let LayoutKind::Packed { width, range, .. } = &mut layout.kind {
        *width = u32::try_from(left.abs_diff(right).checked_add(1)?).ok()?;
        *range = Some(crate::ir::LayoutRange { left, right });
    }
    Some(layout)
}

fn push_local(
    process: &mut ProcessCfg,
    declaration: &ast::LetDecl,
    context: &LoweringContext<'_>,
) -> ProcessLocalId {
    let id = ProcessLocalId(process.locals.len() as u32);
    let ty = declaration
        .value
        .as_ref()
        .and_then(|value| context.typed.expr_type(ast::expr_span(value)))
        .filter(|ty| !matches!(ty, crate::types::Ty::Error))
        .cloned()
        .or_else(|| {
            declaration
                .ty
                .as_ref()
                .and_then(|ty| process_declared_type(ty, context))
        });
    let layout = process_local_layout(declaration, ty.as_ref(), context);
    process.locals.push(ProcessLocal {
        id,
        name: declaration.name.text.clone(),
        source: context.resolved.declared(declaration.name.span),
        span: declaration.span,
        ty,
        layout,
    });
    id
}

/// Resolve an enum-variant declaration to its elaborated discriminant while
/// the resolver is still available. Backends must not rediscover std/user enum
/// values from a `DefId`, and the compiler must not hardcode Bool or logic
/// variant numbers.
fn definition_number(
    definition: crate::resolve::DefId,
    context: &LoweringContext<'_>,
) -> Option<ProcessNumber> {
    let variant = context.resolved.def(definition)?;
    if variant.kind != crate::resolve::DefKind::EnumVariant {
        return None;
    }
    let enumeration = context.resolved.def(variant.parent?)?;
    let qualified = context.resolved.qualified_name(variant.parent?)?;
    let symbols = context
        .design
        .enum_syms
        .get(&qualified)
        .or_else(|| context.design.enum_syms.get(&enumeration.name))?;
    let discriminant = symbols
        .iter()
        .find_map(|(discriminant, symbol)| (symbol == &variant.name).then_some(*discriminant))?;
    Some(ProcessNumber::Integer(vec![discriminant]))
}

/// Resolve a context-typed character literal through the enum declaration's
/// elaborated table. A kernel `Char` remains its Unicode scalar value; a
/// library enum such as `Bit` or `Logic` uses whatever discriminant std chose.
fn character_number(
    character: char,
    ty: Option<&crate::types::Ty>,
    context: &LoweringContext<'_>,
) -> Option<ProcessNumber> {
    let crate::types::Ty::Named(definition) = ty? else {
        return None;
    };
    let enumeration = context.resolved.def(*definition)?;
    let qualified = context.resolved.qualified_name(*definition)?;
    let symbols = context
        .design
        .enum_syms
        .get(&qualified)
        .or_else(|| context.design.enum_syms.get(&enumeration.name))?;
    let quoted = format!("'{character}'");
    let discriminant = symbols.iter().find_map(|(discriminant, symbol)| {
        (symbol == &quoted || symbol == &character.to_string()).then_some(*discriminant)
    })?;
    Some(ProcessNumber::Integer(vec![discriminant]))
}

/// Resolve a function parameter or function-local value in the innermost
/// active inline. Resolver identity keeps equal spellings in nested calls and
/// modules distinct without minting synthetic AST declarations.
fn inline_bound_value(path: &ast::Path, context: &LoweringContext<'_>) -> Option<ProcessValueId> {
    if path.segments.len() == 1 && path.segments[0].text == "self" {
        return context
            .inline_self_values
            .iter()
            .rev()
            .find_map(|value| *value);
    }
    let definition = context.resolved.resolved(path.span)?;
    context
        .value_bindings
        .iter()
        .rev()
        .find_map(|bindings| bindings.get(&definition).copied())
}

fn process_type_key(ty: &crate::types::Ty, context: &LoweringContext<'_>) -> Option<String> {
    match ty {
        crate::types::Ty::Integer => Some("integer".into()),
        crate::types::Ty::Real => Some("real".into()),
        crate::types::Ty::Char => Some("Char".into()),
        crate::types::Ty::Named(definition) => context.functions.nominal_type_key(*definition),
        crate::types::Ty::Array {
            family: Some(family),
            ..
        } => Some(context.functions.canonical_type_key(family)),
        crate::types::Ty::Array { family: None, .. }
        | crate::types::Ty::Void
        | crate::types::Ty::Error => None,
    }
}

fn inline_signal_state(
    expression: &ast::Expr,
    state: ProcessSignalState,
    context: &LoweringContext<'_>,
) -> Option<ProcessValueKind> {
    let ast::Expr::Path(path) = expression else {
        return None;
    };
    let value = inline_bound_value(path, context)?;
    match &context.process_ir.values.get(value.0 as usize)?.kind {
        ProcessValueKind::Signal { signals, .. } => Some(ProcessValueKind::Signal {
            signals: signals.clone(),
            state,
        }),
        ProcessValueKind::Storage(storage) => Some(ProcessValueKind::StorageState {
            storage: *storage,
            state,
        }),
        _ => None,
    }
}

fn process_value_is_real(id: ProcessValueId, context: &LoweringContext<'_>) -> bool {
    let layout_is_real = |layout: &crate::ir::SourceLayout| {
        matches!(
            layout.kind,
            LayoutKind::Scalar {
                domain: crate::ir::ScalarDomain::Real,
                ..
            }
        )
    };
    let mut pending = vec![id];
    while let Some(id) = pending.pop() {
        let Some(value) = context.process_ir.values.get(id.0 as usize) else {
            continue;
        };
        if matches!(value.ty, Some(crate::types::Ty::Real)) {
            return true;
        }
        match &value.kind {
            ProcessValueKind::Number(ProcessNumber::Real(_)) => return true,
            ProcessValueKind::Storage(storage) | ProcessValueKind::StorageState { storage, .. } => {
                let Some(storage) = context.process_ir.storages.get(storage.0 as usize) else {
                    continue;
                };
                if matches!(storage.ty, Some(crate::types::Ty::Real))
                    || storage.layout.as_ref().is_some_and(layout_is_real)
                {
                    return true;
                }
            }
            ProcessValueKind::Local { process, local } => {
                let Some(local) = context
                    .process_ir
                    .processes
                    .get(process.0 as usize)
                    .and_then(|process| process.locals.get(local.0 as usize))
                else {
                    continue;
                };
                if matches!(local.ty, Some(crate::types::Ty::Real))
                    || local.layout.as_ref().is_some_and(layout_is_real)
                {
                    return true;
                }
            }
            ProcessValueKind::Signal { signals, .. } => {
                if signals.iter().any(|signal| {
                    context
                        .design
                        .signals
                        .get(signal.0 as usize)
                        .and_then(|signal| context.design.source_layouts.get(&signal.path))
                        .is_some_and(layout_is_real)
                }) {
                    return true;
                }
            }
            ProcessValueKind::Unary {
                operation: ProcessUnaryOp::Neg,
                operand,
            } => pending.push(*operand),
            ProcessValueKind::Binary {
                operation:
                    ProcessBinaryOp::FloatAdd
                    | ProcessBinaryOp::FloatSub
                    | ProcessBinaryOp::FloatMul
                    | ProcessBinaryOp::FloatDiv,
                ..
            } => return true,
            ProcessValueKind::Select {
                then_value,
                else_value,
                ..
            } => {
                pending.push(*then_value);
                pending.push(*else_value);
            }
            ProcessValueKind::ForeignCall {
                float_result: true, ..
            } => return true,
            ProcessValueKind::HostCall {
                operation: ProcessHostValueOp::Uniform,
                ..
            } => return true,
            _ => {}
        }
    }
    false
}

/// Lower compiler-kernel conversions before they can survive as executable
/// calls. Their resolved builtin identity, rather than the leaf spelling,
/// distinguishes them from an ordinary user function with a similar name.
/// `integer(real)` is a signed numeric conversion; the other currently
/// supported kernel crossings preserve the source bits at the target width.
fn lower_process_kernel_conversion(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let ast::Expr::Call {
        callee,
        type_args,
        args,
        bang: false,
        span,
    } = expression
    else {
        return None;
    };
    if !type_args.is_empty() || args.len() != 1 {
        return None;
    }
    let ast::Expr::Path(path) = callee.as_ref() else {
        return None;
    };
    let definition = context.resolved.resolved(path.span)?;
    let definition = context.resolved.def(definition)?;
    if definition.kind != crate::resolve::DefKind::Builtin {
        return None;
    }
    let name = definition.name.clone();
    let target = match name.as_str() {
        "integer" => crate::types::Ty::Integer,
        "Char" => crate::types::Ty::Char,
        "real" => crate::types::Ty::Real,
        _ => return None,
    };
    let operand_type = context
        .typed
        .expr_type(ast::expr_span(&args[0]))
        .filter(|ty| !matches!(ty, crate::types::Ty::Error))
        .cloned();
    let operand = value_ref_with_type(&args[0], process, context, operand_type.as_ref());
    let operand_is_real = matches!(operand_type, Some(crate::types::Ty::Real))
        || process_value_is_real(operand, context);
    let kind = if name == "integer" && operand_is_real {
        ProcessValueKind::Unary {
            operation: ProcessUnaryOp::RealToInteger,
            operand,
        }
    } else if name == "real" && !operand_is_real {
        // `real(n)`: the number n, not its bits read as an f64.
        ProcessValueKind::Unary {
            operation: ProcessUnaryOp::IntegerToReal,
            operand,
        }
    } else {
        ProcessValueKind::RawResize { operand }
    };
    let width = source_value_width(&kind, Some(&target), process, context)?;
    Some(push_value(*span, Some(target), Some(width), kind, context))
}

/// Whether a representation-preserving enum conversion is total. A nominal
/// derivation must connect the two types, and every source symbol must exist
/// in the target domain; an unrelated enum with the same width is never a
/// conversion route.
fn process_enum_conversion_is_total(design: &Design, target: &str, source: &str) -> bool {
    let ancestor = |ancestor: &str, descendant: &str| {
        let mut current = descendant;
        let mut seen = std::collections::HashSet::new();
        while let Some(base) = design.enum_bases.get(current) {
            if !seen.insert(current.to_string()) {
                return false;
            }
            if base == ancestor {
                return true;
            }
            current = base;
        }
        false
    };
    let connected = ancestor(target, source) || ancestor(source, target);
    connected
        && design
            .enum_syms
            .get(source)
            .zip(design.enum_syms.get(target))
            .is_some_and(|(source, target)| {
                source
                    .values()
                    .all(|symbol| target.values().any(|candidate| candidate == symbol))
            })
}

/// Lower a user-type conversion through the exact source-owned `From` body or
/// through a total enum derivation. The frontend has already rejected calls
/// with no route, so a miss here remains fail-closed instead of reinterpreting
/// unrelated nominal values by width.
fn lower_process_named_conversion(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    return_type: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    let ast::Expr::Call {
        callee,
        type_args,
        args,
        bang: false,
        span,
    } = expression
    else {
        return None;
    };
    if !type_args.is_empty() {
        return None;
    }
    let [argument] = args.as_slice() else {
        return None;
    };
    let ast::Expr::Path(path) = callee.as_ref() else {
        return None;
    };
    let definition = context.resolved.resolved(path.span)?;
    if !matches!(
        context.resolved.kind_of(definition),
        Some(crate::resolve::DefKind::Struct | crate::resolve::DefKind::Enum)
    ) {
        return None;
    }
    let target_type = return_type
        .filter(|ty| !matches!(ty, crate::types::Ty::Error))
        .cloned()
        .unwrap_or(crate::types::Ty::Named(definition));
    let target = process_type_key(&target_type, context)?;
    let source_type = context
        .typed
        .expr_type(ast::expr_span(argument))
        .filter(|ty| !matches!(ty, crate::types::Ty::Error))
        .cloned()?;
    let source = process_type_key(&source_type, context)?;
    let first_value = context.process_ir.values.len();
    let operand = value_ref_with_type(argument, process, context, Some(&source_type));

    if let Some(function) = context.functions.get_conversion(&target, &source) {
        let result = inline_process_function(
            function,
            None,
            &[operand],
            process,
            context,
            Some(&target_type),
        );
        if result.is_none() {
            truncate_process_values(context, first_value);
        }
        return result;
    }

    if target == source || process_enum_conversion_is_total(context.design, &target, &source) {
        let kind = ProcessValueKind::RawResize { operand };
        let width = source_value_width(&kind, Some(&target_type), process, context)?;
        return Some(push_value(
            *span,
            Some(target_type),
            Some(width),
            kind,
            context,
        ));
    }

    truncate_process_values(context, first_value);
    None
}

/// Lower a value-transparent conversion to the language's explicit raw resize
/// operation. Packed families (`unsigned[N](value)`), nominal one-field
/// newtypes (`Byte(value)`), and the family-preserving `resize(value, width)`
/// intrinsic all preserve the operand's bits while changing its declared
/// type/width; resizing itself always truncates or zero-extends.
fn lower_process_raw_resize(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    target: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    let ast::Expr::Call {
        callee,
        type_args,
        args,
        bang: false,
        span,
    } = expression
    else {
        return None;
    };
    if !type_args.is_empty() {
        return None;
    }
    if let ast::Expr::Path(path) = callee.as_ref() {
        let intrinsic_resize = path.segments.len() == 1
            && path.segments[0].text == "resize"
            && context
                .resolved
                .resolved(path.span)
                .is_none_or(|definition| {
                    context.resolved.def(definition).is_some_and(|definition| {
                        definition.kind == crate::resolve::DefKind::Builtin
                            && definition.name == "resize"
                    })
                });
        if intrinsic_resize {
            let [operand, width] = args.as_slice() else {
                return None;
            };
            let operand_type = context
                .typed
                .expr_type(ast::expr_span(operand))
                .filter(|ty| !matches!(ty, crate::types::Ty::Error))
                .cloned();
            let operand = value_ref_with_type(operand, process, context, operand_type.as_ref());
            let first_width_value = context.process_ir.values.len();
            let width =
                value_ref_with_type(width, process, context, Some(&crate::types::Ty::Integer));
            let width = arena_constant_integer(width, &context.process_ir.values)
                .and_then(|width| u32::try_from(width).ok())
                .filter(|width| *width != 0)?;
            truncate_process_values(context, first_width_value);

            let operand_type = process_value_type(operand, context);
            let target = match operand_type {
                Some(crate::types::Ty::Array { elem, family, .. }) => crate::types::Ty::Array {
                    elem,
                    family,
                    len: width,
                },
                Some(operand_type) => target.cloned().unwrap_or(operand_type),
                None => target.cloned()?,
            };
            return Some(push_value(
                *span,
                Some(target),
                Some(width),
                ProcessValueKind::RawResize { operand },
                context,
            ));
        }
    }
    if args.len() != 1 {
        return None;
    }
    let target = match callee.as_ref() {
        ast::Expr::Index { base, index, .. } => {
            let ast::Expr::Path(path) = base.as_ref() else {
                return None;
            };
            let definition = context.resolved.resolved(path.span)?;
            let family = context.resolved.qualified_name(definition)?;
            let family_known = context.design.array_element_of_family.contains_key(&family)
                || family
                    .rsplit("::")
                    .next()
                    .is_some_and(|leaf| context.design.array_element_of_family.contains_key(leaf));
            if !family_known {
                return None;
            }
            let len =
                crate::ir::eval_const_fns(index, context.constant_integers, context.functions, 0)
                    .and_then(|width| u32::try_from(width).ok())?;
            let (elem, family) = match target {
                Some(crate::types::Ty::Array {
                    elem,
                    family: Some(target_family),
                    ..
                }) => (elem.clone(), target_family.clone()),
                _ => (
                    // Packed families carry their width independently of the
                    // element type. The family identity is sufficient until
                    // typed expression metadata retains the concrete element
                    // across this source-to-Process lowering boundary.
                    Box::new(crate::types::Ty::Error),
                    family,
                ),
            };
            crate::types::Ty::Array {
                elem,
                family: Some(family),
                len,
            }
        }
        ast::Expr::Path(path) => {
            let definition = context.resolved.resolved(path.span)?;
            if context.resolved.def(definition)?.kind != crate::resolve::DefKind::Struct {
                return None;
            }
            match target {
                Some(crate::types::Ty::Named(target_definition))
                    if *target_definition == definition =>
                {
                    crate::types::Ty::Named(definition)
                }
                None | Some(crate::types::Ty::Error) => crate::types::Ty::Named(definition),
                _ => return None,
            }
        }
        _ => return None,
    };
    if !matches!(
        target,
        crate::types::Ty::Array {
            family: Some(_),
            ..
        } | crate::types::Ty::Named(_)
    ) {
        return None;
    }
    let operand_type = context
        .typed
        .expr_type(ast::expr_span(&args[0]))
        .filter(|ty| !matches!(ty, crate::types::Ty::Error))
        .cloned();
    let operand = value_ref_with_type(&args[0], process, context, operand_type.as_ref());
    let kind = ProcessValueKind::RawResize { operand };
    let width = source_value_width(&kind, Some(&target), process, context)?;
    Some(push_value(*span, Some(target), Some(width), kind, context))
}

/// Lower zero-argument type construction to the type's retained recursive
/// default rather than leaving `T()`/`T::new()` as an executable call. The
/// resolver check distinguishes constructors from ordinary zero-argument
/// functions, whose bodies still go through constant folding or call inlining.
fn lower_process_default(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    target: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    let ast::Expr::Call {
        callee,
        type_args,
        args,
        bang: false,
        span,
    } = expression
    else {
        return None;
    };
    if !type_args.is_empty() || !args.is_empty() {
        return None;
    }
    let definition = match callee.as_ref() {
        ast::Expr::Path(path) if path.segments.last()?.text == "new" => context
            .resolved
            .resolved(path.segments.get(path.segments.len().checked_sub(2)?)?.span),
        ast::Expr::Path(path) => context.resolved.resolved(path.span),
        ast::Expr::Index { base, .. } => match base.as_ref() {
            ast::Expr::Path(path) => context.resolved.resolved(path.span),
            _ => None,
        },
        _ => None,
    }?;
    let definition_kind = context.resolved.def(definition)?.kind;
    if !matches!(
        definition_kind,
        crate::resolve::DefKind::Builtin
            | crate::resolve::DefKind::Struct
            | crate::resolve::DefKind::Enum
            | crate::resolve::DefKind::TypeAlias
    ) {
        return None;
    }
    let target = target.cloned().or_else(|| {
        matches!(
            definition_kind,
            crate::resolve::DefKind::Struct | crate::resolve::DefKind::Enum
        )
        .then_some(crate::types::Ty::Named(definition))
    })?;
    if matches!(target, crate::types::Ty::Error) {
        return None;
    }
    let kind = ProcessValueKind::Default;
    let width = source_value_width(&kind, Some(&target), process, context);
    Some(push_value(*span, Some(target), width, kind, context))
}

/// Normalize a resolver-selected `extern "C"` value call into its explicit
/// scalar ABI. The source declaration supplies the linker-visible symbol and
/// parameter count; checked expression types retain aliases and constraints as
/// their kernel `integer`/`real` representation. Keeping this in Process IR
/// prevents native lowering from revisiting an extern AST declaration.
fn lower_process_foreign_call(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    return_type: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    let ast::Expr::Call {
        callee,
        type_args,
        args,
        bang: false,
        span,
    } = expression
    else {
        return None;
    };
    if !type_args.is_empty() || !matches!(callee.as_ref(), ast::Expr::Path(_)) {
        return None;
    }
    let function = context.functions.get(callee)?.clone();
    if function.body.is_some() || function.params.iter().any(|parameter| parameter.is_self) {
        return None;
    }
    let parameters = function
        .params
        .iter()
        .filter(|parameter| !parameter.is_self)
        .collect::<Vec<_>>();
    if parameters.len() != args.len() {
        return None;
    }

    let abi_kind = |declared: Option<&ast::Type>, actual: Option<&crate::types::Ty>| {
        let leaf = declared.and_then(type_leaf);
        (
            leaf == Some("real") || matches!(actual, Some(crate::types::Ty::Real)),
            leaf == Some("integer") || matches!(actual, Some(crate::types::Ty::Integer)),
        )
    };
    let mut arguments = Vec::with_capacity(args.len());
    let mut float_arguments = Vec::with_capacity(args.len());
    let mut integer_arguments = Vec::with_capacity(args.len());
    for (argument, parameter) in args.iter().zip(parameters) {
        let actual = context.typed.expr_type(ast::expr_span(argument)).cloned();
        let (float, integer) = abi_kind(parameter.ty.as_ref(), actual.as_ref());
        arguments.push(value_ref_with_type(
            argument,
            process,
            context,
            actual.as_ref(),
        ));
        float_arguments.push(float);
        integer_arguments.push(integer);
    }
    let declared_return = function.ret.as_ref();
    let (float_result, integer_result) = abi_kind(declared_return, return_type);
    let ty = return_type
        .cloned()
        .or_else(|| declared_return.and_then(|ty| declared_process_type(ty, context.resolved)));
    let kind = ProcessValueKind::ForeignCall {
        name: function.name.text.clone(),
        arguments,
        float_arguments,
        integer_arguments,
        float_result,
        integer_result,
    };
    let width = source_value_width(&kind, ty.as_ref(), process, context);
    Some(push_value(*span, ty, width, kind, context))
}

/// Normalize deterministic randomization before it can become a generic
/// source call. Resolved same-named user functions remain ordinary calls;
/// these runtime primitives themselves intentionally have no declaration.
fn lower_process_host_call(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    return_type: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    let ast::Expr::Call {
        callee,
        type_args,
        args,
        bang: false,
        span,
    } = expression
    else {
        return None;
    };
    if !type_args.is_empty() {
        return None;
    }
    let (operation, arity) = if builtin_callee_is(callee, "rand", context) {
        (ProcessHostValueOp::Random, 0)
    } else if builtin_callee_is(callee, "randint", context) {
        (ProcessHostValueOp::RandomRange, 2)
    } else if builtin_callee_is(callee, "uniform", context) {
        (ProcessHostValueOp::Uniform, 0)
    } else {
        return None;
    };
    if args.len() != arity {
        return None;
    }
    let arguments = args
        .iter()
        .map(|argument| value_ref_with_type(argument, process, context, None))
        .collect();
    let ty = return_type.cloned().or(match operation {
        ProcessHostValueOp::Uniform => Some(crate::types::Ty::Real),
        ProcessHostValueOp::Random | ProcessHostValueOp::RandomRange => {
            Some(crate::types::Ty::Integer)
        }
        ProcessHostValueOp::ReadUtf8
        | ProcessHostValueOp::ReadUtf8Fixed
        | ProcessHostValueOp::ReadBinary
        | ProcessHostValueOp::FileExists
        | ProcessHostValueOp::StringLength
        | ProcessHostValueOp::StringIndex
        | ProcessHostValueOp::StringEqualsUtf8 => return None,
    });
    let kind = ProcessValueKind::HostCall {
        operation,
        arguments,
    };
    let width = source_value_width(&kind, ty.as_ref(), process, context);
    Some(push_value(*span, ty, width, kind, context))
}

fn process_type_is_string(ty: Option<&crate::types::Ty>) -> bool {
    matches!(
        ty,
        Some(crate::types::Ty::Array {
            elem,
            family: None,
            ..
        }) if matches!(elem.as_ref(), crate::types::Ty::Char)
    )
}

/// Normalize filesystem calls while the source literal and design base
/// directory are still available. The Process value carries an absolute path;
/// the fixed runtime owns file access, UTF-8 decoding, and string lifetime.
fn lower_process_file_call(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    return_type: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    let ast::Expr::Call {
        callee,
        type_args,
        args,
        bang: false,
        span,
    } = expression
    else {
        return None;
    };
    let [ast::Expr::StrLit {
        text: path,
        span: path_span,
    }] = args.as_slice()
    else {
        return None;
    };
    let direct_string =
        matches!(type_args.as_slice(), [requested] if type_leaf(requested) == Some("string"));
    let operation = if builtin_callee_is(callee, "read", context) {
        if process_type_is_string(return_type) || return_type.is_none() && direct_string {
            match return_type {
                Some(crate::types::Ty::Array { len, .. }) if *len != 0 => {
                    ProcessHostValueOp::ReadUtf8Fixed
                }
                _ => ProcessHostValueOp::ReadUtf8,
            }
        } else if return_type.is_some() {
            ProcessHostValueOp::ReadBinary
        } else {
            return None;
        }
    } else if builtin_callee_is(callee, "exists", context) && type_args.is_empty() {
        ProcessHostValueOp::FileExists
    } else {
        return None;
    };
    let path = context
        .design
        .base_dir
        .join(path)
        .to_string_lossy()
        .into_owned();
    let path_type = crate::types::Ty::Array {
        elem: Box::new(crate::types::Ty::Char),
        family: None,
        len: u32::try_from(path.chars().count()).ok()?,
    };
    let path_kind = ProcessValueKind::String(path);
    let path_width = source_value_width(&path_kind, Some(&path_type), process, context);
    let path = push_value(*path_span, Some(path_type), path_width, path_kind, context);
    let kind = ProcessValueKind::HostCall {
        operation,
        arguments: vec![path],
    };
    let ty = return_type.cloned().or_else(|| match operation {
        ProcessHostValueOp::ReadUtf8 => Some(crate::types::Ty::Array {
            elem: Box::new(crate::types::Ty::Char),
            family: None,
            len: 0,
        }),
        ProcessHostValueOp::FileExists => bool_type(context.resolved),
        _ => None,
    });
    let width = source_value_width(&kind, ty.as_ref(), process, context);
    Some(push_value(*span, ty, width, kind, context))
}

fn process_value_is_runtime_string(id: ProcessValueId, context: &LoweringContext<'_>) -> bool {
    let Some(value) = context.process_ir.values.get(id.0 as usize) else {
        return false;
    };
    match &value.kind {
        ProcessValueKind::HostCall {
            operation: ProcessHostValueOp::ReadUtf8,
            ..
        } => true,
        ProcessValueKind::Storage(storage) => context
            .process_ir
            .storages
            .get(storage.0 as usize)
            .and_then(|storage| storage.initializer)
            .is_some_and(|initializer| process_value_is_runtime_string(initializer, context)),
        ProcessValueKind::Select {
            then_value,
            else_value,
            ..
        } => {
            process_value_is_runtime_string(*then_value, context)
                && process_value_is_runtime_string(*else_value, context)
        }
        _ => false,
    }
}

fn process_value_is_string_literal(id: ProcessValueId, context: &LoweringContext<'_>) -> bool {
    matches!(
        context
            .process_ir
            .values
            .get(id.0 as usize)
            .map(|value| &value.kind),
        Some(ProcessValueKind::String(_))
    )
}

/// Inline a pure, value-returning Siox function into the Process value arena.
/// Parameters and `let` bindings remain compile-time SSA aliases; control
/// flow becomes value-level selection, so the backend never needs an AST or a
/// resolver to execute the call.
fn inline_process_call(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    return_type: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    let ast::Expr::Call { callee, args, .. } = expression else {
        return None;
    };
    let first_value = context.process_ir.values.len();
    let (function, receiver) = match callee.as_ref() {
        ast::Expr::Field { base, field, .. } => {
            let receiver = value_ref(base, process, context);
            let owner = context
                .process_ir
                .values
                .get(receiver.0 as usize)
                .and_then(|value| value.ty.as_ref())
                .and_then(|ty| process_type_key(ty, context))?;
            (
                context.functions.get_associated(&owner, &field.text)?,
                Some(receiver),
            )
        }
        _ => (context.functions.get(callee)?, None),
    };
    let parameter_types = function
        .params
        .iter()
        .filter(|parameter| !parameter.is_self)
        .map(|parameter| {
            parameter
                .ty
                .as_ref()
                .and_then(|ty| process_declared_type(ty, context))
        })
        .collect::<Vec<_>>();
    if args.len() != parameter_types.len() {
        return None;
    }
    let arguments = args
        .iter()
        .zip(&parameter_types)
        .map(|(argument, parameter)| {
            value_ref_with_type(argument, process, context, parameter.as_ref())
        })
        .collect::<Vec<_>>();
    let result = inline_process_function(
        function,
        receiver,
        &arguments,
        process,
        context,
        return_type,
    );
    if result.is_none() {
        truncate_process_values(context, first_value);
    }
    // `y.negate().to_real()`: a method returning its receiver's type keeps
    // the receiver's format, as an operator does.
    // Its width too: `a.rem(m)` on a `signed[8]` is a `signed[8]`, so a
    // comparison on it reads `self'length` as 8 rather than the word's 64.
    if let (Some(result), Some(receiver)) = (result, receiver) {
        if returns_receiver_type(function, receiver, context) {
            if let Some(layout) = process_value_source_layout(receiver, context.process_ir).cloned()
            {
                if process_value_source_layout(result, context.process_ir).is_none() {
                    context.process_ir.value_layouts[result.0 as usize] = Some(layout);
                }
            }
            let receiver_type = context.process_ir.values[receiver.0 as usize].ty.clone();
            if matches!(receiver_type, Some(crate::types::Ty::Array { len, .. }) if len > 0) {
                return Some(narrow_to_type(result, receiver_type.as_ref(), context));
            }
        }
    }
    result
}

/// Whether `function` is declared to return the type of `receiver`.
fn returns_receiver_type(
    function: &ast::FnDecl,
    receiver: ProcessValueId,
    context: &LoweringContext<'_>,
) -> bool {
    let declared = function
        .ret
        .as_ref()
        .and_then(|ty| declared_process_type(ty, context.resolved))
        .and_then(|ty| process_type_key(&ty, context));
    let receiver = context
        .process_ir
        .values
        .get(receiver.0 as usize)
        .and_then(|value| value.ty.as_ref())
        .and_then(|ty| process_type_key(ty, context));
    declared.is_some() && declared == receiver
}

/// Inline one already-selected Siox function over already-lowered operands.
/// Calls and operators share this implementation so receiver binding,
/// overload bodies, recursion recovery, and result typing cannot drift.
fn inline_process_function(
    function: &ast::FnDecl,
    receiver: Option<ProcessValueId>,
    arguments: &[ProcessValueId],
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    return_type: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    let body = function.body.as_ref()?;
    if function.ret.is_none()
        || function.params.iter().any(|parameter| parameter.is_self) != receiver.is_some()
    {
        return None;
    }
    let parameters = function
        .params
        .iter()
        .filter(|parameter| !parameter.is_self)
        .collect::<Vec<_>>();
    if parameters.len() != arguments.len() {
        return None;
    }

    // A generic return type is the type of the argument bound to a parameter
    // of that type: `fn rem<T>(a: T, m: T) -> T` called on integers returns
    // an integer. Without it a nested generic call's result had no type, and
    // `rem(a, m) < 0` inside `mod` compared unsigned.
    let generic_name = |ty: &ast::Type| match ty {
        ast::Type::Path(path) if path.segments.len() == 1 => {
            let name = &path.segments[0].text;
            (function.generics.params.iter())
                .any(|param| &param.name.text == name)
                .then(|| name.clone())
        }
        _ => None,
    };
    let generic_argument = function
        .ret
        .as_ref()
        .and_then(generic_name)
        .and_then(|name| {
            parameters
                .iter()
                .zip(arguments)
                .find(|(parameter, _)| {
                    parameter.ty.as_ref().and_then(generic_name).as_deref() == Some(name.as_str())
                })
                .map(|(_, argument)| *argument)
        });
    let generic_return =
        generic_argument.and_then(|argument| process_value_type(argument, context));

    let mut bindings = std::collections::HashMap::new();
    for (parameter, argument) in parameters.into_iter().zip(arguments.iter().copied()) {
        let name = parameter.name.as_ref()?;
        let definition = context.resolved.declared(name.span)?;
        bindings.insert(definition, argument);
    }
    if !context.inline_functions.insert(function.span) {
        return None;
    }

    let return_type = return_type
        .filter(|ty| is_concrete_type(ty, context))
        .cloned()
        .or(generic_return)
        .or_else(|| {
            function
                .ret
                .as_ref()
                .and_then(|ty| declared_process_type(ty, context.resolved))
        });
    context.value_bindings.push(bindings);
    context.inline_self_values.push(receiver);
    context.inline_return_types.push(return_type);
    let result = inline_value_statements(&body.stmts, process, context);
    context.inline_return_types.pop();
    context.inline_self_values.pop();
    context.value_bindings.pop();
    context.inline_functions.remove(&function.span);
    // ...and that argument's format: `abs(x)` on a `ufixed[3..-4]` is one.
    if let (Some(result), Some(argument)) = (result, generic_argument) {
        inherit_receiver_layout(result, argument, context);
    }
    result
}

/// Materialize one source-array operand as element projections in its written
/// order. Array layout keeps labels and direction even though checked `Ty`
/// intentionally records only the element count.
fn process_array_elements(
    value: ProcessValueId,
    ty: &crate::types::Ty,
    span: crate::diag::Span,
    context: &mut LoweringContext<'_>,
) -> Option<Vec<ProcessValueId>> {
    let crate::types::Ty::Array {
        elem,
        len,
        family: None,
    } = ty
    else {
        return None;
    };
    let (range, element_width) = {
        let layout = process_value_source_layout(value, context.process_ir)?;
        let LayoutKind::Array {
            range: Some(range),
            element,
        } = &layout.kind
        else {
            return None;
        };
        if u32::try_from(range.len()?).ok()? != *len {
            return None;
        }
        (*range, u32::try_from(element.bit_width()?).ok()?)
    };

    let mut elements = Vec::with_capacity(usize::try_from(*len).ok()?);
    let mut label = range.left;
    for position in 0..*len {
        let index = push_value(
            span,
            Some(crate::types::Ty::Integer),
            Some(64),
            ProcessValueKind::Number(ProcessNumber::Integer(vec![label as u64])),
            context,
        );
        elements.push(push_value(
            span,
            Some((**elem).clone()),
            Some(element_width),
            ProcessValueKind::Index { base: value, index },
            context,
        ));
        if position + 1 != *len {
            label = if range.ascending() {
                label.checked_add(1)?
            } else {
                label.checked_sub(1)?
            };
        }
    }
    Some(elements)
}

/// Rebuild element-wise operator results as one canonical source array. The
/// consumer supplies its concrete recursive layout, so identical element
/// counts with different written ranges do not lose their direction here.
fn push_process_array(
    span: crate::diag::Span,
    ty: crate::types::Ty,
    elements: Vec<ProcessValueId>,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let width = elements.iter().try_fold(0u32, |total, element| {
        total.checked_add(
            context
                .process_ir
                .values
                .get(element.0 as usize)?
                .bit_width?,
        )
    })?;
    Some(push_value(
        span,
        Some(ty),
        Some(width),
        ProcessValueKind::Array(elements),
        context,
    ))
}

/// Retain a failed source-declared lowering as an explicit unsupported value
/// rather than silently falling back to a packed primitive with different
/// semantics.
fn unsupported_process_value(
    span: crate::diag::Span,
    ty: Option<&crate::types::Ty>,
    context: &mut LoweringContext<'_>,
) -> ProcessValueId {
    push_value(span, ty.cloned(), None, ProcessValueKind::Invalid, context)
}

/// Expand a source-declared blanket binary array operator by position, then
/// inline the concrete element implementation selected by its nominal type.
#[allow(clippy::too_many_arguments)]
fn inline_process_array_binary_operator(
    symbol: &str,
    lhs: &ast::Expr,
    rhs: &ast::Expr,
    left_type: &crate::types::Ty,
    right_type: &crate::types::Ty,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    return_type: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    if !context.functions.has_blanket_array_operator(symbol, 1) {
        return None;
    }
    let (
        crate::types::Ty::Array {
            elem: left_element,
            len: left_len,
            family: None,
        },
        crate::types::Ty::Array {
            elem: right_element,
            len: right_len,
            family: None,
        },
    ) = (left_type, right_type)
    else {
        return None;
    };
    if left_len != right_len {
        return None;
    }
    let owner = process_type_key(left_element, context)?;
    let input = process_type_key(right_element, context)?;
    let function = context
        .functions
        .get_binary_operator(symbol, &owner, Some(&input))?;
    let span = ast::expr_span(lhs);
    let left = value_ref_with_type(lhs, process, context, Some(left_type));
    let right = value_ref_with_type(rhs, process, context, Some(right_type));
    let left = process_array_elements(left, left_type, span, context)?;
    let right = process_array_elements(right, right_type, ast::expr_span(rhs), context)?;
    let elements = left
        .into_iter()
        .zip(right)
        .map(|(left, right)| {
            inline_process_function(
                function,
                Some(left),
                &[right],
                process,
                context,
                Some(left_element),
            )
        })
        .collect::<Option<Vec<_>>>()?;
    let result_type = return_type
        .filter(|ty| matches!(ty, crate::types::Ty::Array { family: None, .. }))
        .cloned()
        .unwrap_or_else(|| left_type.clone());
    push_process_array(span, result_type, elements, context)
}

/// Unary counterpart of [`inline_process_array_binary_operator`].
fn inline_process_array_unary_operator(
    symbol: &str,
    rhs: &ast::Expr,
    operand_type: &crate::types::Ty,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    return_type: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    if !context.functions.has_blanket_array_operator(symbol, 0) {
        return None;
    }
    let crate::types::Ty::Array {
        elem, family: None, ..
    } = operand_type
    else {
        return None;
    };
    let owner = process_type_key(elem, context)?;
    let function = context.functions.get_unary_operator(symbol, &owner)?;
    let span = ast::expr_span(rhs);
    let operand = value_ref_with_type(rhs, process, context, Some(operand_type));
    let operands = process_array_elements(operand, operand_type, span, context)?;
    let elements = operands
        .into_iter()
        .map(|operand| {
            inline_process_function(function, Some(operand), &[], process, context, Some(elem))
        })
        .collect::<Option<Vec<_>>>()?;
    let result_type = return_type
        .filter(|ty| matches!(ty, crate::types::Ty::Array { family: None, .. }))
        .cloned()
        .unwrap_or_else(|| operand_type.clone());
    push_process_array(span, result_type, elements, context)
}

/// Inline a binary operator's selected `Operator::apply` body. Symbols remain
/// frontend metadata only: a successful inline leaves ordinary Process value
/// nodes, while a recursion guard deliberately falls through to the primitive
/// node used inside std wrappers such as `unsigned + unsigned`.
fn inline_process_binary_operator(
    operator: &ast::BinOp,
    lhs: &ast::Expr,
    rhs: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    return_type: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    let left_type = operand_type(lhs, process, context)?;
    let right_type = operand_type(rhs, process, context);
    let symbol = crate::syntax::pretty::bin_op(operator);
    if let Some(value) = inline_process_comparison(
        operator,
        lhs,
        rhs,
        &left_type,
        right_type.as_ref(),
        process,
        context,
    ) {
        return Some(value);
    }
    if crate::syntax::ast::is_comparison_operator(symbol) {
        return None;
    }
    if matches!(left_type, crate::types::Ty::Array { family: None, .. })
        && right_type
            .as_ref()
            .is_some_and(|ty| matches!(ty, crate::types::Ty::Array { family: None, .. }))
        && context.functions.has_blanket_array_operator(symbol, 1)
    {
        let first_value = context.process_ir.values.len();
        let result = inline_process_array_binary_operator(
            symbol,
            lhs,
            rhs,
            &left_type,
            right_type.as_ref()?,
            process,
            context,
            return_type,
        );
        if result.is_none() {
            truncate_process_values(context, first_value);
        }
        return Some(result.unwrap_or_else(|| {
            unsupported_process_value(ast::expr_span(lhs), return_type, context)
        }));
    }
    let owner = process_type_key(&left_type, context)?;
    let input = right_type
        .as_ref()
        .and_then(|ty| process_type_key(ty, context));
    let function = context
        .functions
        .get_binary_operator(symbol, &owner, input.as_deref())?;

    let first_value = context.process_ir.values.len();
    let left = value_ref_with_type(lhs, process, context, Some(&left_type));
    // Operator selection lets a kernel integer adopt the receiver family.
    // Bind the value under that same contextual type: source implementations
    // legitimately inspect `rhs'length`, and keeping a literal/expression as
    // an unbounded integer here would make the selected `signed[N]` contract
    // disappear between overload resolution and body inlining.
    let right_context = if matches!(right_type, Some(crate::types::Ty::Integer))
        && !matches!(left_type, crate::types::Ty::Integer)
    {
        Some(&left_type)
    } else {
        right_type.as_ref().or(Some(&left_type))
    };
    let right = value_ref_with_type(rhs, process, context, right_context);
    let result = inline_process_function(
        function,
        Some(left),
        &[right],
        process,
        context,
        return_type,
    );
    if result.is_none() {
        truncate_process_values(context, first_value);
    }
    if let Some(result) = result {
        inherit_receiver_layout(result, left, context);
    }
    result
}

/// A comparison on a type with an `Eq`/`Ord` impl (spec 3.25) calls its
/// method: `a < b` is `a.lt(b)`, `a != b` is `a.ne(b)`, and so on. `None` when
/// the operand type has no such impl, or inside that method's own body, where
/// the comparison is the built-in one.
fn inline_process_comparison(
    operator: &ast::BinOp,
    lhs: &ast::Expr,
    rhs: &ast::Expr,
    left_type: &crate::types::Ty,
    right_type: Option<&crate::types::Ty>,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let symbol = crate::syntax::pretty::bin_op(operator);
    if !crate::syntax::ast::is_comparison_operator(symbol) {
        return None;
    }
    let owner = process_type_key(left_type, context)?;
    let input = right_type.and_then(|ty| process_type_key(ty, context));
    let function = context
        .functions
        .get_binary_operator(symbol, &owner, input.as_deref())?;
    if context.inline_functions.contains(&function.span) {
        return None;
    }
    let first_value = context.process_ir.values.len();
    let left = value_ref_with_type(lhs, process, context, Some(left_type));
    // A method result's checked type is unsized (`-> signed`), but its value
    // carries the receiver's sized type; a literal on the right needs that
    // width to read as the same format.
    let sized_left = context.process_ir.values[left.0 as usize].ty.clone();
    let left_type = match (left_type, &sized_left) {
        (
            crate::types::Ty::Array { len: 0, .. },
            Some(sized @ crate::types::Ty::Array { len, .. }),
        ) if *len > 0 => sized,
        _ => left_type,
    };
    let right_context = if matches!(right_type, Some(crate::types::Ty::Integer))
        && !matches!(left_type, crate::types::Ty::Integer)
    {
        Some(left_type)
    } else {
        right_type.or(Some(left_type))
    };
    let right = value_ref_with_type(rhs, process, context, right_context);
    // An operand computed wider than its type (`min - 1` on `signed[8]`)
    // wraps to the type first, as the built-in comparison would.
    let left = narrow_to_type(left, Some(left_type), context);
    let right = narrow_to_type(right, right_context, context);
    let result = inline_process_function(
        function,
        Some(left),
        &[right],
        process,
        context,
        bool_type(context.resolved).as_ref(),
    );
    if result.is_none() {
        truncate_process_values(context, first_value);
    }
    result
}

/// A computed `value` truncated to the width of its fixed-width array type;
/// a stored value is unchanged.
fn narrow_to_type(
    value: ProcessValueId,
    ty: Option<&crate::types::Ty>,
    context: &mut LoweringContext<'_>,
) -> ProcessValueId {
    let Some(ty @ crate::types::Ty::Array { len, .. }) = ty else {
        return value;
    };
    // A stored value already has its width; a computed one is evaluated in
    // whatever width its consumer asks for, so it is masked here.
    let stored = matches!(
        context
            .process_ir
            .values
            .get(value.0 as usize)
            .map(|value| &value.kind),
        Some(
            ProcessValueKind::Storage(_)
                | ProcessValueKind::StorageState { .. }
                | ProcessValueKind::Local { .. }
                | ProcessValueKind::Signal { .. }
        )
    );
    if *len == 0 || stored {
        return value;
    }
    let span = context.process_ir.values[value.0 as usize].span;
    let narrowed = push_value(
        span,
        Some(ty.clone()),
        Some(*len),
        ProcessValueKind::RawResize { operand: value },
        context,
    );
    if let Some(layout) = process_value_source_layout(value, context.process_ir).cloned() {
        context.process_ir.value_layouts[narrowed.0 as usize] = Some(layout);
    }
    narrowed
}

/// An operator that returns its receiver's own type keeps the receiver's
/// format: `a + b` on `ufixed[3..-4]` is a `ufixed[3..-4]`, so a body applied
/// to that result (`(a + b) * c`, reading `self'low`) still finds its index
/// range. Without a layout the result was a bare word with no bounds.
fn inherit_receiver_layout(
    result: ProcessValueId,
    receiver: ProcessValueId,
    context: &mut LoweringContext<'_>,
) {
    if process_value_source_layout(result, context.process_ir).is_some() {
        return;
    }
    let same_type = {
        let values = &context.process_ir.values;
        let ty = |id: ProcessValueId| values.get(id.0 as usize).and_then(|value| value.ty.clone());
        ty(result).is_some() && ty(result) == ty(receiver)
    };
    let Some(layout) = process_value_source_layout(receiver, context.process_ir).cloned() else {
        return;
    };
    if same_type {
        if let Some(slot) = context.process_ir.value_layouts.get_mut(result.0 as usize) {
            *slot = Some(layout);
        }
    }
}

/// Unary counterpart of [`inline_process_binary_operator`]. Only a concrete
/// receiver implementation is considered; plain numeric/vector primitives
/// keep their compact native Process operation when no such impl exists.
fn inline_process_unary_operator(
    operator: &ast::UnOp,
    rhs: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    return_type: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    let operand_type = operand_type(rhs, process, context)?;
    let symbol = match operator {
        ast::UnOp::Neg => "-",
        ast::UnOp::Not => "not",
    };
    if matches!(operand_type, crate::types::Ty::Array { family: None, .. })
        && context.functions.has_blanket_array_operator(symbol, 0)
    {
        let first_value = context.process_ir.values.len();
        let result = inline_process_array_unary_operator(
            symbol,
            rhs,
            &operand_type,
            process,
            context,
            return_type,
        );
        if result.is_none() {
            truncate_process_values(context, first_value);
        }
        return Some(result.unwrap_or_else(|| {
            unsupported_process_value(ast::expr_span(rhs), return_type, context)
        }));
    }
    let owner = process_type_key(&operand_type, context)?;
    let function = context.functions.get_unary_operator(symbol, &owner)?;

    let first_value = context.process_ir.values.len();
    let operand = value_ref_with_type(rhs, process, context, Some(&operand_type));
    let result =
        inline_process_function(function, Some(operand), &[], process, context, return_type);
    if result.is_none() {
        truncate_process_values(context, first_value);
    }
    // `(-y).to_real()` reads the operand's format, as a binary result does.
    if let Some(result) = result {
        inherit_receiver_layout(result, operand, context);
    }
    result
}

/// Evaluate a pure function statement sequence symbolically. `return`, local
/// aliases, and branching cover the expression-shaped Siox functions shared
/// by std and hardware lowering; other statements deliberately leave the call
/// explicit and fail closed in native Process lowering.
fn inline_value_statements(
    statements: &[Stmt],
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let (statement, rest) = statements.split_first()?;
    match statement {
        Stmt::Return {
            value: Some(value), ..
        } => {
            let return_type = context.inline_return_types.last().cloned().flatten();
            Some(value_ref_with_type(
                value,
                process,
                context,
                return_type.as_ref(),
            ))
        }
        Stmt::Let(declaration) => {
            let declared = declaration
                .ty
                .as_ref()
                .and_then(|ty| process_declared_type(ty, context));
            let value = value_ref_with_type(
                declaration.value.as_ref()?,
                process,
                context,
                declared.as_ref(),
            );
            let definition = context.resolved.declared(declaration.name.span)?;
            context.value_bindings.last_mut()?.insert(definition, value);
            inline_value_statements(rest, process, context)
        }
        Stmt::If(statement) => inline_value_if(statement, rest, process, context),
        Stmt::Match(statement) => inline_value_match(statement, rest, process, context),
        _ => None,
    }
}

/// Inline one branch in a fresh lexical binding scope, appending the source
/// continuation so a branch without an early return falls through normally.
fn inline_value_branch(
    branch: &[Stmt],
    continuation: &[Stmt],
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let mut statements = Vec::with_capacity(branch.len() + continuation.len());
    statements.extend_from_slice(branch);
    statements.extend_from_slice(continuation);
    context
        .value_bindings
        .push(std::collections::HashMap::new());
    let result = inline_value_statements(&statements, process, context);
    context.value_bindings.pop();
    result
}

/// Turn a function-body `if` into a dependency-ordered Process selection.
fn inline_value_if(
    statement: &ast::IfStmt,
    continuation: &[Stmt],
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let condition = value_ref(&statement.cond, process, context);
    let then_value = inline_value_branch(&statement.then.stmts, continuation, process, context)?;
    let else_value = match statement.else_.as_deref() {
        Some(ElseBranch::Block(block)) => {
            inline_value_branch(&block.stmts, continuation, process, context)?
        }
        Some(ElseBranch::If(inner)) => {
            let branch = [Stmt::If(inner.clone())];
            inline_value_branch(&branch, continuation, process, context)?
        }
        None => inline_value_branch(&[], continuation, process, context)?,
    };
    inline_select_value(statement.span, condition, then_value, else_value, context)
}

/// Turn a function-body `match` into first-match-priority selections. Pattern
/// decoding happens while enum identities and typed character literals are
/// still available; the resulting Process graph contains only executable
/// comparisons and selects.
fn inline_value_match(
    statement: &ast::MatchStmt,
    continuation: &[Stmt],
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let scrutinee = value_ref(&statement.scrutinee, process, context);
    let mut result = inline_value_branch(&[], continuation, process, context);
    for arm in statement.arms.iter().rev() {
        let value = inline_value_branch(&arm.body.stmts, continuation, process, context)?;
        match inline_pattern_condition(&arm.pattern, scrutinee, context)? {
            None => result = Some(value),
            Some(condition) => {
                result = Some(match result {
                    Some(fallback) => {
                        inline_select_value(arm.span, condition, value, fallback, context)?
                    }
                    None => value,
                });
            }
        }
    }
    result
}

/// `None` inside the outer option denotes a wildcard; an absent outer option
/// means the pattern cannot yet be represented directly.
fn inline_pattern_condition(
    pattern: &ast::Pattern,
    scrutinee: ProcessValueId,
    context: &mut LoweringContext<'_>,
) -> Option<Option<ProcessValueId>> {
    let (span, candidate) = match pattern {
        ast::Pattern::Wildcard => return Some(None),
        ast::Pattern::Path(path) => {
            let definition = context.resolved.resolved(path.span)?;
            let number = definition_number(definition, context)?;
            (path.span, number)
        }
        ast::Pattern::CharLit { ch, span } => {
            let ty = context
                .process_ir
                .values
                .get(scrutinee.0 as usize)?
                .ty
                .as_ref();
            let number = character_number(*ch, ty, context)?;
            (*span, number)
        }
        ast::Pattern::Or { alts, span } => {
            let mut condition = None;
            for alternative in alts {
                let Some(alternative) = inline_pattern_condition(alternative, scrutinee, context)?
                else {
                    return Some(None);
                };
                condition = Some(match condition {
                    Some(previous) => push_inline_binary(
                        *span,
                        ProcessBinaryOp::Or,
                        previous,
                        alternative,
                        Some(1),
                        context,
                    ),
                    None => alternative,
                });
            }
            return Some(condition);
        }
        ast::Pattern::BitPattern { .. } | ast::Pattern::Range { .. } => return None,
    };
    let scrutinee_node = context.process_ir.values.get(scrutinee.0 as usize)?;
    let candidate = push_value(
        span,
        scrutinee_node.ty.clone(),
        scrutinee_node.bit_width,
        ProcessValueKind::Number(candidate),
        context,
    );
    Some(Some(push_inline_binary(
        span,
        ProcessBinaryOp::Eq,
        scrutinee,
        candidate,
        Some(1),
        context,
    )))
}

/// Append a scalar binary node used by symbolic function control flow.
fn push_inline_binary(
    span: crate::diag::Span,
    operation: ProcessBinaryOp,
    left: ProcessValueId,
    right: ProcessValueId,
    width: Option<u32>,
    context: &mut LoweringContext<'_>,
) -> ProcessValueId {
    push_value(
        span,
        None,
        width,
        ProcessValueKind::Binary {
            operation,
            left,
            right,
        },
        context,
    )
}

/// Append one selection while retaining the common result shape.
fn inline_select_value(
    span: crate::diag::Span,
    condition: ProcessValueId,
    then_value: ProcessValueId,
    else_value: ProcessValueId,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let then_node = context.process_ir.values.get(then_value.0 as usize)?;
    let else_node = context.process_ir.values.get(else_value.0 as usize)?;
    let ty = then_node.ty.clone().or_else(|| else_node.ty.clone());
    let width = match (then_node.bit_width, else_node.bit_width) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    };
    Some(push_value(
        span,
        ty,
        width,
        ProcessValueKind::Select {
            condition,
            then_value,
            else_value,
        },
        context,
    ))
}

/// Convert a contextually typed string token into the packed representation of
/// a fixed digital array. Ordinary `string` values keep their runtime string
/// node; only a non-`Char` array target selects this path.
fn contextual_string_bits(
    text: &str,
    ty: Option<&crate::types::Ty>,
    process: &ProcessCfg,
    context: &LoweringContext<'_>,
) -> Option<(u32, Vec<u64>)> {
    let crate::types::Ty::Array { elem, len, family } = ty? else {
        return None;
    };
    if matches!(elem.as_ref(), crate::types::Ty::Char) {
        return None;
    }
    let characters = text.chars().collect::<Vec<_>>();
    if characters.len() != usize::try_from(*len).ok()? {
        return None;
    }

    let mut encoded = Vec::with_capacity(characters.len());
    let element_width = if family.is_some() {
        for character in characters {
            encoded.push(match character {
                '0' | 'L' => 0,
                '1' | 'H' => 1,
                // Packed numeric families carry a separate metavalue plane in
                // hardware. Process storage does not model that plane yet, so
                // do not silently collapse an unknown into an ordinary bit.
                _ => return None,
            });
        }
        1
    } else {
        let first = characters.first().copied()?;
        let number = character_number(first, Some(elem), context)?;
        let kind = ProcessValueKind::Number(number.clone());
        let width = source_value_width(&kind, Some(elem), process, context)?;
        let encode = |character| match character_number(character, Some(elem), context)? {
            ProcessNumber::Integer(words)
                if words
                    .get(1..)
                    .is_none_or(|rest| rest.iter().all(|word| *word == 0)) =>
            {
                Some(words.first().copied().unwrap_or(0))
            }
            ProcessNumber::Integer(_) | ProcessNumber::Real(_) => None,
        };
        encoded.push(encode(first)?);
        for character in characters.into_iter().skip(1) {
            encoded.push(encode(character)?);
        }
        width
    };

    let width = element_width.checked_mul(*len)?;
    let mut words = vec![0u64; usize::try_from(width.div_ceil(64)).ok()?];
    for (position, value) in encoded.into_iter().enumerate() {
        let position = u32::try_from(position).ok()?;
        let element = if family.is_some() {
            len.checked_sub(position.checked_add(1)?)?
        } else {
            position
        };
        let offset = element.checked_mul(element_width)?;
        for bit in 0..element_width.min(64) {
            if value & (1u64 << bit) != 0 {
                let absolute = offset.checked_add(bit)?;
                words[usize::try_from(absolute / 64).ok()?] |= 1u64 << (absolute % 64);
            }
        }
    }
    Some((width, words))
}

/// Lower an expression recursively into the process operand arena. Children
/// are inserted before their parent, so ids form a directly executable DAG.
fn value_ref(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> crate::ir::ProcessValueId {
    value_ref_with_type(expression, process, context, None)
}

/// Lower one value with an optional contextual type for its root. Constant
/// aliases use the type of the path being read: a declaration such as
/// `const HIGH: Bit = '1'` must remain the `Bit` discriminant rather than the
/// Unicode code point of a standalone `Char` expression.
fn value_ref_with_type(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    contextual_type: Option<&crate::types::Ty>,
) -> crate::ir::ProcessValueId {
    let value = value_ref_with_type_inner(expression, process, context, contextual_type);
    // A kernel integer widens into `real` by value (language §3.17's one
    // implicit promotion): convert it, rather than reading its bits as f64.
    let integer = context
        .typed
        .expr_type(ast::expr_span(expression))
        .is_some_and(|ty| matches!(ty, crate::types::Ty::Integer));
    if integer && matches!(contextual_type, Some(crate::types::Ty::Real)) {
        return promote_to_real(value, ast::expr_span(expression), context);
    }
    value
}

/// `value` as a real: unchanged when it already is one, else converted from
/// the signed integer it holds.
fn promote_to_real(
    value: ProcessValueId,
    span: crate::diag::Span,
    context: &mut LoweringContext<'_>,
) -> ProcessValueId {
    if process_value_is_real(value, context) {
        return value;
    }
    push_value(
        span,
        Some(crate::types::Ty::Real),
        Some(64),
        ProcessValueKind::Unary {
            operation: ProcessUnaryOp::IntegerToReal,
            operand: value,
        },
        context,
    )
}

fn value_ref_with_type_inner(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    contextual_type: Option<&crate::types::Ty>,
) -> crate::ir::ProcessValueId {
    let span = ast::expr_span(expression);
    let inferred = context
        .typed
        .expr_type(span)
        .filter(|ty| !matches!(ty, crate::types::Ty::Error))
        .cloned();
    let contextual = contextual_type
        .filter(|ty| !matches!(ty, crate::types::Ty::Error))
        .cloned();
    // A reference owns the type of the declaration it names. Assignment or
    // argument context may coerce that value at its use site, but must not
    // rewrite a loop cursor/local from `integer` into the destination type.
    // Literals and constructors still prefer their surrounding context.
    let mut ty = if matches!(expression, ast::Expr::Path(_)) {
        inferred.or(contextual)
    } else {
        contextual.or(inferred)
    }
    .or_else(|| {
        let ast::Expr::Call { callee, .. } = expression else {
            return None;
        };
        context
            .functions
            .get(callee)?
            .ret
            .as_ref()
            .and_then(|ty| declared_process_type(ty, context.resolved))
    })
    .or_else(|| {
        let ast::Expr::Path(path) = expression else {
            return None;
        };
        let definition = context.resolved.resolved(path.span)?;
        enum_variant_type(definition, context.resolved)
    });

    if let ast::Expr::Path(path) = expression {
        if let Some(value) = inline_bound_value(path, context) {
            return value;
        }
    }

    // Constants are aliases for their initializer values, not runtime
    // storage. Inline them before constructing the surrounding node so the
    // Process IR contains only executable value forms. The stack is merely
    // best-effort recovery for a rejected constant cycle.
    if let ast::Expr::Path(path) = expression {
        if let Some(definition) = context.resolved.resolved(path.span) {
            if let Some(initializer) = context.constants.get(&definition).copied() {
                if context.constant_stack.insert(definition) {
                    let value = value_ref_with_type(initializer, process, context, ty.as_ref());
                    context.constant_stack.remove(&definition);
                    return value;
                }
            }
        }
    }

    if matches!(expression, ast::Expr::Call { .. }) {
        if let Some(value) =
            crate::ir::eval_const_fns(expression, context.constant_integers, context.functions, 0)
        {
            let number = if matches!(ty, Some(crate::types::Ty::Real)) {
                ProcessNumber::Real((value as f64).to_bits())
            } else {
                ProcessNumber::Integer(vec![value as u64])
            };
            let kind = ProcessValueKind::Number(number);
            let width = source_value_width(&kind, ty.as_ref(), process, context);
            return push_value(span, ty, width, kind, context);
        }
        if let Some(value) = lower_process_kernel_conversion(expression, process, context) {
            return value;
        }
        if let Some(value) =
            lower_process_named_conversion(expression, process, context, ty.as_ref())
        {
            return value;
        }
        if let Some(value) = lower_process_raw_resize(expression, process, context, ty.as_ref()) {
            return value;
        }
        if let Some(value) = lower_process_default(expression, process, context, ty.as_ref()) {
            return value;
        }
        if let Some(value) = lower_process_file_call(expression, process, context, ty.as_ref()) {
            return value;
        }
        if let Some(value) = lower_process_host_call(expression, process, context, ty.as_ref()) {
            return value;
        }
        if let Some(value) = lower_process_foreign_call(expression, process, context, ty.as_ref()) {
            return value;
        }
        if let Some(value) = inline_process_call(expression, process, context, ty.as_ref()) {
            return value;
        }
    }

    // Resolve operator dispatch while declarations and checked nominal types
    // are still available. Backends receive only the inlined semantic value
    // graph; an arbitrary source symbol must never become an LLVM opcode.
    match expression {
        ast::Expr::Binary { op, lhs, rhs, .. } => {
            if let Some(value) =
                inline_process_binary_operator(op, lhs, rhs, process, context, ty.as_ref())
            {
                return value;
            }
        }
        ast::Expr::Unary { op, rhs, .. } => {
            if let Some(value) =
                inline_process_unary_operator(op, rhs, process, context, ty.as_ref())
            {
                return value;
            }
        }
        _ => {}
    }

    let kind = match expression {
        ast::Expr::Path(path) => {
            if let Some(local) = process_local(path, process, context.resolved) {
                ProcessValueKind::Local {
                    process: process.id,
                    local,
                }
            } else if let Some(signals) = signal_reference(expression, process, context) {
                ProcessValueKind::Signal {
                    signals,
                    state: ProcessSignalState::Current,
                }
            } else if let Some(storage) = testbench_storage(path, process.owner, context) {
                ProcessValueKind::Storage(storage)
            } else if let Some(definition) = context.resolved.resolved(path.span) {
                match definition_number(definition, context) {
                    Some(number) => ProcessValueKind::Number(number),
                    None => ProcessValueKind::Definition(definition),
                }
            } else {
                ProcessValueKind::Intrinsic(path_name(path))
            }
        }
        ast::Expr::Int { text, .. } => ProcessValueKind::Number(parse_number(text, ty.as_ref())),
        ast::Expr::SuffixLit { text, suffix, .. } => {
            match normalized_suffix(text, &suffix.text, context) {
                Some(number) => ProcessValueKind::Number(number),
                None => ProcessValueKind::Suffixed {
                    number: parse_number(text, ty.as_ref()),
                    suffix: suffix.text.clone(),
                },
            }
        }
        ast::Expr::BitStrLit { base, digits, .. } => {
            let radix = crate::syntax::radix_of(*base);
            let width = crate::syntax::radix_digits(digits)
                .count()
                .saturating_mul(radix.ilog2() as usize)
                .try_into()
                .unwrap_or(u32::MAX);
            ProcessValueKind::BitString {
                width,
                words: parse_digits_words(digits, radix),
            }
        }
        ast::Expr::CharLit { ch, .. } => match character_number(*ch, ty.as_ref(), context) {
            Some(number) => ProcessValueKind::Number(number),
            None => ProcessValueKind::Char(*ch),
        },
        ast::Expr::StrLit { text, .. } => {
            match contextual_string_bits(text, ty.as_ref(), process, context) {
                Some((width, words)) => ProcessValueKind::BitString { width, words },
                None => ProcessValueKind::String(text.clone()),
            }
        }
        ast::Expr::Field { base, field, .. } => {
            if let Some(signals) = signal_reference(expression, process, context) {
                ProcessValueKind::Signal {
                    signals,
                    state: ProcessSignalState::Current,
                }
            } else {
                let base = value_ref(base, process, context);
                ProcessValueKind::Field {
                    base,
                    field: field.text.clone(),
                }
            }
        }
        ast::Expr::SysAttr { base, attr, .. } => {
            let state = match attr.text.as_str() {
                "old" => Some(ProcessSignalState::Old),
                "event" => Some(ProcessSignalState::Event),
                _ => None,
            };
            if let Some(kind) = state
                .and_then(|state| inline_signal_state(base, state, context))
                .or_else(|| {
                    let ast::Expr::Path(path) = base.as_ref() else {
                        return None;
                    };
                    state
                        .zip(testbench_storage(path, process.owner, context))
                        .map(|(state, storage)| ProcessValueKind::StorageState { storage, state })
                })
                .or_else(|| {
                    state
                        .zip(signal_reference(base, process, context))
                        .map(|(state, signals)| ProcessValueKind::Signal { signals, state })
                })
            {
                kind
            } else {
                let base = value_ref(base, process, context);
                if attr.text == "length" && process_value_is_runtime_string(base, context) {
                    ProcessValueKind::HostCall {
                        operation: ProcessHostValueOp::StringLength,
                        arguments: vec![base],
                    }
                } else {
                    ProcessValueKind::Attribute {
                        base,
                        attribute: attr.text.clone(),
                    }
                }
            }
        }
        ast::Expr::Index { base, index, .. } => {
            if let Some(signals) = signal_reference(expression, process, context) {
                ProcessValueKind::Signal {
                    signals,
                    state: ProcessSignalState::Current,
                }
            } else {
                let base = value_ref(base, process, context);
                if process_value_is_runtime_string(base, context) {
                    let index_span = ast::expr_span(index);
                    let index = value_ref_with_type(
                        index,
                        process,
                        context,
                        Some(&crate::types::Ty::Integer),
                    );
                    let kind = ProcessValueKind::HostCall {
                        operation: ProcessHostValueOp::StringIndex,
                        arguments: vec![base, index],
                    };
                    let ty = ty
                        .filter(|ty| !matches!(ty, crate::types::Ty::Error))
                        .or(Some(crate::types::Ty::Char));
                    let width = source_value_width(&kind, ty.as_ref(), process, context);
                    return push_value(index_span, ty, width, kind, context);
                }
                let base_layout = process_value_source_layout(base, context.process_ir)
                    .cloned()
                    .or_else(|| {
                        let ProcessValueKind::Local {
                            process: owner,
                            local,
                        } = &context.process_ir.values.get(base.0 as usize)?.kind
                        else {
                            return None;
                        };
                        (*owner == process.id)
                            .then(|| process.locals.get(local.0 as usize)?.layout.clone())?
                    });
                if let Some((left, right)) = base_layout
                    .as_ref()
                    .and_then(|layout| packed_slice_bounds(index, layout, context))
                {
                    if left == right {
                        let index_span = ast::expr_span(index);
                        let index = push_value(
                            index_span,
                            Some(crate::types::Ty::Integer),
                            Some(64),
                            ProcessValueKind::Number(ProcessNumber::Integer(vec![left as u64])),
                            context,
                        );
                        let range = base_layout
                            .as_ref()
                            .and_then(crate::ir::SourceLayout::index_range);
                        let index = range.map_or(index, |range| {
                            checked_process_index(index, index_span, range, context)
                        });
                        let kind = ProcessValueKind::Index { base, index };
                        let ty = ty
                            .filter(|ty| !matches!(ty, crate::types::Ty::Error))
                            .or_else(|| process_index_type(base, context));
                        let width = source_value_width(&kind, ty.as_ref(), process, context);
                        return push_value(span, ty, width, kind, context);
                    }
                    let kind = ProcessValueKind::PackedSlice { base, left, right };
                    let width = left
                        .abs_diff(right)
                        .checked_add(1)
                        .and_then(|width| u32::try_from(width).ok());
                    let id = push_value(span, ty, width, kind, context);
                    context.process_ir.value_layouts[id.0 as usize] = base_layout
                        .as_ref()
                        .and_then(|layout| packed_slice_layout(layout, left, right, span));
                    return id;
                }
                let index_span = ast::expr_span(index);
                let index = value_ref(index, process, context);
                let packed = base_layout
                    .as_ref()
                    .is_some_and(|layout| matches!(&layout.kind, LayoutKind::Packed { .. }));
                let range = base_layout
                    .as_ref()
                    .and_then(crate::ir::SourceLayout::index_range);
                let index = range.map_or(index, |range| {
                    checked_process_index(index, index_span, range, context)
                });
                let kind = ProcessValueKind::Index { base, index };
                let recovered = process_index_type(base, context);
                let ty = ty
                    .filter(|ty| !matches!(ty, crate::types::Ty::Error))
                    .or(recovered);
                let width = if packed {
                    ty.as_ref()
                        .and_then(crate::types::Ty::bit_width)
                        .or(Some(1))
                } else {
                    source_value_width(&kind, ty.as_ref(), process, context)
                };
                return push_value(span, ty, width, kind, context);
            }
        }
        ast::Expr::Range { lo, hi, .. } => ProcessValueKind::Range {
            left: Some(value_ref(lo, process, context)),
            right: Some(value_ref(hi, process, context)),
        },
        ast::Expr::PartialRange { lo, hi, .. } => ProcessValueKind::Range {
            left: lo
                .as_deref()
                .map(|bound| value_ref(bound, process, context)),
            right: hi
                .as_deref()
                .map(|bound| value_ref(bound, process, context)),
        },
        ast::Expr::Unary { op, rhs, .. } => ProcessValueKind::Unary {
            operation: match op {
                ast::UnOp::Neg => ProcessUnaryOp::Neg,
                ast::UnOp::Not => ProcessUnaryOp::Not,
            },
            operand: value_ref(rhs, process, context),
        },
        ast::Expr::Binary { op, lhs, rhs, .. } => {
            // Character literals are context-typed enum values. Type checking
            // records the counterpart but deliberately keeps the literal's
            // standalone `Char` identity, so retain the counterpart here
            // before its declaration identity disappears.
            let mut left_type = operand_type(lhs, process, context);
            let mut right_type = operand_type(rhs, process, context);
            let (left, right) = if matches!(left_type, Some(crate::types::Ty::Real))
                || matches!(right_type, Some(crate::types::Ty::Real))
            {
                // Integer literals in a real arithmetic/comparison context
                // are real values, not integer bit patterns. Paths retain
                // their declaration type inside `value_ref_with_type`, while
                // literals and aggregate projections adopt this context.
                let real = crate::types::Ty::Real;
                (
                    value_ref_with_type(lhs, process, context, Some(&real)),
                    value_ref_with_type(rhs, process, context, Some(&real)),
                )
            } else if matches!(rhs.as_ref(), ast::Expr::CharLit { .. }) {
                let left = value_ref_with_type(lhs, process, context, None);
                left_type = left_type.or_else(|| process_value_type(left, context));
                let right = value_ref_with_type(rhs, process, context, left_type.as_ref());
                (left, right)
            } else if matches!(lhs.as_ref(), ast::Expr::CharLit { .. }) {
                let right = value_ref_with_type(rhs, process, context, None);
                right_type = right_type.or_else(|| process_value_type(right, context));
                let left = value_ref_with_type(lhs, process, context, right_type.as_ref());
                (left, right)
            } else {
                (
                    value_ref_with_type(lhs, process, context, None),
                    value_ref_with_type(rhs, process, context, None),
                )
            };
            if matches!(op, ast::BinOp::Eq | ast::BinOp::Ne) {
                let operands = if process_value_is_runtime_string(left, context)
                    && process_value_is_string_literal(right, context)
                {
                    Some((left, right))
                } else if process_value_is_runtime_string(right, context)
                    && process_value_is_string_literal(left, context)
                {
                    Some((right, left))
                } else {
                    None
                };
                if let Some((handle, literal)) = operands {
                    let bool_type = ty.clone();
                    let equals = push_value(
                        span,
                        bool_type.clone(),
                        Some(1),
                        ProcessValueKind::HostCall {
                            operation: ProcessHostValueOp::StringEqualsUtf8,
                            arguments: vec![handle, literal],
                        },
                        context,
                    );
                    if matches!(op, ast::BinOp::Eq) {
                        return equals;
                    }
                    return push_value(
                        span,
                        bool_type,
                        Some(1),
                        ProcessValueKind::Unary {
                            operation: ProcessUnaryOp::Not,
                            operand: equals,
                        },
                        context,
                    );
                }
            }
            // An operand that is a real, by its checked type or its value
            // (`real(n)`), makes the operation a float one; an integer beside
            // it is then promoted by value.
            let real_operand =
                process_value_is_real(left, context) || process_value_is_real(right, context);
            let real_type = Some(crate::types::Ty::Real);
            let operation = if real_operand {
                lower_binary_operator(op, real_type.as_ref(), right_type.as_ref())
            } else {
                lower_binary_operator(op, left_type.as_ref(), right_type.as_ref())
            };
            let (left, right) = if is_float_operation(&operation) {
                (
                    promote_to_real(left, span, context),
                    promote_to_real(right, span, context),
                )
            } else {
                (left, right)
            };
            ProcessValueKind::Binary {
                operation,
                left,
                right,
            }
        }
        ast::Expr::IfExpr {
            cond, then, els, ..
        } => ProcessValueKind::Select {
            condition: value_ref(cond, process, context),
            then_value: value_ref(then, process, context),
            else_value: value_ref(els, process, context),
        },
        ast::Expr::Match {
            scrutinee, arms, ..
        } => {
            let scrutinee_type = context.typed.expr_type(ast::expr_span(scrutinee)).cloned();
            let scrutinee = value_ref(scrutinee, process, context);
            let arms = arms
                .iter()
                .map(|arm| {
                    let value = match arm.value_expr() {
                        Some(value) => value_ref(value, process, context),
                        None => missing_value(arm.span, context),
                    };
                    ProcessValueMatchArm {
                        pattern: lower_pattern(&arm.pattern, scrutinee_type.as_ref(), context),
                        value,
                        span: arm.span,
                    }
                })
                .collect();
            ProcessValueKind::Match { scrutinee, arms }
        }
        ast::Expr::Call {
            callee,
            type_args,
            args,
            bang,
            ..
        } => {
            let callee = value_ref(callee, process, context);
            let arguments = args
                .iter()
                .map(|argument| value_ref(argument, process, context))
                .collect();
            let type_arguments = if type_args.is_empty() {
                Vec::new()
            } else {
                // Explicit type arguments are currently accepted only by
                // `read<T>`, whose expression type is exactly `T`.
                ty.iter().cloned().collect()
            };
            ProcessValueKind::Call {
                callee,
                type_arguments,
                arguments,
                bang: *bang,
            }
        }
        ast::Expr::Construct { args, spread, .. } => {
            let layout_fields = ty
                .as_ref()
                .and_then(|ty| process_aggregate_layout_for_type(ty, span, context))
                .and_then(|layout| match layout.kind {
                    LayoutKind::Struct { fields, .. } => Some(fields),
                    _ => None,
                });
            let mut positional = 0usize;
            let fields = args
                .iter()
                .map(|field| {
                    let field_index = match &field.field {
                        Some(name) => layout_fields.as_ref().and_then(|fields| {
                            fields
                                .iter()
                                .position(|candidate| candidate.name == name.text)
                        }),
                        None => {
                            let index = positional;
                            positional = positional.saturating_add(1);
                            Some(index)
                        }
                    };
                    let field_type = field_index
                        .and_then(|index| layout_fields.as_ref()?.get(index))
                        .and_then(|field| {
                            process_type_from_layout(&field.layout, context.resolved)
                        });
                    ProcessAggregateField {
                        name: field.field.as_ref().map(|name| name.text.clone()),
                        value: field.value.as_ref().map(|value| {
                            value_ref_with_type(value, process, context, field_type.as_ref())
                        }),
                        span: field.span,
                    }
                })
                .collect();
            let spread = spread
                .as_deref()
                .map(|value| value_ref_with_type(value, process, context, ty.as_ref()));
            ProcessValueKind::Construct {
                ty: ty.clone(),
                fields,
                spread,
            }
        }
        ast::Expr::Concat { parts, .. }
            if matches!(
                ty.as_ref(),
                Some(crate::types::Ty::Named(definition))
                    if context.resolved.kind_of(*definition)
                        == Some(crate::resolve::DefKind::Struct)
            ) =>
        {
            let layout_fields = ty
                .as_ref()
                .and_then(|ty| process_aggregate_layout_for_type(ty, span, context))
                .and_then(|layout| match layout.kind {
                    LayoutKind::Struct { fields, .. } => Some(fields),
                    _ => None,
                });
            ProcessValueKind::Construct {
                ty: ty.clone(),
                fields: parts
                    .iter()
                    .enumerate()
                    .map(|(index, part)| {
                        let field_type = layout_fields
                            .as_ref()
                            .and_then(|fields| fields.get(index))
                            .and_then(|field| {
                                process_type_from_layout(&field.layout, context.resolved)
                            });
                        ProcessAggregateField {
                            name: None,
                            value: Some(value_ref_with_type(
                                part,
                                process,
                                context,
                                field_type.as_ref(),
                            )),
                            span: ast::expr_span(part),
                        }
                    })
                    .collect(),
                spread: None,
            }
        }
        ast::Expr::Concat { parts, .. } => ProcessValueKind::Concat(
            parts
                .iter()
                .map(|part| value_ref(part, process, context))
                .collect(),
        ),
        ast::Expr::Array { elems, .. } => {
            let element_type = match ty.as_ref() {
                Some(crate::types::Ty::Array { elem, .. }) => Some(elem.as_ref().clone()),
                _ => None,
            };
            ProcessValueKind::Array(
                elems
                    .iter()
                    .map(|element| {
                        value_ref_with_type(element, process, context, element_type.as_ref())
                    })
                    .collect(),
            )
        }
    };

    ty = ty.or_else(|| process_kind_type(&kind, context));
    let width = source_value_width(&kind, ty.as_ref(), process, context);
    push_value(span, ty, width, kind, context)
}

/// Insert one already-lowered value node.
fn truncate_process_values(context: &mut LoweringContext<'_>, length: usize) {
    context.process_ir.values.truncate(length);
    context.process_ir.value_layouts.truncate(length);
}

fn push_value(
    span: crate::diag::Span,
    ty: Option<crate::types::Ty>,
    width: Option<u32>,
    kind: ProcessValueKind,
    context: &mut LoweringContext<'_>,
) -> crate::ir::ProcessValueId {
    let id = crate::ir::ProcessValueId(context.process_ir.values.len() as u32);
    // Places and projections already have a declaration-owned layout whose
    // labels and direction are more precise than `Ty` (which stores only an
    // array length). Retain a type-derived layout only for values that own
    // their aggregate representation; otherwise `Bit[3..0]` would silently
    // become `Bit[0..3]` merely because it was read into the arena.
    let layout = match &kind {
        // A default needs its scalar nominal identity too: a derived enum's
        // first discriminant may be nonzero and can collide by leaf name with
        // another module's enum. LLVM cannot recover that from width alone.
        ProcessValueKind::Default => ty
            .as_ref()
            .and_then(|ty| process_layout_for_type(ty, span, context)),
        ProcessValueKind::Array(_)
        | ProcessValueKind::Construct { .. }
        | ProcessValueKind::String(_) => ty
            .as_ref()
            .and_then(|ty| process_aggregate_layout_for_type(ty, span, context)),
        _ => None,
    };
    let width = width.or_else(|| {
        layout
            .as_ref()
            .and_then(|layout| layout.bit_width()?.try_into().ok())
    });
    context.process_ir.value_layouts.resize(id.0 as usize, None);
    context.process_ir.value_layouts.push(layout);
    context.process_ir.values.push(ProcessValue {
        span,
        ty,
        bit_width: width,
        kind,
    });
    id
}

/// Packed width known at the typed source-lowering boundary. Composite
/// runtime values retain their recursive layout elsewhere; this records only
/// the scalar width a direct LLVM operation may rely on.
fn source_value_width(
    kind: &ProcessValueKind,
    ty: Option<&crate::types::Ty>,
    process: &ProcessCfg,
    context: &LoweringContext<'_>,
) -> Option<u32> {
    let runtime_handle_width = |layout: Option<&crate::ir::SourceLayout>| {
        matches!(
            layout.map(|layout| &layout.kind),
            Some(LayoutKind::Array {
                range: None,
                element,
            }) if matches!(
                element.kind,
                LayoutKind::Scalar {
                    domain: crate::ir::ScalarDomain::Character,
                    ..
                }
            )
        )
        .then_some(64)
    };
    let value_width = |id: &ProcessValueId| context.process_ir.values.get(id.0 as usize)?.bit_width;
    let typed_width = |ty: &crate::types::Ty| {
        ty.bit_width()
            .or_else(|| {
                let crate::types::Ty::Named(definition) = ty else {
                    return None;
                };
                let qualified = context.resolved.qualified_name(*definition)?;
                let symbols = context.design.enum_syms.get(&qualified).or_else(|| {
                    qualified
                        .rsplit("::")
                        .next()
                        .and_then(|name| context.design.enum_syms.get(name))
                })?;
                let highest = symbols.keys().copied().max().unwrap_or(0);
                Some((u64::BITS - highest.leading_zeros()).max(1))
            })
            .or_else(|| {
                let mut widths = context
                    .process_ir
                    .storages
                    .iter()
                    .filter(|storage| storage.ty.as_ref() == Some(ty))
                    .filter_map(|storage| storage.layout.as_ref())
                    .chain(
                        process
                            .locals
                            .iter()
                            .filter(|local| local.ty.as_ref() == Some(ty))
                            .filter_map(|local| local.layout.as_ref()),
                    )
                    .filter_map(|layout| layout.bit_width()?.try_into().ok());
                let first = widths.next()?;
                widths.all(|width| width == first).then_some(first)
            })
    };
    // A place is represented by its declaration-owned frame, not by the
    // kernel type's ordinary expression width. Consumers widen constrained
    // integers through `process_value_at`; recording them as i64 here would
    // make the place disagree with the storage/local object that owns it.
    match kind {
        ProcessValueKind::Local { local, .. } => {
            let local = process.locals.get(local.0 as usize)?;
            return local
                .layout
                .as_ref()
                .and_then(|layout| layout.bit_width()?.try_into().ok())
                .or_else(|| runtime_handle_width(local.layout.as_ref()))
                .or_else(|| local.ty.as_ref().and_then(typed_width))
                .filter(|width| *width != 0);
        }
        ProcessValueKind::Storage(storage) => {
            let storage = context.process_ir.storages.get(storage.0 as usize)?;
            return storage
                .layout
                .as_ref()
                .and_then(|layout| layout.bit_width()?.try_into().ok())
                // A named alias may deliberately retain an opaque source
                // layout while the checked storage type already names its
                // concrete scalar representation. Prefer any declared layout
                // (especially constrained integers), then use that terminal
                // type only when the layout carries no width of its own.
                .or_else(|| runtime_handle_width(storage.layout.as_ref()))
                .or_else(|| storage.ty.as_ref().and_then(typed_width))
                .filter(|width| *width != 0);
        }
        ProcessValueKind::StorageState {
            state: ProcessSignalState::Event,
            ..
        } => return Some(1),
        ProcessValueKind::StorageState { storage, .. } => {
            let storage = context.process_ir.storages.get(storage.0 as usize)?;
            return storage
                .layout
                .as_ref()
                .and_then(|layout| layout.bit_width()?.try_into().ok())
                .or_else(|| runtime_handle_width(storage.layout.as_ref()))
                .or_else(|| storage.ty.as_ref().and_then(typed_width))
                .filter(|width| *width != 0);
        }
        _ => {}
    }
    if let ProcessValueKind::Number(ProcessNumber::Integer(words)) = kind {
        let natural = integer_words_width(words)?;
        return Some(
            ty.and_then(&typed_width)
                .map_or(natural, |contextual| natural.max(contextual)),
        );
    }
    if let Some(contextual) = ty.and_then(&typed_width).filter(|width| *width != 0) {
        // `integer` is mathematically unbounded even though its ordinary ABI
        // floor is one word. Preserve a wider operand through value-producing
        // expressions so a multiword literal is not truncated merely because
        // the checker correctly names the result `integer`. Fixed-width
        // families and explicit `integer(value)` conversions deliberately do
        // not take this path.
        let natural = if matches!(ty, Some(crate::types::Ty::Integer)) {
            match kind {
                ProcessValueKind::Unary {
                    operation: ProcessUnaryOp::Neg,
                    operand,
                } => value_width(operand),
                ProcessValueKind::Binary {
                    operation:
                        ProcessBinaryOp::Add
                        | ProcessBinaryOp::Sub
                        | ProcessBinaryOp::Mul
                        | ProcessBinaryOp::Div
                        | ProcessBinaryOp::SignedAdd
                        | ProcessBinaryOp::SignedSub
                        | ProcessBinaryOp::SignedMul
                        | ProcessBinaryOp::SignedDiv
                        | ProcessBinaryOp::Shr
                        | ProcessBinaryOp::ArithmeticShr,
                    left,
                    right,
                } => Some(value_width(left)?.max(value_width(right)?)),
                ProcessValueKind::Binary {
                    operation: ProcessBinaryOp::Shl,
                    left,
                    right,
                } => shifted_width(value_width(left)?, *right, context),
                ProcessValueKind::Select {
                    then_value,
                    else_value,
                    ..
                } => Some(value_width(then_value)?.max(value_width(else_value)?)),
                _ => None,
            }
        } else {
            None
        };
        return Some(natural.map_or(contextual, |natural| contextual.max(natural)));
    }
    let width = match kind {
        ProcessValueKind::Number(ProcessNumber::Integer(words)) => integer_words_width(words),
        ProcessValueKind::Number(ProcessNumber::Real(_))
        | ProcessValueKind::ForeignCall { .. }
        | ProcessValueKind::HostCall { .. } => Some(64),
        ProcessValueKind::Suffixed { number, .. } => match number {
            ProcessNumber::Integer(_) | ProcessNumber::Real(_) => Some(64),
        },
        ProcessValueKind::BitString { width, .. } => Some(*width),
        ProcessValueKind::Char(_) => Some(32),
        ProcessValueKind::String(value) => {
            u32::try_from(value.chars().count()).ok()?.checked_mul(32)
        }
        ProcessValueKind::Local { .. }
        | ProcessValueKind::Storage(_)
        | ProcessValueKind::StorageState { .. } => unreachable!("places return above"),
        ProcessValueKind::Signal {
            state: ProcessSignalState::Event,
            ..
        } => Some(1),
        ProcessValueKind::Signal { signals, .. } => {
            signals.iter().try_fold(0u32, |total, signal| {
                total.checked_add(context.design.signal_width(*signal)?)
            })
        }
        ProcessValueKind::BitSlice { high, low, .. } => high.checked_sub(*low)?.checked_add(1),
        ProcessValueKind::PackedSlice { left, right, .. } => left
            .abs_diff(*right)
            .checked_add(1)
            .and_then(|width| u32::try_from(width).ok()),
        ProcessValueKind::CheckedIndex { index, .. } => value_width(index),
        ProcessValueKind::TableLookup { table, .. } => context
            .design
            .lookup_tables
            .get(table.0)
            .map(|table| table.element_width),
        ProcessValueKind::Unary { operation, operand } => match operation {
            ProcessUnaryOp::RealToInteger | ProcessUnaryOp::IntegerToReal => Some(64),
            ProcessUnaryOp::Neg | ProcessUnaryOp::Not => value_width(operand),
        },
        ProcessValueKind::RawResize { operand } => value_width(operand),
        ProcessValueKind::Binary {
            operation,
            left,
            right,
        } => match operation {
            ProcessBinaryOp::Eq
            | ProcessBinaryOp::Ne
            | ProcessBinaryOp::Lt
            | ProcessBinaryOp::Le
            | ProcessBinaryOp::Gt
            | ProcessBinaryOp::Ge
            | ProcessBinaryOp::SignedLt
            | ProcessBinaryOp::SignedLe
            | ProcessBinaryOp::SignedGt
            | ProcessBinaryOp::SignedGe
            | ProcessBinaryOp::FloatEq
            | ProcessBinaryOp::FloatNe
            | ProcessBinaryOp::FloatLt
            | ProcessBinaryOp::FloatLe
            | ProcessBinaryOp::FloatGt
            | ProcessBinaryOp::FloatGe => Some(1),
            ProcessBinaryOp::FloatAdd
            | ProcessBinaryOp::FloatSub
            | ProcessBinaryOp::FloatMul
            | ProcessBinaryOp::FloatDiv => Some(64),
            ProcessBinaryOp::Shl => shifted_width(value_width(left)?, *right, context),
            _ => Some(value_width(left)?.max(value_width(right)?)),
        },
        ProcessValueKind::Select {
            then_value,
            else_value,
            ..
        } => Some(value_width(then_value)?.max(value_width(else_value)?)),
        ProcessValueKind::MetaCompare { .. } => Some(1),
        ProcessValueKind::Concat(values) | ProcessValueKind::Array(values) => values
            .iter()
            .try_fold(0u32, |total, value| total.checked_add(value_width(value)?)),
        ProcessValueKind::Field { base, field } => {
            let layout = process_value_source_layout(*base, context.process_ir)?;
            let LayoutKind::Struct { fields, .. } = &layout.kind else {
                return None;
            };
            fields
                .iter()
                .find(|candidate| candidate.name == *field)?
                .layout
                .bit_width()?
                .try_into()
                .ok()
        }
        ProcessValueKind::Index { base, .. } => {
            match &process_value_source_layout(*base, context.process_ir)?.kind {
                LayoutKind::Array { element, .. } => element.bit_width()?.try_into().ok(),
                LayoutKind::Packed { .. } => Some(1),
                _ => None,
            }
        }
        ProcessValueKind::Definition(_)
        | ProcessValueKind::Intrinsic(_)
        | ProcessValueKind::Default
        | ProcessValueKind::Attribute { .. }
        | ProcessValueKind::Range { .. }
        | ProcessValueKind::Match { .. }
        | ProcessValueKind::Call { .. }
        | ProcessValueKind::Construct { .. }
        | ProcessValueKind::Invalid => None,
    };
    width.filter(|width| *width != 0)
}

/// Add a constant shift to a value's natural width; dynamic shifts retain the
/// left operand's width, matching normalized digital expression inference.
fn shifted_width(left: u32, right: ProcessValueId, context: &LoweringContext<'_>) -> Option<u32> {
    shifted_arena_width(left, right, &context.process_ir.values)
}

/// Error-recovery value for a malformed value-level match arm. Correct source
/// never contains this node because type checking rejects a missing arm value.
fn missing_value(
    span: crate::diag::Span,
    context: &mut LoweringContext<'_>,
) -> crate::ir::ProcessValueId {
    push_value(
        span,
        None,
        None,
        ProcessValueKind::Intrinsic("<missing-match-value>".to_string()),
        context,
    )
}

/// Find the process-local declaration selected by a path.
fn process_local(
    path: &ast::Path,
    process: &ProcessCfg,
    resolved: &Resolved,
) -> Option<ProcessLocalId> {
    let definition = resolved.resolved(path.span)?;
    process
        .locals
        .iter()
        .find(|local| local.source == Some(definition))
        .map(|local| local.id)
}

/// The persistent testbench storage a path names, if any.
///
/// Tried only after process locals and hardware signals, so lexical shadowing
/// and DUT references keep their existing meaning; this catches the entity-level
/// `let`s of a test entity, which digital lowering deliberately gives no signal.
/// Matching is by resolved declaration, so two roots declaring the same name
/// stay distinct.
fn testbench_storage(
    path: &ast::Path,
    owner: crate::elab::InstanceId,
    context: &LoweringContext<'_>,
) -> Option<ProcessStorageId> {
    let declaration = context.resolved.resolved(path.span)?;
    context
        .process_ir
        .storages
        .iter()
        .find(|storage| storage.owner == owner && storage.source == Some(declaration))
        .map(|storage| storage.id)
}

/// Resolve a source value to its flattened storage leaves. Process locals win
/// over equal signal spellings, preserving lexical shadowing.
fn signal_reference(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &LoweringContext<'_>,
) -> Option<Vec<SignalId>> {
    if assignment_base(expression)
        .and_then(|path| process_local(path, process, context.resolved))
        .is_some()
    {
        return None;
    }
    if !matches!(
        expression,
        ast::Expr::Path(_) | ast::Expr::Field { .. } | ast::Expr::Index { .. }
    ) {
        return None;
    }

    let source_path = crate::syntax::pretty::expr_string(expression);
    let qualified = format!("{}.{}", context.root_path, source_path);
    let exact = context
        .design
        .signals
        .iter()
        .enumerate()
        .find(|(_, signal)| signal.path == qualified)
        .and_then(|(index, _)| u32::try_from(index).ok())
        .filter(|id| !is_representation_signal(context.design, *id));
    if let Some(id) = exact {
        return Some(vec![SignalId(id)]);
    }

    let field_prefix = format!("{qualified}.");
    let index_prefix = format!("{qualified}[");
    let signals = context
        .design
        .signals
        .iter()
        .enumerate()
        .filter(|(_, signal)| {
            signal.path.starts_with(&field_prefix) || signal.path.starts_with(&index_prefix)
        })
        .filter_map(|(index, _)| u32::try_from(index).ok())
        .filter(|id| !is_representation_signal(context.design, *id))
        .map(SignalId)
        .collect::<Vec<_>>();
    (!signals.is_empty()).then_some(signals)
}

/// Turn an integer/real spelling into a source-independent numeric payload.
fn parse_number(text: &str, ty: Option<&crate::types::Ty>) -> ProcessNumber {
    let normalized = text.trim().replace('_', "");
    if normalized.contains('.') || matches!(ty, Some(crate::types::Ty::Real)) {
        return ProcessNumber::Real(normalized.parse::<f64>().unwrap_or(0.0).to_bits());
    }
    let (digits, radix) = if let Some(digits) = normalized
        .strip_prefix("0x")
        .or_else(|| normalized.strip_prefix("0X"))
    {
        (digits, 16)
    } else if let Some(digits) = normalized
        .strip_prefix("0b")
        .or_else(|| normalized.strip_prefix("0B"))
    {
        (digits, 2)
    } else if let Some(digits) = normalized
        .strip_prefix("0o")
        .or_else(|| normalized.strip_prefix("0O"))
    {
        (digits, 8)
    } else {
        (normalized.as_str(), 10)
    };
    ProcessNumber::Integer(parse_digits_words(digits, radix))
}

/// Accumulate arbitrary-width digits into low-word-first storage.
fn parse_digits_words(digits: &str, radix: u32) -> Vec<u64> {
    let mut words = Vec::<u64>::new();
    for digit in crate::syntax::radix_digits(digits) {
        let Some(digit) = digit.to_digit(radix) else {
            return vec![0];
        };
        let mut carry = u128::from(digit);
        for word in &mut words {
            let next = u128::from(*word) * u128::from(radix) + carry;
            *word = next as u64;
            carry = next >> 64;
        }
        if carry != 0 || words.is_empty() {
            words.push(carry as u64);
        }
    }
    words
}

/// Convert a parsed operator to its precedence-free process form.
/// Whether a lowered binary operation works on reals.
fn is_float_operation(operation: &ProcessBinaryOp) -> bool {
    matches!(
        operation,
        ProcessBinaryOp::FloatAdd
            | ProcessBinaryOp::FloatSub
            | ProcessBinaryOp::FloatMul
            | ProcessBinaryOp::FloatDiv
            | ProcessBinaryOp::FloatEq
            | ProcessBinaryOp::FloatNe
            | ProcessBinaryOp::FloatLt
            | ProcessBinaryOp::FloatLe
            | ProcessBinaryOp::FloatGt
            | ProcessBinaryOp::FloatGe
    )
}

fn lower_binary_operator(
    operator: &ast::BinOp,
    left: Option<&crate::types::Ty>,
    right: Option<&crate::types::Ty>,
) -> ProcessBinaryOp {
    let real = [left, right]
        .into_iter()
        .flatten()
        .any(|ty| matches!(ty, crate::types::Ty::Real));
    // A contextual integer literal adopts the numeric vector family beside
    // it. `unsigned[4] * 2` is therefore unsigned even though the standalone
    // literal's fallback type is the signed kernel integer; a `signed`
    // family beside that same literal selects signed operations.
    let types = [left, right].into_iter().flatten().collect::<Vec<_>>();
    let signed = types
        .iter()
        .find_map(|ty| match ty {
            crate::types::Ty::Array {
                family: Some(family),
                ..
            } => Some(family.rsplit("::").next() == Some("signed")),
            _ => None,
        })
        .unwrap_or_else(|| types.into_iter().any(process_type_is_signed));
    match operator {
        ast::BinOp::Add if real => ProcessBinaryOp::FloatAdd,
        ast::BinOp::Sub if real => ProcessBinaryOp::FloatSub,
        ast::BinOp::Mul if real => ProcessBinaryOp::FloatMul,
        ast::BinOp::Div if real => ProcessBinaryOp::FloatDiv,
        ast::BinOp::Eq if real => ProcessBinaryOp::FloatEq,
        ast::BinOp::Ne if real => ProcessBinaryOp::FloatNe,
        ast::BinOp::Lt if real => ProcessBinaryOp::FloatLt,
        ast::BinOp::Le if real => ProcessBinaryOp::FloatLe,
        ast::BinOp::Gt if real => ProcessBinaryOp::FloatGt,
        ast::BinOp::Ge if real => ProcessBinaryOp::FloatGe,
        ast::BinOp::Add if signed => ProcessBinaryOp::SignedAdd,
        ast::BinOp::Sub if signed => ProcessBinaryOp::SignedSub,
        ast::BinOp::Mul if signed => ProcessBinaryOp::SignedMul,
        ast::BinOp::Div if signed => ProcessBinaryOp::SignedDiv,
        ast::BinOp::Shr if signed => ProcessBinaryOp::ArithmeticShr,
        ast::BinOp::Lt if signed => ProcessBinaryOp::SignedLt,
        ast::BinOp::Le if signed => ProcessBinaryOp::SignedLe,
        ast::BinOp::Gt if signed => ProcessBinaryOp::SignedGt,
        ast::BinOp::Ge if signed => ProcessBinaryOp::SignedGe,
        ast::BinOp::Add => ProcessBinaryOp::Add,
        ast::BinOp::Sub => ProcessBinaryOp::Sub,
        ast::BinOp::Mul => ProcessBinaryOp::Mul,
        ast::BinOp::Div => ProcessBinaryOp::Div,
        ast::BinOp::And => ProcessBinaryOp::And,
        ast::BinOp::Or => ProcessBinaryOp::Or,
        ast::BinOp::Custom { symbol, .. } => ProcessBinaryOp::Custom(symbol.clone()),
        ast::BinOp::Shl => ProcessBinaryOp::Shl,
        ast::BinOp::Shr => ProcessBinaryOp::Shr,
        ast::BinOp::Eq => ProcessBinaryOp::Eq,
        ast::BinOp::Ne => ProcessBinaryOp::Ne,
        ast::BinOp::Lt => ProcessBinaryOp::Lt,
        ast::BinOp::Le => ProcessBinaryOp::Le,
        ast::BinOp::Gt => ProcessBinaryOp::Gt,
        ast::BinOp::Ge => ProcessBinaryOp::Ge,
    }
}

/// Whether a checked source type carries signed numeric semantics. The
/// compiler is allowed to identify type families; their values and operator
/// tables remain owned by std.
fn process_type_is_signed(ty: &crate::types::Ty) -> bool {
    matches!(ty, crate::types::Ty::Integer)
        || matches!(
            ty,
            crate::types::Ty::Array {
                family: Some(family),
                ..
            } if family.rsplit("::").next() == Some("signed")
        )
}

/// Namespace-qualified spelling of a path used as an intrinsic dispatch key.
fn path_name(path: &ast::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join("::")
}

/// The callee's rendered name: a `::`-joined path, or the method name for a
/// `.method` call.
fn callee_name(callee: &ast::Expr) -> String {
    match callee {
        ast::Expr::Path(path) => path
            .segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join("::"),
        _ => crate::syntax::pretty::expr_string(callee),
    }
}

/// Whether a path names one compiler-provided intrinsic. Runtime primitives
/// deliberately have no declaration; a resolved same-named path is accepted
/// only for a builtin fallback and otherwise belongs to the user.
fn builtin_callee_is(callee: &ast::Expr, expected: &str, context: &LoweringContext<'_>) -> bool {
    let ast::Expr::Path(path) = callee else {
        return false;
    };
    path.segments
        .last()
        .is_some_and(|leaf| leaf.text == expected)
        && context
            .resolved
            .resolved(path.span)
            .is_none_or(|definition| {
                context.resolved.def(definition).is_some_and(|definition| {
                    definition.kind == crate::resolve::DefKind::Builtin
                        && definition.name == expected
                })
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::{DiagnosticSink, FileId};

    #[test]
    /// End-to-end check that a test body fills `Design::process_ir` with the
    /// branches and suspension points its source implies.
    fn fills_design_process_cfg_with_branches_and_suspension() {
        let sources = [
            "module tests;\n\
             enum Mode { Off, On }\n\
             enum Cell { Low, High }\n\
             impl Not<Cell> for Cell {\n\
               fn not(self) -> Cell {\n\
                 if self == Cell::Low { return Cell::High; }\n\
                 return Cell::Low;\n\
               }\n\
             }\n\
             struct Wrap(integer);\n\
             impl Eq<Wrap> for Wrap {\n\
               fn eq(self, rhs: Wrap) -> Bool { return true; }\n\
             }\n\
             entity Device { input: Bool in, output: Bool out }\n\
             impl Device { output = input; }\n\
             #[test] entity Smoke {}\n\
             impl Smoke {\n\
               let flag: Bool = true;\n\
               let i: integer = 9;\n\
               let observed: Bool;\n\
               let wrapped: Wrap = Wrap(12);\n\
               let concat_high: Bool = false;\n\
               let concat_low: Bool = false;\n\
               let cells: Cell[2] = [Cell::Low, Cell::High];\n\
               let flipped: Cell[2] = not cells;\n\
               let dut: Device = { .input = flag, .output = observed };\n\
               clock: process { flag = not flag after 1ns; }\n\
               stimulus: process {\n\
                 let seen: Bool = flag;\n\
                 let mode: Mode = Mode::On;\n\
                 if seen { print!(\"set\"); } else { warn!(true, \"clear\"); }\n\
                 match mode {\n\
                   Mode::Off => { warn!(true, \"off\"); }\n\
                   Mode::On => { print!(\"on\"); }\n\
                 }\n\
                 assert!(wrapped == Wrap(12), \"constructor without assignment context\");\n\
                 assert!((18446744073709551616 + 1) != 1, \"wide integer expression\");\n\
                 print!(\"before loop\");\n\
                 for i in 0..2 { print!(\"loop {}\", i); }\n\
                 i = 7;\n\
                 seen = false;\n\
                 { concat_high, concat_low } = 2;\n\
                 flag = false after 1;\n\
                 flag = false;\n\
                 await 2ns;\n\
                 await true;\n\
                 assert!(flag == false, \"done\");\n\
                 finish();\n\
               }\n\
             }",
            "module std::logic; pub enum Bool { false, true } attr lang for Bool = \"bool\";",
            "module std::attrs; use std::logic::Bool; pub attr test: Bool for entity; attr lang for test = \"test\";",
            "module std::ops; use std::logic::Bool; pub enum Ordering { Less, Equal, Greater } \
             pub trait Boolean { fn as_bool(self) -> Bool; } attr lang for Boolean = \"boolean\"; \
             pub trait Not<Out> { fn not(self) -> Out; } attr lang for Not = \"not\"; \
             impl Boolean for Bool { fn as_bool(self) -> Bool { return self; } } \
             impl Not<Bool> for Bool { fn not(self) -> Bool { return self; } } \
             impl<T: Not<T>> Not<T> for T[] { \
               fn not(self) -> T[] { let result: T[] = self; \
                 for i in self'range { result[i] = not self[i]; } return result; } } \
             pub trait Suffix<symbol: string, input> { fn suffix(data: input) {} } \
             attr lang for Suffix = \"suffix\";",
            "module std::prelude; pub use std::logic::Bool; pub use std::attrs::test; \
             pub use std::ops::{Boolean, Not};",
            "module std::sim; use std::ops::Suffix; pub struct time(integer); attr lang for time = \"time\"; \
             impl Suffix<\"ns\", integer> for time { \
               fn suffix(value: integer) -> time { return time(value * 37); } \
             }",
        ];
        let mut sink = DiagnosticSink::new();
        let modules: Vec<Module> = sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                crate::syntax::parse_module(FileId(index as u32), source, &mut sink)
            })
            .collect();
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let typed = crate::types::check(&modules, &resolved, &mut sink);
        let (hierarchy, plan) = crate::testbench::elaborate(&modules, &resolved, &typed, &mut sink);
        let mut design = crate::ir::lower(&modules, &resolved, &hierarchy, &mut sink);
        assert!(!sink.has_errors(), "{:#?}", sink.diagnostics());

        lower(
            &modules,
            &resolved,
            &typed,
            &hierarchy,
            Some(&plan),
            &mut design,
        );
        assert!(design
            .process_ir
            .validate(design.signals.len() as u32)
            .is_empty());
        assert!(design.process_ir.values.iter().all(|value| !matches!(
            value.kind,
            ProcessValueKind::Definition(_) | ProcessValueKind::Call { .. }
        )));
        assert!(
            design.process_ir.values.iter().any(|value| matches!(
                &value.kind,
                ProcessValueKind::Array(elements)
                    if elements.len() == 2 && elements.iter().all(|element| matches!(
                        design.process_ir.values[element.0 as usize].kind,
                        ProcessValueKind::Select { .. }
                    ))
            )),
            "blanket array operators should inline each element's source implementation"
        );
        let wide_literal = design
            .process_ir
            .values
            .iter()
            .position(|value| {
                matches!(
                    &value.kind,
                    ProcessValueKind::Number(ProcessNumber::Integer(words))
                        if words == &[0, 1]
                )
            })
            .map(|index| ProcessValueId(index as u32))
            .expect("wide integer literal");
        let wide_add = design
            .process_ir
            .values
            .iter()
            .find(|value| {
                matches!(
                    value.kind,
                    ProcessValueKind::Binary {
                        operation: ProcessBinaryOp::SignedAdd,
                        left,
                        ..
                    } if left == wide_literal
                )
            })
            .expect("wide integer addition");
        assert_eq!(wide_add.bit_width, Some(65));
        assert_eq!(design.process_ir.tests.len(), 1);
        assert_eq!(design.process_ir.processes.len(), 3);
        assert!(!design.process_ir.values.is_empty());
        let descriptor = &design.process_ir.tests[0];
        assert_eq!(descriptor.qualified_name, "tests::Smoke");
        assert_eq!(
            descriptor.processes,
            [ProcessId(0), ProcessId(1), ProcessId(2)]
        );
        let flag_storage = design
            .process_ir
            .storages
            .iter()
            .find(|storage| storage.name == "flag")
            .expect("flag storage")
            .id;
        let clock = &design.process_ir.processes[0];
        assert_eq!(clock.label.as_deref(), Some("Smoke::clock"));
        assert_eq!(
            clock.activation,
            ProcessActivation::Reactive {
                sensitivity: vec![ProcessSensitivity::Storage(flag_storage)]
            },
            "a self-toggle clock wakes on its persistent testbench storage"
        );
        assert!(clock
            .blocks
            .iter()
            .any(
                |block| block.instructions.iter().any(|instruction| matches!(
                    instruction,
                    ProcessInstruction::Schedule { delay, .. }
                        if matches!(
                            &design.process_ir.values[delay.0 as usize].kind,
                            ProcessValueKind::Number(ProcessNumber::Integer(words))
                                if words == &[37]
                        )
                ))
            ));
        let process = &design.process_ir.processes[1];
        assert_eq!(process.label.as_deref(), Some("Smoke::stimulus"));
        assert_eq!(process.locals.len(), 3);
        assert!(process
            .blocks
            .iter()
            .any(|block| matches!(block.terminator, ProcessTerminator::Branch { .. })));
        assert!(process.blocks.iter().any(|block| matches!(
            &block.terminator,
            ProcessTerminator::Suspend {
                operation: ProcessSuspendOp::AwaitTime,
                arguments,
                ..
            }
                if matches!(
                    &design.process_ir.values[arguments[0].0 as usize].kind,
                    ProcessValueKind::Number(ProcessNumber::Integer(words))
                        if words == &[74]
                )
        )));
        assert!(
            process.blocks.iter().any(|block| matches!(
                &block.terminator,
                ProcessTerminator::Suspend {
                    operation: ProcessSuspendOp::Settle,
                    arguments,
                    ..
                } if arguments.is_empty()
            )),
            "a foreground drive into the DUT must yield until reactive settling"
        );
        let hardware = &design.process_ir.processes[2];
        let input = design
            .signals
            .iter()
            .position(|signal| signal.path.ends_with(".dut.input"))
            .map(|index| SignalId(index as u32))
            .expect("DUT input signal");
        let output = design
            .signals
            .iter()
            .position(|signal| signal.path.ends_with(".dut.output"))
            .map(|index| SignalId(index as u32))
            .expect("DUT output signal");
        assert_eq!(hardware.root, descriptor.root);
        assert_ne!(hardware.owner, descriptor.root);
        assert_eq!(
            hardware.activation,
            ProcessActivation::Reactive {
                sensitivity: vec![ProcessSensitivity::Signal(input)]
            }
        );
        assert!(hardware
            .blocks
            .iter()
            .any(
                |block| block.instructions.iter().any(|instruction| matches!(
                    instruction,
                    ProcessInstruction::Assign {
                        semantics: ProcessAssignment::StagedSignal,
                        driver_context: Some(_),
                        target,
                        value,
                        ..
                    } if matches!(
                        &design.process_ir.values[target.0 as usize].kind,
                        ProcessValueKind::Signal { signals, .. } if signals == &[output]
                    ) && matches!(
                        &design.process_ir.values[value.0 as usize].kind,
                        ProcessValueKind::Signal { signals, .. } if signals == &[input]
                    )
                ))
            ));
        assert!(process
            .blocks
            .iter()
            .any(|block| matches!(block.terminator, ProcessTerminator::Match { .. })));
        assert!(process
            .blocks
            .iter()
            .any(|block| matches!(block.terminator, ProcessTerminator::For { .. })));
        let loop_header = process
            .blocks
            .iter()
            .find(|block| matches!(block.terminator, ProcessTerminator::For { .. }))
            .expect("missing loop header");
        assert!(
            loop_header.instructions.is_empty(),
            "loop back-edge would replay pre-loop instructions: {loop_header:?}"
        );
        assert!(process
            .blocks
            .iter()
            .any(|block| matches!(block.terminator, ProcessTerminator::Finish { .. })));
        assert!(process.blocks.iter().any(|block| block
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, ProcessInstruction::Schedule { .. }))));
        assert!(design.process_ir.processes[0]
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                instruction,
                ProcessInstruction::Schedule {
                    driver_context: None,
                    ..
                }
            )));
        let assignments = process
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| match instruction {
                ProcessInstruction::Assign { semantics, .. } => Some(*semantics),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(assignments.contains(&ProcessAssignment::ImmediateLocal));
        assert!(assignments.contains(&ProcessAssignment::ImmediateStorage));
        assert!(assignments.contains(&ProcessAssignment::PerPlace));
        let post_loop_i = process
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| match instruction {
                ProcessInstruction::Assign {
                    semantics, target, ..
                } if matches!(
                    &design.process_ir.values[target.0 as usize].kind,
                    ProcessValueKind::Storage(storage)
                        if design.process_ir.storages[storage.0 as usize].name == "i"
                ) => Some(*semantics),
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!(
                    "missing post-loop write to the shadowed entity-level `i`; storages={:?}, values={:?}",
                    design
                        .process_ir
                        .storages
                        .iter()
                        .map(|storage| &storage.name)
                        .collect::<Vec<_>>(),
                    design.process_ir.values
                )
            });
        // `i` inside the loop is a process local and `i` after it is the
        // entity-level declaration, so finding storage here is also the
        // shadowing check.
        assert_eq!(post_loop_i, ProcessAssignment::ImmediateStorage);
        assert!(
            design
                .process_ir
                .storages
                .iter()
                .any(|storage| storage.name == "i" && storage.owner == descriptor.root),
            "the entity-level `let i` should be testbench storage owned by the test root"
        );
        let flag = design
            .process_ir
            .storages
            .iter()
            .find(|storage| storage.name == "flag")
            .expect("flag storage");
        assert!(flag.initializer.is_some());
        assert!(flag.bindings.iter().any(|binding| {
            binding.direction == LayoutDirection::In
                && design.signals[binding.signal.0 as usize]
                    .path
                    .ends_with(".dut.input")
        }));
        let observed = design
            .process_ir
            .storages
            .iter()
            .find(|storage| storage.name == "observed")
            .expect("observed storage");
        assert!(observed.bindings.iter().any(|binding| {
            binding.direction == LayoutDirection::Out
                && design.signals[binding.signal.0 as usize]
                    .path
                    .ends_with(".dut.output")
        }));
        let dump = design.process_ir.to_ir_string();
        assert!(dump.contains("value %v0"));
        assert!(dump.contains(&format!(
            "test @tests::Smoke root {} processes [%p0, %p1, %p2]",
            descriptor.root.0
        )));
        // The storage arena is part of the product, so `--emit ir` style dumps
        // have to show it or a migration bug there is invisible.
        assert!(
            dump.contains(&format!("storage %g1 root {} i", descriptor.root.0)),
            "the dump should name the entity-level `i` storage:\n{dump}"
        );
        assert!(dump.contains(&format!(
            "process %p1 root {} owner {} [Smoke::stimulus]",
            descriptor.root.0, descriptor.root.0
        )));
        assert!(design.process_ir.values.iter().any(|value| matches!(
            value.kind,
            ProcessValueKind::Binary {
                operation: ProcessBinaryOp::Eq,
                ..
            }
        )));
        assert!(
            design.process_ir.values.iter().all(|value| {
                !matches!(
                    value.kind,
                    ProcessValueKind::Local { .. } | ProcessValueKind::Storage(_)
                ) || value.bit_width.is_some()
            }),
            "scalar state references need direct-backend widths: {:#?}",
            design.process_ir.values
        );
        for (index, value) in design.process_ir.values.iter().enumerate() {
            assert!(
                crate::ir::process::process_value_dependencies(&value.kind)
                    .into_iter()
                    .all(|dependency| dependency.0 < index as u32),
                "value %{index} is not in dependency order: {value:?}"
            );
        }
        assert_eq!(
            design.process_ir.value_layouts.len(),
            design.process_ir.values.len(),
            "production lowering keeps aggregate metadata arena-aligned"
        );
        assert!(design
            .process_ir
            .values
            .iter()
            .enumerate()
            .any(|(index, value)| {
                matches!(value.kind, ProcessValueKind::Array(_))
                    && matches!(
                        design.process_ir.value_layouts[index]
                            .as_ref()
                            .map(|layout| &layout.kind),
                        Some(LayoutKind::Array { .. })
                    )
            }));
    }

    #[test]
    /// Display metadata belongs to the derived Process value, not to the AST
    /// spelling that happened to produce it. Attributes, enum variants and
    /// selects, and an indexed runtime string must therefore retain their
    /// respective Bool/enum/Char identities.
    fn derived_display_values_retain_source_types() {
        let sources = [
            "module tests;\n\
             enum State { Idle, Run, Done }\n\
             #[test] entity Smoke {}\n\
             impl Smoke {\n\
               let flags: Bool[0..1] = [false, true];\n\
               let state: State = State::Run;\n\
               let text: string = \"hello\";\n\
               run: process {\n\
                 print!(\"{} {} {} {}\", flags'ascending, state, State::Done,\n\
                   if true { state } else { State::Idle });\n\
                 print!(\"{} {}\", text, text[2]);\n\
               }\n\
             }",
            "module std::logic; pub enum Bool { false, true } attr lang for Bool = \"bool\";",
            "module std::attrs; use std::logic::Bool; pub attr test: Bool for entity; attr lang for test = \"test\";",
            "module std::ops; use std::logic::Bool; pub trait Boolean { fn as_bool(self) -> Bool; } attr lang for Boolean = \"boolean\"; \
             impl Boolean for Bool { fn as_bool(self) -> Bool { return self; } }",
            "module std::text; pub type string = Char[];",
            "module std::prelude; pub use std::logic::Bool; pub use std::attrs::test; \
             pub use std::ops::Boolean; pub use std::text::string;",
        ];
        let mut sink = DiagnosticSink::new();
        let modules = sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                crate::syntax::parse_module(FileId(index as u32), source, &mut sink)
            })
            .collect::<Vec<_>>();
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let typed = crate::types::check(&modules, &resolved, &mut sink);
        let (hierarchy, plan) = crate::testbench::elaborate(&modules, &resolved, &typed, &mut sink);
        let mut design = crate::ir::lower(&modules, &resolved, &hierarchy, &mut sink);
        assert!(!sink.has_errors(), "{:#?}", sink.diagnostics());

        lower(
            &modules,
            &resolved,
            &typed,
            &hierarchy,
            Some(&plan),
            &mut design,
        );

        let formatted = design
            .process_ir
            .processes
            .iter()
            .flat_map(|process| &process.blocks)
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| match instruction {
                ProcessInstruction::Runtime {
                    format: Some(format),
                    ..
                } => Some(format),
                _ => None,
            })
            .flatten()
            .filter_map(|part| match part {
                ProcessFormatPart::Value { value, kind } => Some((*value, kind)),
                ProcessFormatPart::Text(_) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(formatted.len(), 6, "{formatted:#?}");
        assert!(
            formatted.iter().any(|(value, kind)| {
                matches!(kind, ProcessDisplayKind::String)
                    && process_value_source_layout(*value, &design.process_ir)
                        .and_then(crate::ir::SourceLayout::index_range)
                        .and_then(crate::ir::LayoutRange::len)
                        == Some(5)
            }),
            "formatted={formatted:#?} storages={:#?}",
            design.process_ir.storages
        );
        assert!(formatted.iter().any(|(value, kind)| {
            matches!(kind, ProcessDisplayKind::Character)
                && design.process_ir.values[value.0 as usize].bit_width == Some(32)
        }));
        assert!(formatted.iter().any(|(value, kind)| {
            matches!(kind, ProcessDisplayKind::Enum(name) if name.ends_with("Bool"))
                && design.process_ir.values[value.0 as usize].bit_width == Some(1)
        }));
        assert_eq!(
            formatted
                .iter()
                .filter(|(value, kind)| {
                    matches!(kind, ProcessDisplayKind::Enum(name) if name.ends_with("State"))
                        && design.process_ir.values[value.0 as usize].bit_width == Some(2)
                })
                .count(),
            3,
            "{formatted:#?}"
        );
        assert!(design
            .process_ir
            .validate(design.signals.len() as u32)
            .is_empty());
    }

    #[test]
    /// Storage bindings retain the direction of each applied-view field. A
    /// whole bus connection can therefore drive some DUT leaves and observe
    /// others without inventing a direction for the backing struct itself.
    fn storage_bindings_follow_applied_view_directions() {
        let sources = [
            "module tests;\n\
             struct Link { pub request: Bool, pub response: Bool }\n\
             view Slave for Link { request in, response out }\n\
             entity Device { bus: Link Slave }\n\
             impl Device { bus.response = bus.request; }\n\
             #[test] entity Smoke {}\n\
             impl Smoke {\n\
               let link: Link;\n\
               let dut: Device = { .bus = link };\n\
               stimulus: process {}\n\
             }",
            "module std::logic; pub enum Bool { false, true } attr lang for Bool = \"bool\";",
            "module std::attrs; use std::logic::Bool; pub attr test: Bool for entity; attr lang for test = \"test\";",
            "module std::ops; use std::logic::Bool; pub trait Boolean { fn as_bool(self) -> Bool; } attr lang for Boolean = \"boolean\"; \
             impl Boolean for Bool { fn as_bool(self) -> Bool { return self; } }",
            "module std::prelude; pub use std::logic::Bool; pub use std::attrs::test; \
             pub use std::ops::Boolean;",
        ];
        let mut sink = DiagnosticSink::new();
        let modules = sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                crate::syntax::parse_module(FileId(index as u32), source, &mut sink)
            })
            .collect::<Vec<_>>();
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let typed = crate::types::check(&modules, &resolved, &mut sink);
        let (hierarchy, plan) = crate::testbench::elaborate(&modules, &resolved, &typed, &mut sink);
        let mut design = crate::ir::lower(&modules, &resolved, &hierarchy, &mut sink);
        assert!(!sink.has_errors(), "{:#?}", sink.diagnostics());

        lower(
            &modules,
            &resolved,
            &typed,
            &hierarchy,
            Some(&plan),
            &mut design,
        );
        let link = design
            .process_ir
            .storages
            .iter()
            .find(|storage| storage.name == "link")
            .expect("link storage");
        let binding = |projection: &str, direction| {
            link.bindings.iter().any(|binding| {
                binding.projection == projection
                    && binding.direction == direction
                    && design.signals[binding.signal.0 as usize]
                        .path
                        .contains(".dut.bus.")
            })
        };
        assert!(binding(".request", LayoutDirection::In));
        assert!(binding(".response", LayoutDirection::Out));
        assert!(design
            .process_ir
            .validate(design.signals.len() as u32)
            .is_empty());
    }

    #[test]
    /// Hardware CFG construction is not conditional on native-test discovery:
    /// ordinary IR/object compilations carry the same canonical process graph.
    fn hardware_processes_lower_without_a_test_plan() {
        let sources = [
            "module gates; entity Gate { input: Bool in, output: Bool out } \
             impl Gate { output = input; }",
            "module std::logic; pub enum Bool { false, true } attr lang for Bool = \"bool\";",
            "module std::ops; use std::logic::Bool; pub trait Boolean { fn as_bool(self) -> Bool; } attr lang for Boolean = \"boolean\"; \
             impl Boolean for Bool { fn as_bool(self) -> Bool { return self; } }",
            "module std::prelude; pub use std::logic::Bool; pub use std::ops::Boolean;",
        ];
        let mut sink = DiagnosticSink::new();
        let modules = sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                crate::syntax::parse_module(FileId(index as u32), source, &mut sink)
            })
            .collect::<Vec<_>>();
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let typed = crate::types::check(&modules, &resolved, &mut sink);
        let hierarchy = crate::elab::elaborate(&modules, &resolved, &typed, &mut sink);
        let mut design = crate::ir::lower(&modules, &resolved, &hierarchy, &mut sink);
        assert!(!sink.has_errors(), "{:#?}", sink.diagnostics());

        lower(&modules, &resolved, &typed, &hierarchy, None, &mut design);
        assert!(design.process_ir.tests.is_empty());
        assert_eq!(design.process_ir.processes.len(), 1);
        let process = &design.process_ir.processes[0];
        assert_eq!(process.root, process.owner);
        assert!(matches!(
            process.activation,
            ProcessActivation::Reactive { ref sensitivity }
                if matches!(sensitivity.as_slice(), [ProcessSensitivity::Signal(_)])
        ));
        assert!(
            design
                .process_ir
                .values
                .iter()
                .all(|value| value.bit_width.is_some()),
            "normalized hardware values must carry backend-ready widths: {:#?}",
            design.process_ir.values
        );
        assert!(design
            .process_ir
            .validate(design.signals.len() as u32)
            .is_empty());
    }

    #[test]
    /// Resolver-selected constants become their executable initializer graph;
    /// a backend must never need the frontend declaration behind a `DefId`.
    fn module_constants_do_not_survive_as_definitions() {
        let sources = [
            "module tests; const EXPECTED: integer = 3; \
             fn choose(a: integer, b: integer) -> integer { \
               let left: integer = a; \
               if left > b { return left; } return b; \
             } \
             #[test] entity Smoke {} \
             impl Smoke { let observed: integer = 0; \
               run: process { observed = choose(observed, EXPECTED); } }",
            "module std::logic; pub enum Bool { false, true } attr lang for Bool = \"bool\";",
            "module std::attrs; use std::logic::Bool; pub attr test: Bool for entity; attr lang for test = \"test\";",
            "module std::ops; use std::logic::Bool; \
             pub trait Boolean { fn as_bool(self) -> Bool; } attr lang for Boolean = \"boolean\"; \
             impl Boolean for Bool { fn as_bool(self) -> Bool { return self; } }",
            "module std::prelude; pub use std::logic::Bool; \
             pub use std::attrs::test; pub use std::ops::Boolean;",
        ];
        let mut sink = DiagnosticSink::new();
        let modules = sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                crate::syntax::parse_module(FileId(index as u32), source, &mut sink)
            })
            .collect::<Vec<_>>();
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let typed = crate::types::check(&modules, &resolved, &mut sink);
        let (hierarchy, plan) = crate::testbench::elaborate(&modules, &resolved, &typed, &mut sink);
        let mut design = crate::ir::lower(&modules, &resolved, &hierarchy, &mut sink);
        assert!(!sink.has_errors(), "{:#?}", sink.diagnostics());

        lower(
            &modules,
            &resolved,
            &typed,
            &hierarchy,
            Some(&plan),
            &mut design,
        );

        assert!(design.process_ir.values.iter().any(|value| matches!(
            value.kind,
            ProcessValueKind::Number(ProcessNumber::Integer(ref words))
                if words.as_slice() == [3]
        )));
        assert!(design
            .process_ir
            .values
            .iter()
            .all(|value| !matches!(value.kind, ProcessValueKind::Definition(_))));
        assert!(design
            .process_ir
            .values
            .iter()
            .any(|value| matches!(value.kind, ProcessValueKind::Select { .. })));
        assert!(design
            .process_ir
            .values
            .iter()
            .all(|value| !matches!(value.kind, ProcessValueKind::Call { .. })));
    }

    #[test]
    /// Aggregate values which exist only as constants, call arguments, or
    /// process-local initializers retain the recursive source layout required
    /// by native Process lowering. Function signatures provide the context for
    /// anonymous array and struct literals before the AST is discarded.
    fn aggregate_only_values_retain_recursive_layouts() {
        let sources = [
            "module tests; \
             struct Pair { pub a: integer, pub b: integer } \
             fn first(values: integer[2]) -> integer { return values[0]; } \
             fn first_pair(value: Pair) -> integer { return value.a; } \
             #[test] entity Smoke {} \
             impl Smoke { \
               const VALUES: integer[2] = [7, 8]; \
               let seed: Pair = { .a = 0, .b = 0 }; \
               let result: integer = 0; \
               run: process { \
                 let local: integer[2] = [1, 2]; \
                 result = first(local); \
                 result = first(VALUES); \
                 result = first_pair({ .a = 3, .b = 4 }); \
               } \
             }",
            "module std::logic; pub enum Bool { false, true } attr lang for Bool = \"bool\";",
            "module std::attrs; use std::logic::Bool; pub attr test: Bool for entity; attr lang for test = \"test\";",
            "module std::ops; use std::logic::Bool; \
             pub trait Boolean { fn as_bool(self) -> Bool; } attr lang for Boolean = \"boolean\"; \
             impl Boolean for Bool { fn as_bool(self) -> Bool { return self; } }",
        ];
        let mut sink = DiagnosticSink::new();
        let modules = sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                crate::syntax::parse_module(FileId(index as u32), source, &mut sink)
            })
            .collect::<Vec<_>>();
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        let typed = crate::types::check(&modules, &resolved, &mut sink);
        let (hierarchy, plan) = crate::testbench::elaborate(&modules, &resolved, &typed, &mut sink);
        let mut design = crate::ir::lower(&modules, &resolved, &hierarchy, &mut sink);
        assert!(!sink.has_errors(), "{:#?}", sink.diagnostics());

        lower(
            &modules,
            &resolved,
            &typed,
            &hierarchy,
            Some(&plan),
            &mut design,
        );

        assert_eq!(
            design.process_ir.value_layouts.len(),
            design.process_ir.values.len()
        );
        let has_layout = |predicate: fn(&ProcessValueKind) -> bool,
                          layout: fn(&LayoutKind) -> bool| {
            design
                .process_ir
                .values
                .iter()
                .zip(&design.process_ir.value_layouts)
                .any(|(value, retained)| {
                    predicate(&value.kind)
                        && retained
                            .as_ref()
                            .is_some_and(|retained| layout(&retained.kind))
                })
        };
        assert!(has_layout(
            |kind| matches!(kind, ProcessValueKind::Array(_)),
            |layout| matches!(layout, LayoutKind::Array { .. })
        ));
        assert!(has_layout(
            |kind| matches!(kind, ProcessValueKind::Construct { .. }),
            |layout| matches!(layout, LayoutKind::Struct { .. })
        ));
        assert!(design
            .process_ir
            .values
            .iter()
            .all(|value| !matches!(value.kind, ProcessValueKind::Definition(_))));
        assert!(design
            .process_ir
            .validate(design.signals.len() as u32)
            .is_empty());
    }

    #[test]
    /// Checked nominal families and source implementation declarations use
    /// one canonical dispatch key, and a trait implementation inherits every
    /// default body it did not override.
    fn process_function_index_canonicalizes_families_and_defaults() {
        let sources = [
            "module std::bits; pub struct unsigned(integer);",
            "module tests; trait Meter { fn label(self) -> integer { return 7; } } \
             struct Plain(integer); impl Meter for Plain {}",
        ];
        let mut sink = DiagnosticSink::new();
        let modules = sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                crate::syntax::parse_module(FileId(index as u32), source, &mut sink)
            })
            .collect::<Vec<_>>();
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        assert!(!sink.has_errors(), "{:#?}", sink.diagnostics());

        let functions = process_functions(&modules, &resolved);
        assert_eq!(
            functions.canonical_type_key("std::bits::unsigned"),
            "unsigned"
        );
        assert_eq!(
            functions
                .get_associated("Plain", "label")
                .map(|function| function.name.text.as_str()),
            Some("label")
        );
    }

    #[test]
    /// A suffix implementation over `real` is folded from its std source body
    /// just like integer time units, retaining the exact IEEE-754 payload in
    /// canonical Process IR.
    fn real_suffix_body_folds_to_process_number() {
        let source = "module units; struct frequency(real); \
            impl Suffix<\"MHz\", real> for frequency { \
                fn suffix(value: real) -> frequency { \
                    return frequency(value * 1000000.0); \
                } \
            }";
        let mut sink = DiagnosticSink::new();
        let modules = vec![crate::syntax::parse_module(FileId(0), source, &mut sink)];
        let resolved = crate::resolve::resolve(&modules, &mut sink);
        assert!(!sink.has_errors(), "{:#?}", sink.diagnostics());

        let suffixes = constant_suffixes(&modules, &resolved);
        let [suffix] = suffixes
            .get("MHz")
            .map(Vec::as_slice)
            .expect("MHz suffix implementation")
        else {
            panic!("expected exactly one MHz suffix implementation");
        };
        assert!(matches!(suffix.domain, SuffixDomain::Real));
        assert_eq!(
            eval_real_suffix_block(&suffix.body, suffix, 2.5),
            Some(2_500_000.0)
        );
    }

    #[test]
    /// Validation must reject a process whose owner or entry block does not
    /// exist, since the native runtime could not execute one.
    fn design_validator_rejects_invalid_process_ownership_and_entry() {
        let span = crate::diag::Span::new(FileId(0), 0..1);
        let design = Design {
            process_ir: ProcessIr {
                storages: Vec::new(),
                processes: vec![ProcessCfg {
                    id: ProcessId(0),
                    root: crate::elab::InstanceId(1),
                    owner: crate::elab::InstanceId(1),
                    label: None,
                    span,
                    activation: ProcessActivation::TimeZero,
                    entry: ProcessBlockId(1),
                    locals: Vec::new(),
                    blocks: vec![ProcessBlock {
                        id: ProcessBlockId(0),
                        instructions: vec![ProcessInstruction::Runtime {
                            operation: ProcessRuntimeOp::Print,
                            arguments: vec![crate::ir::ProcessValueId(9)],
                            format: None,
                            span,
                        }],
                        terminator: ProcessTerminator::Return {
                            value: None,
                            span: None,
                        },
                    }],
                }],
                tests: vec![ProcessTest {
                    entity: crate::resolve::DefId(0),
                    root: crate::elab::InstanceId(0),
                    qualified_name: "tests::Broken".to_string(),
                    span,
                    processes: vec![ProcessId(0)],
                }],
                values: Vec::new(),
                value_layouts: vec![None],
            },
            ..Design::default()
        };
        let issues = design.validate();
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("invalid entry block")),
            "{issues:?}"
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("assigned to another root")),
            "{issues:?}"
        );
        assert!(
            issues.iter().any(|issue| issue.contains("invalid value")),
            "{issues:?}"
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("value layout arena")),
            "{issues:?}"
        );
    }
}
