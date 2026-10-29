//! Typed source lowering into the canonical process IR.
//!
//! Process/CFG types, validation, test descriptors, and ownership live in
//! [`crate::ir::Design`]. Test stimulus enters from typed Siox AST;
//! hardware CFGs are already built by source hardware lowering. This entry
//! attaches procedural state and test descriptors without repeating hardware
//! elaboration or reading a compatibility scheduler graph.

use crate::elab::Hierarchy;
use crate::ir::{
    arena_constant_integer, integer_words_width, shifted_arena_width, Design, LayoutDirection,
    LayoutKind, ProcessActivation, ProcessAggregateField, ProcessAssignment, ProcessBinaryOp,
    ProcessBlock, ProcessBlockId, ProcessCfg, ProcessDisplayKind, ProcessFormatPart,
    ProcessHostValueOp, ProcessId, ProcessInstruction, ProcessIr, ProcessLocal, ProcessLocalId,
    ProcessMatchArm, ProcessNumber, ProcessPattern, ProcessRegion, ProcessRuntimeOp,
    ProcessSensitivity, ProcessSignalState, ProcessStorage, ProcessStorageBinding,
    ProcessStorageId, ProcessSuspendOp, ProcessTerminator, ProcessTest, ProcessUnaryOp,
    ProcessValue, ProcessValueId, ProcessValueKind, ProcessValueMatchArm, SignalId, SourceLayout,
};
use crate::resolve::Resolved;
use crate::syntax::ast::{self, ElseBranch, ImplItem, Stmt};
use crate::syntax::Module;
use crate::testbench::TestPlan;
use crate::types::Typed;

mod cfg_calls;

#[derive(Clone)]
struct ConstantSuffix {
    target: String,
    parameter: String,
    domain: SuffixDomain,
    body: ast::Block,
    /// The impl's `suffix`, inlined when the body builds a struct.
    function: ast::FnDecl,
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
    /// Type aliases by resolver identity, for a local declared through one
    /// (`let h: binary16`) to find the index range its type denotes.
    type_aliases: &'a std::collections::HashMap<crate::resolve::DefId, &'a ast::Type>,
    constant_stack: std::collections::HashSet<crate::resolve::DefId>,
    functions: &'a crate::ir::FunctionIndex<'a>,
    constant_integers: &'a std::collections::HashMap<String, i64>,
    value_bindings: Vec<std::collections::HashMap<crate::resolve::DefId, ProcessValueId>>,
    /// A value carrying the format a family `From` impl is building, which
    /// its body reads as `Self'high` (`lower_process_family_from`).
    self_formats: Vec<ProcessValueId>,
    /// Operands being widened now (spec 3.17): their source value is
    /// lowered as written.
    widening: std::collections::HashSet<crate::diag::Span>,
    inline_self_values: Vec<Option<ProcessValueId>>,
    inline_return_types: Vec<Option<crate::types::Ty>>,
    /// Procedure returns resume the enclosing caller, not the whole process.
    inline_return_blocks: Vec<(ProcessBlockId, Option<ProcessValueId>)>,
    inline_functions: std::collections::HashSet<crate::diag::Span>,
    /// Current canonical source call instance while expanding a CFG body.
    inline_scope: Option<u32>,
    inline_value_locals: Vec<Vec<crate::ir::ProcessValueBinding>>,
    inline_value_branches: Vec<Vec<(ProcessValueId, [crate::diag::Span; 2])>>,
    /// Memoized call reachability in the append-only procedural value arena.
    cfg_call_cache: Vec<bool>,
    /// Shared function bodies by signature, across every process lowered.
    shared_functions: &'a std::cell::RefCell<SharedFunctions>,
    /// Functions found calling themselves, with a call that does: reported
    /// once each when lowering ends.
    /// Recursive calls that could not be lowered: (function name, call,
    /// function, whether the call changes its argument types).
    #[allow(clippy::type_complexity)]
    recursive_calls:
        &'a std::cell::RefCell<Vec<(String, crate::diag::Span, crate::diag::Span, bool)>>,
    /// Source a testbench cannot lower, reported once lowering is done.
    rejected: &'a std::cell::RefCell<Vec<crate::diag::Diagnostic>>,
    /// Recursive calls of pure value functions: each is a call of the
    /// function's shared copy, evaluated where its selection reaches it, so
    /// CFG normalization leaves it alone.
    self_calls: &'a std::cell::RefCell<std::collections::HashSet<ProcessValueId>>,
    /// Recursive functions being expanded with a frame stack, innermost last.
    frames: Vec<cfg_calls::FrameState>,
    /// Functions to expand with a frame stack: a plain expansion found them
    /// recursive.
    frame_functions: std::collections::HashSet<crate::diag::Span>,
}

/// A shared value-function body per signature; `None` marks a signature whose
/// body cannot be shared (it reads state, has effects or uses caller values).
type SharedFunctions = std::collections::HashMap<String, Option<crate::ir::ProcessFunctionId>>;

/// An expansion larger than this many arena values calls one shared copy of
/// its function unless `#[inline]` says otherwise. Small helpers (`abs`,
/// `word`) stay inline; arithmetic like `std::float`'s is shared.
const SHARE_ABOVE_VALUES: usize = 32;

/// Non-generic type aliases indexed by resolver identity.
fn source_type_aliases<'a>(
    modules: &'a [Module],
    resolved: &Resolved,
) -> std::collections::HashMap<crate::resolve::DefId, &'a ast::Type> {
    modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            ast::Item::Using(using) => match &using.kind {
                ast::UsingKind::Alias { name, ty, .. } => Some((resolved.declared(name.span)?, ty)),
                _ => None,
            },
            _ => None,
        })
        .collect()
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
            ast::Item::ExternBlock { abi, fns, .. } => functions.insert_extern(abi, fns),
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
        // A trait's methods also under `Owner#Trait`: two traits' `ping`
        // share one `Owner::ping` key, and `A::ping(f)` called `B`'s (#192).
        let trait_key = implementation
            .trait_
            .as_ref()
            .and_then(|path| functions.trait_path_key(path));
        for item in &implementation.items {
            if let ast::ImplItem::Fn(function) = item {
                functions.insert_associated(format!("{owner}::{}", function.name.text), function);
                if let Some(trait_key) = &trait_key {
                    functions.insert_associated(
                        format!("{owner}#{trait_key}::{}", function.name.text),
                        function,
                    );
                }
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
        ast::Type::Tuple { .. } => None,
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
        "natural" => Some(crate::types::Ty::Natural),
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
/// The sized vector a field-less struct wraps (`struct W(Logic[7..0])`):
/// written without an index, `W` is that width and range, as a signal of it
/// already is.
fn sized_struct_base<'a>(
    definition: crate::resolve::DefId,
    context: &LoweringContext<'a>,
) -> Option<&'a ast::Type> {
    context
        .modules
        .iter()
        .flat_map(|module| &module.items)
        .find_map(|item| {
            let ast::Item::Struct(declaration) = item else {
                return None;
            };
            (declaration.fields.is_empty()
                && context.resolved.declared(declaration.name.span) == Some(definition))
            .then_some(declaration.base.as_ref())
            .flatten()
            .filter(|base| matches!(base, ast::Type::Indexed { index: Some(_), .. }))
        })
}

fn process_declared_type(
    ty: &ast::Type,
    context: &LoweringContext<'_>,
) -> Option<crate::types::Ty> {
    process_declared_type_inner(ty, context, &mut std::collections::HashSet::new())
}

fn process_declared_type_inner(
    ty: &ast::Type,
    context: &LoweringContext<'_>,
    aliases: &mut std::collections::HashSet<crate::resolve::DefId>,
) -> Option<crate::types::Ty> {
    match ty {
        ast::Type::Path(path) => {
            if let Some((definition, aliased)) =
                context.resolved.resolved(path.span).and_then(|definition| {
                    context
                        .type_aliases
                        .get(&definition)
                        .map(|ty| (definition, *ty))
                })
            {
                if !aliases.insert(definition) {
                    return None;
                }
                let result = process_declared_type_inner(aliased, context, aliases);
                aliases.remove(&definition);
                return result;
            }
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
            // A struct over a sized vector has that vector's length.
            let len = context
                .resolved
                .resolved(path.span)
                .and_then(|definition| sized_struct_base(definition, context))
                .and_then(|base| process_declared_type_inner(base, context, aliases))
                .and_then(|base| match base {
                    crate::types::Ty::Array { len, .. } => Some(len),
                    _ => None,
                })
                .unwrap_or(0);
            family.map(|family| {
                let element = context
                    .design
                    .array_element_of_family
                    .get(&family)
                    .and_then(|element| nominal_type_from_name(element, context.resolved))
                    .unwrap_or(crate::types::Ty::Error);
                crate::types::Ty::Array {
                    elem: Box::new(element),
                    len,
                    family: Some(family),
                    low: 0,
                }
            })
        }
        .or_else(|| declared_process_type(ty, context.resolved)),
        ast::Type::Indexed { base, index, .. } => {
            let base = process_declared_type_inner(base, context, aliases)?;
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
                    ..
                } => Some(crate::types::Ty::Array {
                    elem,
                    len: length,
                    family: Some(family),
                    low: 0,
                }),
                // A sized index fills an unconstrained array's hole
                // (`string[5]` is `Char[5]`); `[]` nests (`integer[][]`).
                crate::types::Ty::Array {
                    elem,
                    len: 0,
                    family: None,
                    ..
                } if index.is_some() => Some(crate::types::Ty::Array {
                    elem,
                    len: length,
                    family: None,
                    low: 0,
                }),
                element => Some(crate::types::Ty::Array {
                    elem: Box::new(element),
                    len: length,
                    family: None,
                    low: 0,
                }),
            }
        }
        ast::Type::Generic { base, args, .. } if tuple_type_base(base).is_some() => args
            .iter()
            .map(|arg| process_declared_type_inner(&arg.as_type()?, context, aliases))
            .collect::<Option<Vec<_>>>()
            .map(crate::types::Ty::Tuple),
        ast::Type::Generic { base, .. } => process_declared_type_inner(base, context, aliases),
        ast::Type::View { target, .. } => process_declared_type_inner(target, context, aliases),
        // Rewritten to `(,)<..>` before lowering (`types::lower_tuples`).
        ast::Type::Tuple { .. } => None,
    }
}

/// The arity of a lowered tuple type's base, `(,)` in `(,)<A, B>`.
fn tuple_type_base(base: &ast::Type) -> Option<usize> {
    match base {
        ast::Type::Path(path) => match path.segments.as_slice() {
            [name] => crate::types::tuple_struct_arity(&name.text),
            _ => None,
        },
        _ => None,
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
            (resolved.qualified_name_str(definition) == Some(name)).then_some(definition)
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
                crate::ir::ScalarDomain::Natural => Some(crate::types::Ty::Natural),
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
            low: 0,
        }),
        LayoutKind::Array { range, element } => Some(crate::types::Ty::Array {
            elem: Box::new(process_type_from_layout(element, resolved)?),
            len: range
                .and_then(|range| range.len())
                .and_then(|length| u32::try_from(length).ok())
                .unwrap_or(0),
            family: None,
            low: 0,
        }),
        LayoutKind::Struct { name, fields, .. }
            if crate::types::tuple_struct_arity(name).is_some() =>
        {
            fields
                .iter()
                .map(|field| process_type_from_layout(&field.layout, resolved))
                .collect::<Option<Vec<_>>>()
                .map(crate::types::Ty::Tuple)
        }
        LayoutKind::Struct { name, .. } => nominal_type_from_name(name, resolved),
        LayoutKind::Opaque { .. } => None,
    }
}

/// Apply a checked representation to an incomplete declaration layout.
/// Qualified scalar aliases may be opaque at elaboration, while an inferred
/// string length is absent from its unconstrained declaration. Preserve any
/// complete source layout, including constrained widths and written ranges.
fn process_layout_with_type(
    layout: &crate::ir::SourceLayout,
    ty: Option<&crate::types::Ty>,
) -> crate::ir::SourceLayout {
    let mut concrete = layout.clone();
    if matches!(concrete.kind, LayoutKind::Opaque { width: None, .. }) {
        let domain = match ty {
            Some(crate::types::Ty::Integer) => Some(crate::ir::ScalarDomain::Integer),
            Some(crate::types::Ty::Natural) => Some(crate::ir::ScalarDomain::Natural),
            Some(crate::types::Ty::Real) => Some(crate::ir::ScalarDomain::Real),
            Some(crate::types::Ty::Char) => Some(crate::ir::ScalarDomain::Character),
            _ => None,
        };
        if let Some(domain) = domain {
            concrete.kind = LayoutKind::Scalar {
                width: ty
                    .and_then(crate::types::Ty::bit_width)
                    .expect("scalar width"),
                domain,
                nominal: None,
                value_range: None,
            };
        }
    }
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
        crate::types::Ty::Natural => LayoutKind::Scalar {
            width: 64,
            domain: crate::ir::ScalarDomain::Natural,
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
            ..
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
            ..
        } => LayoutKind::Array {
            range: (*len != 0).then(|| crate::ir::LayoutRange {
                left: 0,
                right: i64::from(*len) - 1,
            }),
            element: Box::new(process_layout_for_type(elem, span, context)?),
        },
        crate::types::Ty::Named(definition) => {
            let info = context.resolved.def(*definition)?;
            if let Some(aliased) = context.type_aliases.get(definition) {
                let concrete = process_declared_type(aliased, context)?;
                return process_layout_for_type(&concrete, span, context);
            }
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
                if let Some(layout) = context.design.type_layouts.get(definition) {
                    return Some(layout.clone());
                }
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
        crate::types::Ty::Tuple(elems) => LayoutKind::Struct {
            name: crate::types::tuple_struct_name(elems.len()),
            view: None,
            fields: elems
                .iter()
                .enumerate()
                .map(|(index, elem)| {
                    Some(crate::ir::LayoutField {
                        name: index.to_string(),
                        direction: None,
                        layout: process_layout_for_type(elem, span, context)?,
                    })
                })
                .collect::<Option<Vec<_>>>()?,
        },
        crate::types::Ty::Void | crate::types::Ty::Error => return None,
    };
    Some(crate::ir::SourceLayout { span, kind })
}

fn process_aggregate_layout_for_type(
    ty: &crate::types::Ty,
    span: crate::diag::Span,
    context: &LoweringContext<'_>,
) -> Option<crate::ir::SourceLayout> {
    let aggregate = matches!(
        ty,
        crate::types::Ty::Array { .. } | crate::types::Ty::Tuple(_)
    ) || matches!(ty, crate::types::Ty::Named(definition)
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
                    function: function.clone(),
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
                ast::BinOp::Rem => left.checked_rem(right),
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
                ast::BinOp::Rem => Some(left % right),
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
        // `1.5ns`: an integer-domain time suffix over a fraction is exact
        // in femtoseconds, 1_500_000. It reached the backend unscaled and
        // did not lower (#247).
        SuffixDomain::Integer if text.contains('.') => {
            let scaled = text.replace('_', "").parse::<f64>().ok()?
                * crate::syntax::ast::suffix_scale(symbol)? as f64;
            (scaled.fract() == 0.0 && scaled >= 0.0 && scaled < u64::MAX as f64)
                .then(|| ProcessNumber::Integer(vec![scaled as u64]))
        }
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

/// Attach typed procedural behavior and an optional native-test plan to the
/// canonical hardware process product already built by source lowering.
///
/// One explicit test process becomes one CFG. Legacy impl-scope test statements
/// remain one implicit foreground process so their existing sequential/`await`
/// behavior is preserved until the syntax is retired. Hardware CFGs retain
/// their existing values, control flow, and ownership for every compiler output.
pub fn lower(
    modules: &[Module],
    resolved: &Resolved,
    typed: &Typed,
    hierarchy: &Hierarchy,
    plan: Option<&TestPlan>,
    design: &mut Design,
    sink: &mut crate::diag::DiagnosticSink,
) -> Result<(), String> {
    // Hardware CFGs already exist from source hardware lowering. Retain their
    // arena and ownership; lowering stimulus must never reconstruct hardware
    // from a scheduler view. Procedural entries stay before hardware entries
    // to preserve the runtime's foreground/clock dispatch order.
    let mut process_ir = std::mem::take(&mut design.process_ir);
    let hardware_processes = std::mem::take(&mut process_ir.processes);
    let suffixes = constant_suffixes(modules, resolved);
    let constants = source_constants(modules, resolved);
    let type_aliases = source_type_aliases(modules, resolved);
    let shared_functions = std::cell::RefCell::new(SharedFunctions::new());
    let recursive_calls = std::cell::RefCell::new(Vec::new());
    let rejected = std::cell::RefCell::new(Vec::new());
    let self_calls = std::cell::RefCell::new(std::collections::HashSet::new());
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

        // Initializers are ordinary canonical CFGs with an earlier activation
        // boundary. Their locals, returned calls and suspension are real state,
        // never an expression evaluated against a fake process id.
        let mut initializer_process = ProcessCfg {
            id: ProcessId(process_ir.processes.len() as u32),
            root: test.root,
            owner: test.root,
            label: Some(format!("{root_path}::<initializers>")),
            span: test.span,
            activation: ProcessActivation::Initialization,
            region: ProcessRegion::Procedural,
            entry: ProcessBlockId(0),
            inline_calls: Vec::new(),
            locals: Vec::new(),
            blocks: vec![ProcessBlock::empty(ProcessBlockId(0))],
        };
        let initializers = items
            .iter()
            .filter_map(|item| match item {
                ImplItem::Let(declaration) if declaration.value.is_some() => Some(declaration),
                _ => None,
            })
            .filter(|declaration| {
                resolved
                    .declared(declaration.name.span)
                    .is_some_and(|definition| {
                        !ordered_initializers.contains(&definition)
                            && process_ir.storages.iter().any(|storage| {
                                storage.owner == test.root && storage.source == Some(definition)
                            })
                    })
            })
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
                type_aliases: &type_aliases,
                constant_stack: std::collections::HashSet::new(),
                functions: &functions,
                constant_integers: &constant_integers,
                value_bindings: Vec::new(),
                self_formats: Vec::new(),

                widening: std::collections::HashSet::new(),
                inline_self_values: Vec::new(),
                inline_return_types: Vec::new(),
                inline_return_blocks: Vec::new(),
                inline_functions: std::collections::HashSet::new(),
                inline_scope: None,
                inline_value_locals: Vec::new(),
                inline_value_branches: Vec::new(),
                cfg_call_cache: Vec::new(),
                shared_functions: &shared_functions,
                recursive_calls: &recursive_calls,
                rejected: &rejected,
                self_calls: &self_calls,
                frames: Vec::new(),
                frame_functions: Default::default(),
            };
            let mut block = ProcessBlockId(0);
            for declaration in initializers {
                block = lower_ordered_storage_initializer(
                    declaration,
                    &mut context,
                    &mut initializer_process,
                    block,
                )
                .expect("registered initializer storage");
            }
            cfg_calls::lower_value_calls(&mut initializer_process, &mut context);
        }
        if !initializer_process.blocks[0].instructions.is_empty()
            || initializer_process.blocks.len() > 1
        {
            test_processes.push(initializer_process.id);
            process_ir.processes.push(initializer_process);
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
                            type_aliases: &type_aliases,
                            constant_stack: std::collections::HashSet::new(),
                            functions: &functions,
                            constant_integers: &constant_integers,
                            value_bindings: Vec::new(),
                            self_formats: Vec::new(),

                            widening: std::collections::HashSet::new(),
                            inline_self_values: Vec::new(),
                            inline_return_types: Vec::new(),
                            inline_return_blocks: Vec::new(),
                            inline_functions: std::collections::HashSet::new(),
                            inline_scope: None,
                            inline_value_locals: Vec::new(),
                            inline_value_branches: Vec::new(),
                            cfg_call_cache: Vec::new(),
                            shared_functions: &shared_functions,
                            recursive_calls: &recursive_calls,
                            rejected: &rejected,
                            self_calls: &self_calls,
                            frames: Vec::new(),
                            frame_functions: Default::default(),
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
                            type_aliases: &type_aliases,
                            constant_stack: std::collections::HashSet::new(),
                            functions: &functions,
                            constant_integers: &constant_integers,
                            value_bindings: Vec::new(),
                            self_formats: Vec::new(),

                            widening: std::collections::HashSet::new(),
                            inline_self_values: Vec::new(),
                            inline_return_types: Vec::new(),
                            inline_return_blocks: Vec::new(),
                            inline_functions: std::collections::HashSet::new(),
                            inline_scope: None,
                            inline_value_locals: Vec::new(),
                            inline_value_branches: Vec::new(),
                            cfg_call_cache: Vec::new(),
                            shared_functions: &shared_functions,
                            recursive_calls: &recursive_calls,
                            rejected: &rejected,
                            self_calls: &self_calls,
                            frames: Vec::new(),
                            frame_functions: Default::default(),
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
                | ImplItem::AttrBinding(_)
                | ImplItem::Attr(_) => {}
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
                    type_aliases: &type_aliases,
                    constant_stack: std::collections::HashSet::new(),
                    functions: &functions,
                    constant_integers: &constant_integers,
                    value_bindings: Vec::new(),
                    self_formats: Vec::new(),

                    widening: std::collections::HashSet::new(),
                    inline_self_values: Vec::new(),
                    inline_return_types: Vec::new(),
                    inline_return_blocks: Vec::new(),
                    inline_functions: std::collections::HashSet::new(),
                    inline_scope: None,
                    inline_value_locals: Vec::new(),
                    inline_value_branches: Vec::new(),
                    cfg_call_cache: Vec::new(),
                    shared_functions: &shared_functions,
                    recursive_calls: &recursive_calls,
                    rejected: &rejected,
                    self_calls: &self_calls,
                    frames: Vec::new(),
                    frame_functions: Default::default(),
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

    let hardware_offset = process_ir.processes.len() as u32;
    for mut process in hardware_processes {
        process.id = ProcessId(process.id.0 + hardware_offset);
        if let Some(test) = process_ir
            .tests
            .iter_mut()
            .find(|test| test.root == process.root)
        {
            test.processes.push(process.id);
        }
        process_ir.processes.push(process);
    }

    // Hardware items whose loops do not unroll run as procedural CFGs,
    // re-evaluated whenever a signal they read changes.
    for item in &design.procedural_items {
        let Some((root, owner)) = instance_at(hierarchy, &item.path) else {
            continue;
        };
        let entity = hierarchy.instance(owner).entity_id;
        let items = crate::testbench::implementation_items(modules, resolved, entity);
        let statements = items.iter().find_map(|candidate| match candidate {
            ImplItem::Process(process) if process.span == item.span => {
                Some(process.body.stmts.as_slice())
            }
            ImplItem::Stmt(statement) if ast::stmt_span(statement) == item.span => {
                Some(std::slice::from_ref(statement))
            }
            _ => None,
        });
        let Some(statements) = statements else {
            continue;
        };
        let id = ProcessId(process_ir.processes.len() as u32);
        // The instance's parameters fold as constants (`unsigned[W]`).
        let mut instance_integers = constant_integers.clone();
        for (name, value) in &hierarchy.instance(owner).params {
            if let crate::elab::ParamValue::Int(value) = value {
                instance_integers.insert(name.clone(), *value);
            }
        }
        let mut process = {
            let mut context = LoweringContext {
                modules,
                resolved,
                typed,
                design,
                hierarchy,
                root_path: &item.path,
                process_ir: &mut process_ir,
                suffixes: &suffixes,
                constants: &constants,
                type_aliases: &type_aliases,
                constant_stack: std::collections::HashSet::new(),
                functions: &functions,
                constant_integers: &instance_integers,
                value_bindings: Vec::new(),
                self_formats: Vec::new(),

                widening: std::collections::HashSet::new(),
                inline_self_values: Vec::new(),
                inline_return_types: Vec::new(),
                inline_return_blocks: Vec::new(),
                inline_functions: std::collections::HashSet::new(),
                inline_scope: None,
                inline_value_locals: Vec::new(),
                inline_value_branches: Vec::new(),
                cfg_call_cache: Vec::new(),
                shared_functions: &shared_functions,
                recursive_calls: &recursive_calls,
                rejected: &rejected,
                self_calls: &self_calls,
                frames: Vec::new(),
                frame_functions: Default::default(),
            };
            lower_process(
                id,
                owner,
                Some(
                    item.label
                        .clone()
                        .unwrap_or_else(|| format!("{}::<procedural:{}>", item.path, id.0)),
                ),
                item.span,
                // Reactive from the start: a hardware write does not settle.
                ProcessActivation::Reactive {
                    sensitivity: Vec::new(),
                },
                statements,
                &mut context,
            )
        };
        process.root = root;
        process.activation = ProcessActivation::Reactive {
            sensitivity: procedural_sensitivity(&process, &process_ir),
        };
        if let Some(test) = process_ir.tests.iter_mut().find(|test| test.root == root) {
            test.processes.push(id);
        }
        process_ir.processes.push(process);
    }

    for diagnostic in rejected.into_inner() {
        sink.emit(diagnostic);
    }
    let mut reported = std::collections::HashSet::new();
    for (name, call, function, polymorphic) in recursive_calls.into_inner() {
        if polymorphic && reported.insert(function) {
            sink.emit(
                crate::diag::Diagnostic::error(format!(
                    "`{name}` calls itself with arguments of other types, so it has no finite expansion"
                ))
                .with_code(crate::diag::codes::RECURSIVE_CALL)
                .at(call)
                .label(function, "this function")
                .help(
                    "each call would need a new copy of the function for its new argument \
                     types, without end; recurse with the types it was called with",
                ),
            );
        } else if reported.insert(function) {
            sink.emit(
                crate::diag::Diagnostic::error(format!(
                    "`{name}` calls itself, directly or through other calls, and cannot keep a frame"
                ))
                .with_code(crate::diag::codes::RECURSIVE_CALL)
                .at(call)
                .label(function, "this function")
                .help(
                    "a recursive testbench function keeps one frame per call and copies a written \
                     parameter back to its argument on return, which this version does for a \
                     local argument only; pass a local, or write the recursion as a loop",
                ),
            );
        }
    }

    // Backends look calls up by value (`ProcessIr::call_of`).
    process_ir.calls.sort_unstable_by_key(|call| call.value.0);
    design.process_ir = process_ir;
    crate::ir::derive_scheduler_forms(design).map_err(|error| error.message)
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
        // The initializer's type stands in for an unsized declaration, but a
        // kernel `integer` initializer (`0 - 5`) does not retype a declared
        // vector: a `signed[65]` stored as `integer` read its initializer at
        // 64 bits.
        let from_layout = process_type_from_layout(layout, resolved);
        let ty = declared_nominal_type(declaration.ty.as_ref(), resolved)
            .or_else(|| {
                declaration
                    .value
                    .as_ref()
                    .and_then(|value| typed.expr_type(ast::expr_span(value)))
                    .filter(|ty| !matches!(ty, crate::types::Ty::Error))
                    .filter(|ty| {
                        !matches!(ty, crate::types::Ty::Integer)
                            || from_layout
                                .as_ref()
                                .is_none_or(|layout| matches!(layout, crate::types::Ty::Integer))
                    })
                    .cloned()
            })
            .or(from_layout);
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
pub(super) fn testbench_bindings(
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
            // A slice or computed index (rendered `[<expr>]`) is no storage
            // projection: an input reads it as a value.
            if storage_prefix.contains("<expr>") {
                continue;
            }
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
    // Several outputs onto one projection read their shared, resolved net.
    let shared: Vec<(String, SignalId)> = bindings
        .iter()
        .filter(|binding| matches!(binding.direction, LayoutDirection::Out))
        .filter_map(|binding| {
            let net = design
                .testbench_nets
                .get(&format!("{root_path}.{storage}{}", binding.projection))?;
            Some((binding.projection.clone(), *net))
        })
        .collect();
    for (projection, net) in shared {
        bindings.retain(|binding| {
            binding.projection != projection || !matches!(binding.direction, LayoutDirection::Out)
        });
        bindings.push(ProcessStorageBinding {
            projection,
            signal: net,
            direction: LayoutDirection::Out,
        });
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
        region: ProcessRegion::Procedural,
        entry: ProcessBlockId(0),
        inline_calls: Vec::new(),
        locals: Vec::new(),
        blocks: vec![ProcessBlock::empty(ProcessBlockId(0))],
    };
    lower_statements(statements, context, &mut process, ProcessBlockId(0));
    cfg_calls::lower_value_calls(&mut process, context);
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
        region: ProcessRegion::Procedural,
        entry: ProcessBlockId(0),
        inline_calls: Vec::new(),
        locals: Vec::new(),
        blocks: vec![ProcessBlock::empty(ProcessBlockId(0))],
    };
    let entity = context.hierarchy.instance(owner).entity_id;
    let all_items =
        crate::testbench::implementation_items(context.modules, context.resolved, entity);
    let instance_arrays: std::collections::HashSet<String> = all_items
        .iter()
        .filter_map(|item| match item {
            ImplItem::Let(declaration)
                if matches!(declaration.ty, Some(ast::Type::Indexed { .. }))
                    && declaration_is_entity(declaration, context.resolved) =>
            {
                Some(declaration.name.text.clone())
            }
            _ => None,
        })
        .collect();
    let mut current = Some(ProcessBlockId(0));
    for item in items {
        let Some(block) = current else { break };
        current = match item {
            ImplItem::Stmt(statement) if !instance_arrays.is_empty() => {
                let mut block = Some(block);
                for statement in instance_array_statements(statement, &instance_arrays) {
                    let Some(at) = block else { break };
                    block = lower_statement(&statement, context, &mut process, at);
                }
                block
            }
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
    cfg_calls::lower_value_calls(&mut process, context);
    process
}

/// A testbench's instance arrays (`let stage: Cell[N];`) are hardware, so a
/// `for` over constant bounds that touches one is a generate loop: unrolled,
/// each slot named by its index. Building a slot (`stage[i] = Cell { .din =
/// x }`, or `= { .. }`) writes the ports it names (#290).
fn instance_array_statements(
    statement: &Stmt,
    arrays: &std::collections::HashSet<String>,
) -> Vec<Stmt> {
    let touches = |statement: &Stmt| {
        let text = crate::syntax::pretty::print_stmts(std::slice::from_ref(statement));
        arrays.iter().any(|name| text.contains(&format!("{name}[")))
    };
    match statement {
        Stmt::For {
            var,
            range: ast::Expr::Range { lo, hi, .. },
            body,
            ..
        } if touches(statement) => {
            let (Some(left), Some(right)) = (literal_arithmetic(lo), literal_arithmetic(hi)) else {
                return vec![statement.clone()];
            };
            let (Ok(left), Ok(right)) = (i64::try_from(left), i64::try_from(right)) else {
                return vec![statement.clone()];
            };
            crate::ir::lower_helpers::loop_range(left, right)
                .into_iter()
                .flat_map(|index| {
                    body.stmts.iter().map(move |inner| {
                        crate::ir::lower_helpers::subst_stmt(inner, &var.text, index)
                    })
                })
                .flat_map(|inner| instance_array_statements(&inner, arrays))
                .collect()
        }
        Stmt::Assign {
            target,
            value: ast::Expr::Construct { args, .. },
            span,
            ..
        } if expr_path_root(target).is_some_and(|root| arrays.contains(&root)) => args
            .iter()
            .filter_map(|argument| {
                Some(Stmt::Assign {
                    label: None,
                    target: ast::Expr::Field {
                        base: Box::new(target.clone()),
                        field: argument.field.clone()?,
                        span: *span,
                    },
                    value: argument.value.clone()?,
                    after: None,
                    span: *span,
                })
            })
            .collect(),
        // `stage[i] = {}` lexes as an empty concatenation: nothing to wire.
        Stmt::Assign {
            target,
            value: ast::Expr::Concat { parts, .. },
            ..
        } if parts.is_empty()
            && expr_path_root(target).is_some_and(|root| arrays.contains(&root)) =>
        {
            Vec::new()
        }
        _ => vec![statement.clone()],
    }
}

/// The name an access path starts from: `stage` of `stage[1].din`.
fn expr_path_root(expression: &ast::Expr) -> Option<String> {
    match expression {
        ast::Expr::Path(path) if path.segments.len() == 1 => Some(path.segments[0].text.clone()),
        ast::Expr::Field { base, .. } | ast::Expr::Index { base, .. } => expr_path_root(base),
        _ => None,
    }
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
        ast::Type::Tuple { .. } => None,
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
        ast::Expr::Concat { parts, .. } | ast::Expr::Tuple { elems: parts, .. } => parts
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
        // A slice or computed index (`x[3..0]`, `x[i]`) is a value, read at
        // the connection as `x[3..0] + 0` is: bound as storage, it did not
        // lower.
        ast::Expr::Index { index, .. } if !matches!(index.as_ref(), ast::Expr::Int { .. }) => false,
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
    if matches!(process.activation, ProcessActivation::Initialization) {
        context.process_ir.storages[storage.0 as usize].initializer = Some(value);
    }
    // A literal empty string has no elements to write. Retain its source/type
    // metadata for formatting and equality without turning it into a packed
    // assignment or an invalid zero runtime-string handle. Runtime reads have
    // a real handle and must still execute, even when the loaded file is empty.
    if matches!(
        ty.as_ref(),
        Some(crate::types::Ty::Array { elem, len: 0, family: None, .. })
            if matches!(elem.as_ref(), crate::types::Ty::Char)
    ) && matches!(
        &context.process_ir.values[value.0 as usize].kind,
        ProcessValueKind::String(text) if text.is_empty()
    ) {
        return Some(block);
    }
    process.blocks[block.0 as usize]
        .instructions
        .push(ProcessInstruction::Assign {
            semantics: ProcessAssignment::ImmediateStorage,
            driver_context: None,
            target,
            value,
            span: declaration.span,
        });
    if !drives_design || matches!(process.activation, ProcessActivation::Initialization) {
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
    let first_local = process.locals.len();
    let mut current = Some(entry);
    for statement in statements {
        let Some(block) = current else { break };
        current = lower_statement(statement, context, process, block);
    }
    if let (Some(first), Some(last)) = (statements.first(), statements.last()) {
        let extent = ast::stmt_span(first).to(ast::stmt_span(last));
        for local in &mut process.locals[first_local..] {
            // Nested blocks record their smaller extent before returning.
            if local.source.is_some() && local.scope.is_none() {
                local.scope = Some(extent);
            }
        }
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
        // `let b: Box<(integer, integer)> = { .val = (10, 20) }`: a generic
        // struct's construct has no field types to give its values, but the
        // local is laid out from its declaration (#196). Declared bare and
        // written field by field, a tuple field value lowers (#227).
        Stmt::Let(declaration)
            if declaration.ty.as_ref().is_some_and(|ty| {
                generic_struct_layout(ty, declaration.span, context).is_some()
            }) && matches!(
                &declaration.value,
                Some(ast::Expr::Construct { args, spread: None, .. })
                    if !args.is_empty() && args.iter().all(|arg| arg.field.is_some() && arg.value.is_some())
            ) =>
        {
            let Some(ast::Expr::Construct { args, .. }) = &declaration.value else {
                unreachable!()
            };
            let bare = Stmt::Let(ast::LetDecl {
                value: None,
                ..declaration.clone()
            });
            let block = lower_statement(&bare, context, process, block)?;
            let local = ProcessLocalId(process.locals.len().checked_sub(1)? as u32);
            let layout = process.locals[local.0 as usize].layout.clone()?;
            let LayoutKind::Struct { fields, .. } = &layout.kind else {
                return None;
            };
            for arg in args {
                let (Some(field), Some(value)) = (&arg.field, &arg.value) else {
                    continue;
                };
                let field_layout = fields.iter().find(|f| f.name == field.text)?.layout.clone();
                let field_type = process_type_from_layout(&field_layout, context.resolved);
                let base = cfg_calls::local_value(local, process, context);
                let place = push_value(
                    arg.span,
                    field_type.clone(),
                    field_layout.packed_width(),
                    ProcessValueKind::Field {
                        base,
                        field: field.text.clone(),
                    },
                    context,
                );
                context.process_ir.value_layouts[place.0 as usize] = Some(field_layout);
                let value = value_ref_with_type(value, process, context, field_type.as_ref());
                let semantics = process_place_assignment(place, context.process_ir)?;
                process.blocks[block.0 as usize]
                    .instructions
                    .push(ProcessInstruction::Assign {
                        semantics,
                        driver_context: None,
                        target: place,
                        value,
                        span: arg.span,
                    });
            }

            Some(block)
        }
        Stmt::Let(declaration) => {
            let sized = sized_by_value(declaration, process, context);
            let declaration = sized.as_ref().unwrap_or(declaration);
            let local = push_local(process, declaration, context);
            let target = process.locals[local.0 as usize].ty.clone();
            let initializer = match declaration.value.as_ref() {
                Some(value) => {
                    let value = value_ref_with_type(value, process, context, target.as_ref());
                    Some(runtime_array_copy(value, target.as_ref(), context))
                }
                // An unset runtime array or string is the empty one, handle
                // zero.
                None if matches!(
                    target,
                    Some(crate::types::Ty::Array {
                        len: 0,
                        family: None,
                        ..
                    })
                ) =>
                {
                    Some(push_value(
                        declaration.span,
                        target.clone(),
                        Some(64),
                        ProcessValueKind::Number(ProcessNumber::Integer(vec![0])),
                        context,
                    ))
                }
                None => None,
            };
            // A width-generic declaration (`let r: unsigned = v;` in a
            // function over `unsigned`) takes its shape from the initializer.
            if process.locals[local.0 as usize]
                .layout
                .as_ref()
                .and_then(SourceLayout::packed_width)
                .is_none()
            {
                if let Some(layout) = initializer
                    .and_then(|value| operand_source_layout(value, process, context))
                    .filter(|layout| layout.packed_width().is_some())
                {
                    process.locals[local.0 as usize].layout = Some(layout);
                }
            }
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
            target: target @ ast::Expr::Tuple { .. },
            value,
            after,
            span,
            ..
        } => lower_tuple_assign(
            target,
            value,
            after.as_ref(),
            *span,
            context,
            process,
            block,
        ),
        // `regs[1] = v` on a struct's own `IndexAssign` contract is the call
        // `regs.index_assign(1, v)`; as an element write it did not lower.
        Stmt::Assign {
            target: ast::Expr::Index { base, index, .. },
            value,
            after: None,
            span,
            ..
        } if index_assign_contract(base, context) => {
            let call = Stmt::Expr(ast::Expr::Call {
                callee: Box::new(ast::Expr::Field {
                    base: base.clone(),
                    field: ast::Ident {
                        text: "index_assign".to_string(),
                        span: *span,
                    },
                    span: *span,
                }),
                type_args: Vec::new(),
                qualifier: None,
                args: vec![index.as_ref().clone(), value.clone()],
                bang: false,
                span: *span,
            });
            lower_statement(&call, context, process, block)
        }
        Stmt::Assign {
            target,
            value,
            after,
            span,
            ..
        } => {
            if after.is_none() {
                if let Some(elements) = array_slice_elements(target, value, *span, process, context)
                {
                    let mut block = block;
                    for element in &elements {
                        block = lower_statement(element, context, process, block)?;
                    }
                    return Some(block);
                }
            }
            assign_process_value(
                target,
                AssignedValue::Source(value),
                after.as_ref(),
                *span,
                context,
                process,
                block,
            )
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
        } => {
            // `for x in []` / `for c in ""` never runs its body. With no
            // elements the loop had no shape and did not lower (#200).
            if matches!(range, ast::Expr::Array { elems, .. } if elems.is_empty())
                || matches!(range, ast::Expr::StrLit { text, .. } if text.is_empty())
            {
                return Some(block);
            }
            lower_for(var, range, body, *span, context, process, block)
        }
        Stmt::Return { value, span } => {
            if let Some((resume, target)) = context.inline_return_blocks.last().copied() {
                if let (Some(target), Some(value)) = (target, value) {
                    let ty = context.inline_return_types.last().cloned().flatten();
                    let value = value_ref_with_type(value, process, context, ty.as_ref());
                    // `-> Logic[]`: the result has the shape the body returns.
                    adopt_returned_layout(target, value, process, context);
                    process.blocks[block.0 as usize].instructions.push(
                        ProcessInstruction::Assign {
                            semantics: ProcessAssignment::ImmediateLocal,
                            driver_context: None,
                            target,
                            value,
                            span: *span,
                        },
                    );
                }
                process.blocks[block.0 as usize].terminator = ProcessTerminator::Goto(resume);
                return None;
            }
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

/// `r[3..0] = v` on an array local, at constant bounds and from a named
/// array: one element assignment per index, paired in written order. A
/// range place has no single storage width, so it is written element-wise.
fn array_slice_elements(
    target: &ast::Expr,
    value: &ast::Expr,
    span: crate::diag::Span,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<Vec<Stmt>> {
    let ast::Expr::Index {
        base,
        index,
        span: target_span,
    } = target
    else {
        return None;
    };
    let ast::Expr::Range { lo, hi, .. } = index.as_ref() else {
        return None;
    };
    let base_value = value_ref(base, process, context);
    if !matches!(
        operand_source_layout(base_value, process, context)?.kind,
        LayoutKind::Array { .. }
    ) {
        return None;
    }
    let integer = Some(&crate::types::Ty::Integer);
    let left = value_ref_with_type(lo, process, context, integer);
    let right = value_ref_with_type(hi, process, context, integer);
    let (Some(left), Some(right)) = (
        constant_integer(left, context),
        constant_integer(right, context),
    ) else {
        // `arr[hi..lo] = v` with run-time bounds has no element list; as a
        // whole write it reached code generation with no width (#219).
        reject_runtime_slice(target, index, span, process, context);
        return Some(Vec::new());
    };
    let label = |n: i64, span| ast::Expr::Int {
        text: n.to_string(),
        span,
    };
    // The checker typed these spans as the whole slice and array; empty
    // spans at their ends carry no checked type, so each element is typed
    // from its value.
    let empty = |span: crate::diag::Span| crate::diag::Span {
        start: span.end,
        ..span
    };
    let targets = crate::ir::loop_range(left, right);
    let value_span = empty(ast::expr_span(value));
    // Each element of the value, as an expression: `[a, b]` is its elements,
    // `other[3..4]` and a whole array `v` are indexed. Only a whole array
    // was, and any other value lowered to a slice write nothing could run
    // (#170, #185).
    let indexed = |source: &ast::Expr, labels: Vec<i64>| {
        labels
            .into_iter()
            .map(|from| ast::Expr::Index {
                base: Box::new(source.clone()),
                index: Box::new(label(from, value_span)),
                span: value_span,
            })
            .collect::<Vec<_>>()
    };
    let (elements, source_base) = match value {
        ast::Expr::Array { elems, .. } => (elems.clone(), None),
        ast::Expr::Index {
            base: source,
            index: source_index,
            ..
        } => {
            let ast::Expr::Range { lo, hi, .. } = source_index.as_ref() else {
                return None;
            };
            let source_value = value_ref(source, process, context);
            if !matches!(
                operand_source_layout(source_value, process, context)?.kind,
                LayoutKind::Array { .. }
            ) {
                return None;
            }
            let left = value_ref_with_type(lo, process, context, integer);
            let right = value_ref_with_type(hi, process, context, integer);
            let labels = crate::ir::loop_range(
                constant_integer(left, context)?,
                constant_integer(right, context)?,
            );
            (indexed(source, labels), Some(source.as_ref()))
        }
        ast::Expr::Path(path) if !path.segments.is_empty() => {
            let source_value = value_ref(value, process, context);
            let source = operand_source_layout(source_value, process, context)?.index_range()?;
            (
                indexed(value, crate::ir::loop_range(source.left, source.right)),
                Some(value),
            )
        }
        _ => return None,
    };
    if targets.len() != elements.len() || targets.iter().any(|label| *label < 0) {
        return None;
    }
    let target_span = empty(*target_span);
    let mut pairs = targets.into_iter().zip(elements).collect::<Vec<_>>();
    // `a[1..2] = a[0..1]` copies within one array: element by element it
    // must go from the far end, or it reads what it just wrote.
    let same_array = |a: &ast::Expr, b: &ast::Expr| match (a, b) {
        (ast::Expr::Path(a), ast::Expr::Path(b)) => a
            .segments
            .iter()
            .map(|s| &s.text)
            .eq(b.segments.iter().map(|s| &s.text)),
        _ => false,
    };
    if let (Some(source), Some((first, ast::Expr::Index { index, .. }))) =
        (source_base, pairs.first())
    {
        let from = match index.as_ref() {
            ast::Expr::Int { text, .. } => text.parse::<i64>().ok(),
            _ => None,
        };
        if same_array(base, source) && from.is_some_and(|from| from < *first) {
            pairs.reverse();
        }
    }
    Some(
        pairs
            .into_iter()
            .map(|(to, element)| Stmt::Assign {
                label: None,
                target: ast::Expr::Index {
                    base: base.clone(),
                    index: Box::new(label(to, target_span)),
                    span: target_span,
                },
                value: element,
                after: None,
                span,
            })
            .collect(),
    )
}

/// A slice of an array with bounds known only at run time: it has no fixed
/// length to give the value (#219).
fn reject_runtime_slice(
    expression: &ast::Expr,
    index: &ast::Expr,
    span: crate::diag::Span,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) {
    let ast::Expr::Range { lo, hi, .. } = index else {
        return;
    };
    let integer = Some(&crate::types::Ty::Integer);
    let first = context.process_ir.values.len();
    let left = value_ref_with_type(lo, process, context, integer);
    let right = value_ref_with_type(hi, process, context, integer);
    let runtime =
        constant_integer(left, context).is_none() || constant_integer(right, context).is_none();
    truncate_process_values(context, first);
    if runtime {
        context.rejected.borrow_mut().push(
            crate::diag::Diagnostic::error(format!(
                "`{}` has bounds known only at run time",
                crate::syntax::pretty::expr_string(expression)
            ))
            .with_code(crate::diag::codes::UNSUPPORTED_EXPR)
            .at(span)
            .help("array slice bounds must be constant; copy the elements in a loop instead"),
        );
    }
}

/// `v == "10XZ"`: a string literal beside a vector of non-`Char` elements
/// (`Logic`, `Bit`, `unsigned`) is that vector's literal, as on assignment,
/// not a `Char` string; compared as text it was never equal. `None` leaves
/// the operands to the other rules.
fn logic_string_comparison(
    lhs: &ast::Expr,
    rhs: &ast::Expr,
    left_type: Option<&crate::types::Ty>,
    right_type: Option<&crate::types::Ty>,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<(ProcessValueId, ProcessValueId)> {
    let vector = |ty: Option<&crate::types::Ty>| {
        matches!(ty, Some(crate::types::Ty::Array { elem, .. })
            if !matches!(elem.as_ref(), crate::types::Ty::Char))
    };
    let string = |expression: &ast::Expr| matches!(expression, ast::Expr::StrLit { .. });
    if string(rhs) && !string(lhs) && vector(left_type) {
        let left = value_ref_with_type(lhs, process, context, None);
        let ty = process_value_type(left, context).or_else(|| left_type.cloned());
        return Some((
            left,
            value_ref_with_type(rhs, process, context, ty.as_ref()),
        ));
    }
    if string(lhs) && !string(rhs) && vector(right_type) {
        let right = value_ref_with_type(rhs, process, context, None);
        let ty = process_value_type(right, context).or_else(|| right_type.cloned());
        return Some((
            value_ref_with_type(lhs, process, context, ty.as_ref()),
            right,
        ));
    }
    None
}

/// Give an unsized call result (`-> Logic[]`) the layout of the value its
/// body returns, which is where the size is set.
fn adopt_returned_layout(
    target: ProcessValueId,
    value: ProcessValueId,
    process: &mut ProcessCfg,
    context: &mut LoweringContext<'_>,
) {
    let ProcessValueKind::Local { local, .. } = context.process_ir.values[target.0 as usize].kind
    else {
        return;
    };
    let sized =
        |layout: &Option<SourceLayout>| layout.as_ref().and_then(SourceLayout::bit_width).is_some();
    if sized(&process.locals[local.0 as usize].layout) {
        return;
    }
    let Some(layout) = operand_source_layout(value, process, context) else {
        return;
    };
    if layout.bit_width().is_none() {
        return;
    }
    let width = layout.packed_width();
    process.locals[local.0 as usize].layout = Some(layout.clone());
    context.process_ir.values[target.0 as usize].bit_width = width;
    context.process_ir.value_layouts[target.0 as usize] = Some(layout);
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
    // `xs.push(v)` on a runtime array: append, then keep the handle (a new
    // one when the array was empty).
    if let (ast::Expr::Field { base, field, .. }, [pushed]) = (callee, arguments) {
        let ty = context
            .typed
            .expr_type(ast::expr_span(base))
            .cloned()
            .filter(|_| field.text == "push");
        if let Some(element) = runtime_array_element(ty.as_ref()).cloned() {
            let handle = value_ref(base, process, context);
            let literal = literal_type(pushed, Some(&element));
            let pushed = value_ref_with_type(
                pushed,
                process,
                context,
                literal.as_ref().or(Some(&element)),
            );
            let pushed = if literal.is_some() {
                cfg_calls::frame_copy(pushed, process, context, block)
            } else {
                pushed
            };
            let pushed = runtime_array_copy(pushed, Some(&element), context);
            let pushed = symbol_encode(pushed, &element, span, process, context).unwrap_or(pushed);
            let appended = push_value(
                span,
                ty,
                Some(64),
                ProcessValueKind::HostCall {
                    operation: ProcessHostValueOp::ArrayPush,
                    arguments: vec![handle, pushed],
                },
                context,
            );
            // `rows[i].push(v)`: the inner array's handle goes back into
            // the outer one (it is new when the inner array was empty).
            if let ProcessValueKind::HostCall {
                operation: ProcessHostValueOp::StringIndex,
                arguments,
            } = &context.process_ir.values[handle.0 as usize].kind
            {
                let (outer, index) = (arguments[0], arguments[1]);
                let store = push_value(
                    span,
                    Some(crate::types::Ty::Integer),
                    Some(64),
                    ProcessValueKind::HostCall {
                        operation: ProcessHostValueOp::ArrayStore,
                        arguments: vec![outer, index, appended],
                    },
                    context,
                );
                cfg_calls::frame_copy(store, process, context, block);
                return Some(block);
            }
            let semantics = process_place_assignment(handle, context.process_ir)?;
            process.blocks[block.0 as usize]
                .instructions
                .push(ProcessInstruction::Assign {
                    semantics,
                    driver_context: None,
                    target: handle,
                    value: appended,
                    span,
                });
            return Some(block);
        }
    }
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
                inline_process_procedure_call(callee, arguments, span, context, process, block)
            {
                return Some(tail);
            }
            // `putchar(65);`: a C function called for its effect is the
            // call's value, taken and dropped. As a runtime statement it
            // had no lowering (#193).
            if context
                .functions
                .get(callee)
                .is_some_and(|function| function.body.is_none())
            {
                let call = ast::Expr::Call {
                    callee: Box::new(callee.clone()),
                    type_args: Vec::new(),
                    qualifier: None,
                    args: arguments.to_vec(),
                    bang: false,
                    span,
                };
                let value = value_ref(&call, process, context);
                cfg_calls::frame_copy(value, process, context, block);
                return Some(block);
            }
            let operation = match name.as_str() {
                "assert" => ProcessRuntimeOp::Assert,
                "warn" => ProcessRuntimeOp::Warn,
                "print" => ProcessRuntimeOp::Print,
                "seed" if builtin_callee_is(callee, "seed", context) => ProcessRuntimeOp::Seed,
                _ => ProcessRuntimeOp::Call(name),
            };
            let format =
                lower_process_format(&operation, arguments, &lowered_arguments, process, context);
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
/// Process writes. The existing statement lowerer handles the body, including
/// branches, loops, suspension and early returns to the caller. Recursive calls
/// remain fail-closed instead of introducing a second runtime call convention.
fn inline_process_procedure_call(
    callee: &ast::Expr,
    arguments: &[ast::Expr],
    call_span: crate::diag::Span,
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let first_value = context.process_ir.values.len();
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
                context.functions.get_associated(&owner, &field.text)?,
                Some(receiver),
            )
        }
        _ => (context.functions.get(callee)?, None),
    };
    let body = function.body.as_ref()?;
    if function.ret.is_some()
        || function.params.iter().any(|parameter| parameter.is_self) != receiver.is_some()
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

    let mut bindings = Vec::new();
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
        give_literal_parameter_layout(parameter, argument, process, context);
        bindings.push((definition, argument));
    }
    if !context.inline_functions.insert(function.span) {
        truncate_process_values(context, first_value);
        return None;
    }

    let first_block = process.blocks.len();
    let first_local = process.locals.len();
    let first_inline = process.inline_calls.len();
    let original_block = process.blocks[block.0 as usize].clone();
    // Evaluate non-place arguments and place selectors once, before callee
    // writes or suspension. Places themselves remain aliases, so later reads
    // observe immediate local writes but not uncommitted signal writes.
    let receiver = receiver.map(|value| capture_process_argument(value, context, process, block));
    for (_, value) in &mut bindings {
        *value = capture_process_argument(*value, context, process, block);
    }
    let bindings = bindings.into_iter().collect();
    let resume = process.push_block();
    let previous_scope = context.inline_scope;
    let scope = begin_inline_call(function, call_span, receiver, &bindings, process, context);
    let entry = process.push_block();
    process.blocks[block.0 as usize].terminator = ProcessTerminator::Goto(entry);
    context.value_bindings.push(bindings);
    context.inline_self_values.push(receiver);
    context.inline_return_types.push(None);
    context.inline_return_blocks.push((resume, None));
    let tail = lower_statements(&body.stmts, context, process, entry);
    finish_inline_call(scope, entry, first_local, process);
    context.inline_scope = previous_scope;
    context.inline_return_blocks.pop();
    context.inline_return_types.pop();
    context.inline_self_values.pop();
    context.value_bindings.pop();
    context.inline_functions.remove(&function.span);

    // A recursive/unknown statement call anywhere in this inline must fail
    // before its entry captures or earlier callee effects can run. Restore
    // only the caller block and newly appended frames, not the whole process.
    let unsupported = |instruction: &ProcessInstruction| {
        matches!(
            instruction,
            ProcessInstruction::Runtime {
                operation: ProcessRuntimeOp::Call(_),
                ..
            }
        )
    };
    if process.blocks[block.0 as usize].instructions[original_block.instructions.len()..]
        .iter()
        .any(unsupported)
        || process.blocks[first_block..]
            .iter()
            .any(|block| block.instructions.iter().any(unsupported))
    {
        process.blocks.truncate(first_block);
        process.locals.truncate(first_local);
        process.inline_calls.truncate(first_inline);
        process.blocks[block.0 as usize] = original_block;
        truncate_process_values(context, first_value);
        return None;
    }
    if let Some(tail) = tail {
        process.blocks[tail.0 as usize].terminator = ProcessTerminator::Goto(resume);
    }
    Some(resume)
}

fn begin_inline_call(
    function: &ast::FnDecl,
    call: crate::diag::Span,
    receiver: Option<ProcessValueId>,
    bindings: &std::collections::HashMap<crate::resolve::DefId, ProcessValueId>,
    process: &mut ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> u32 {
    let id = process.inline_calls.len() as u32;
    let name = context
        .resolved
        .declared(function.name.span)
        .and_then(|definition| context.resolved.qualified_name(definition))
        .unwrap_or_else(|| function.name.text.clone());
    process.inline_calls.push(crate::ir::ProcessInlineCall {
        parent: context.inline_scope,
        name,
        function: function.span,
        call,
        parameters: function
            .params
            .iter()
            .filter_map(|parameter| {
                let (name, value) = if parameter.is_self {
                    ("self".into(), receiver?)
                } else {
                    let name = parameter.name.as_ref()?;
                    (
                        name.text.clone(),
                        *bindings.get(&context.resolved.declared(name.span)?)?,
                    )
                };
                Some(crate::ir::ProcessInlineParameter {
                    name,
                    value,
                    span: parameter.span,
                })
            })
            .collect(),
        result: None,
        blocks: Vec::new(),
        locals: process.locals.len() as u32..process.locals.len() as u32,
    });
    context.inline_scope = Some(id);
    id
}

fn finish_inline_call(
    scope: u32,
    entry: ProcessBlockId,
    first_local: usize,
    process: &mut ProcessCfg,
) {
    let call = &mut process.inline_calls[scope as usize];
    call.blocks.push(entry.0..process.blocks.len() as u32);
    call.locals = first_local as u32..process.locals.len() as u32;
}

/// Keep a call's evaluation boundary in ordinary Process locals. Dynamic
/// selectors must survive CFG/cache boundaries without retargeting a place;
/// computed arguments must not repeat foreign calls or mutable reads.
fn capture_process_argument(
    value: ProcessValueId,
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> ProcessValueId {
    let node = context.process_ir.values[value.0 as usize].clone();
    let kind = match node.kind {
        ProcessValueKind::Local { .. }
        | ProcessValueKind::Storage(_)
        | ProcessValueKind::Signal {
            state: ProcessSignalState::Current,
            ..
        }
        | ProcessValueKind::Number(_)
        | ProcessValueKind::BitString { .. }
        | ProcessValueKind::Char(_)
        | ProcessValueKind::String(_)
        | ProcessValueKind::Default => return value,
        ProcessValueKind::Field { base, field }
            if process_place_assignment(base, context.process_ir).is_some() =>
        {
            ProcessValueKind::Field {
                base: capture_process_argument(base, context, process, block),
                field,
            }
        }
        ProcessValueKind::Index { base, index }
            if process_place_assignment(base, context.process_ir).is_some() =>
        {
            ProcessValueKind::Index {
                base: capture_process_argument(base, context, process, block),
                index: capture_process_operand(index, context, process, block),
            }
        }
        ProcessValueKind::PackedSlice { base, left, right }
            if process_place_assignment(base, context.process_ir).is_some() =>
        {
            ProcessValueKind::PackedSlice {
                base: capture_process_argument(base, context, process, block),
                left,
                right,
            }
        }
        ProcessValueKind::Concat(parts)
            if process_place_assignment(value, context.process_ir).is_some() =>
        {
            ProcessValueKind::Concat(
                parts
                    .into_iter()
                    .map(|part| capture_process_argument(part, context, process, block))
                    .collect(),
            )
        }
        _ => return capture_process_operand(value, context, process, block),
    };
    let layout = context.process_ir.value_layouts[value.0 as usize].clone();
    let id = push_value(node.span, node.ty, node.bit_width, kind, context);
    if layout.is_some() {
        context.process_ir.value_layouts[id.0 as usize] = layout;
    }
    id
}

fn capture_process_operand(
    value: ProcessValueId,
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> ProcessValueId {
    let node = context.process_ir.values[value.0 as usize].clone();
    if matches!(
        node.kind,
        ProcessValueKind::Number(_)
            | ProcessValueKind::BitString { .. }
            | ProcessValueKind::Char(_)
            | ProcessValueKind::String(_)
            | ProcessValueKind::Default
    ) {
        return value;
    }
    let local = ProcessLocalId(process.locals.len() as u32);
    let layout = process_value_source_layout(value, context.process_ir)
        .cloned()
        .or_else(|| {
            node.ty
                .as_ref()
                .and_then(|ty| process_layout_for_type(ty, node.span, context))
        });
    // A kernel integer wider than its word (`x << 6` keeps the operand's
    // bits) wraps to the word it is captured as, as any `integer` does.
    let word = layout.as_ref().and_then(SourceLayout::packed_width);
    let (value, width) = match (word, node.bit_width) {
        (Some(word), Some(width))
            if width > word && matches!(node.ty, Some(crate::types::Ty::Integer)) =>
        {
            let narrowed = push_value(
                node.span,
                node.ty.clone(),
                Some(word),
                ProcessValueKind::RawResize { operand: value },
                context,
            );
            context.process_ir.value_layouts[narrowed.0 as usize] = layout.clone();
            (narrowed, Some(word))
        }
        _ => (value, node.bit_width),
    };
    process.locals.push(ProcessLocal {
        id: local,
        name: format!("<call:{}>", local.0),
        source: None,
        span: node.span,
        scope: None,
        ty: node.ty.clone(),
        layout: layout.clone(),
    });
    process.blocks[block.0 as usize]
        .instructions
        .push(ProcessInstruction::Declare {
            local,
            initializer: Some(value),
            span: node.span,
        });
    let id = push_value(
        node.span,
        node.ty,
        width,
        ProcessValueKind::Local {
            process: process.id,
            local,
        },
        context,
    );
    context.process_ir.value_layouts[id.0 as usize] = layout;
    id
}

fn lower_process_format(
    operation: &ProcessRuntimeOp,
    arguments: &[ast::Expr],
    lowered: &[ProcessValueId],
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
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
    let mut parts = Vec::new();
    for part in crate::syntax::format::parts(text) {
        match part {
            crate::syntax::format::FormatPart::Text(text) => {
                parts.push(ProcessFormatPart::Text(text));
            }
            crate::syntax::format::FormatPart::Placeholder(spec) => {
                let (expression, value) = values.next()?;
                lower_format_value(
                    Some(expression),
                    *value,
                    &spec,
                    &mut parts,
                    process,
                    context,
                )?;
            }
        }
    }
    Some(parts)
}

/// Append the parts that print `value` with `spec`. A width wraps the whole
/// output in `Open`/`Close`; numbers sit right by default, everything else
/// left, as in Rust. `expression` is the source argument, when there is one;
/// a field or element printed inside a composite has none.
fn lower_format_value(
    expression: Option<&ast::Expr>,
    value: ProcessValueId,
    spec: &crate::syntax::format::FormatSpec,
    parts: &mut Vec<ProcessFormatPart>,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<()> {
    // A DUT's aggregate port read directly (`print!("{}", dut.z)`) is its
    // signals, untyped: take the checked type, as a `let` copy would.
    // Its layout too, which a `Logic[4]` port needs to print as symbols.
    if let Some(expression) = expression {
        let ty = process_value_type(value, context).or_else(|| {
            context
                .typed
                .expr_type(ast::expr_span(expression))
                .filter(|ty| !matches!(ty, crate::types::Ty::Error))
                .cloned()
        });
        let signals = matches!(
            context.process_ir.values[value.0 as usize].kind,
            ProcessValueKind::Signal { .. }
        );
        if let Some(ty) = ty.filter(|_| signals) {
            let span = ast::expr_span(expression);
            // Only an aggregate's: a scalar's (a ranged `integer<0..15>`,
            // 4 bits) would contradict the 64-bit word the read carries.
            if context.process_ir.value_layouts[value.0 as usize].is_none() {
                context.process_ir.value_layouts[value.0 as usize] =
                    process_aggregate_layout_for_type(&ty, span, context);
            }
            context.process_ir.values[value.0 as usize]
                .ty
                .get_or_insert(ty);
        }
    }
    let layout = operand_source_layout(value, process, context);
    let debug = spec.kind == crate::syntax::format::FormatKind::Debug;
    // `{:?}` is the built-in form, whatever `Display` says.
    let display = (!debug).then(|| display_function(value, context)).flatten();
    let composite = display.is_some() || layout.as_ref().is_some_and(is_composite_layout);
    let kind = if composite {
        None
    } else {
        Some(match expression {
            Some(expression) => process_display_kind(expression, value, context)?,
            None => process_value_type(value, context)
                .and_then(|ty| process_display_kind_for_type(&ty, context))?,
        })
    };
    // A number-format family (`float`, `ufixed`) is a number even when its
    // `Display` writes it: it sits right in a width.
    let numeric = matches!(
        kind,
        Some(ProcessDisplayKind::Unsigned | ProcessDisplayKind::Signed | ProcessDisplayKind::Real)
    ) || display.is_some()
        && matches!(
            layout.as_ref().map(|layout| &layout.kind),
            Some(LayoutKind::Packed { .. })
        );
    if let Some(width) = spec.width {
        parts.push(ProcessFormatPart::Open {
            fill: spec.fill,
            align: spec.align.unwrap_or(if numeric {
                crate::syntax::format::FormatAlign::Right
            } else {
                crate::syntax::format::FormatAlign::Left
            }),
            width,
            zero: spec.zero && numeric,
        });
    }
    match (kind, layout) {
        _ if display.is_some() => {
            lower_format_display(value, display?, &spec.numeric(), parts, process, context)?
        }
        (Some(kind), _) => {
            // Rust's `Debug` quotes text: `"hi"`, `'a'`.
            let quote = match kind {
                ProcessDisplayKind::String if debug => Some("\""),
                ProcessDisplayKind::Character if debug => Some("'"),
                _ => None,
            };
            parts.extend(quote.map(|quote| ProcessFormatPart::Text(quote.to_owned())));
            parts.push(ProcessFormatPart::Value {
                value,
                kind,
                spec: spec.numeric(),
            });
            parts.extend(quote.map(|quote| ProcessFormatPart::Text(quote.to_owned())));
        }
        (None, Some(layout)) => {
            lower_format_composite(value, &layout, &spec.numeric(), parts, process, context)?
        }
        (None, None) => return None,
    }
    if spec.width.is_some() {
        parts.push(ProcessFormatPart::Close);
    }
    Some(())
}

/// The `Display::fmt` a value's type implements: a method `fmt(self, f:
/// Formatter)` with a body.
fn display_function<'a>(
    value: ProcessValueId,
    context: &LoweringContext<'a>,
) -> Option<&'a ast::FnDecl> {
    let ty = process_value_type(value, context)?;
    let owner = process_type_key(&ty, context)?;
    let function = context.functions.get_associated(&owner, "fmt")?;
    let formatter = function
        .params
        .iter()
        .find(|parameter| !parameter.is_self)
        .and_then(|parameter| parameter.ty.as_ref());
    let takes_formatter = matches!(formatter, Some(ast::Type::Path(path))
        if path.segments.last().is_some_and(|segment| segment.text == "Formatter"));
    (takes_formatter && function.body.is_some() && function.ret.is_none()).then_some(function)
}

/// Expand a `Display::fmt` body into format parts: each `write!` appends its
/// text and values, with `self` bound to `value`; `let`s bind as in any
/// inlined body. A plain `{}` inside takes the caller's numeric `spec`.
fn lower_format_display(
    value: ProcessValueId,
    function: &ast::FnDecl,
    spec: &crate::syntax::format::FormatSpec,
    parts: &mut Vec<ProcessFormatPart>,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<()> {
    // A `fmt` that prints its own type would expand forever.
    if !context.inline_functions.insert(function.span) {
        return None;
    }
    context.inline_self_values.push(Some(value));
    context
        .value_bindings
        .push(std::collections::HashMap::new());
    let mut expanded = Some(());
    for statement in &function.body.as_ref()?.stmts {
        let done = match statement {
            Stmt::Let(declaration) => (|| {
                let declared = declaration
                    .ty
                    .as_ref()
                    .and_then(|ty| process_declared_type(ty, context));
                let bound = value_ref_with_type(
                    declaration.value.as_ref()?,
                    process,
                    context,
                    declared.as_ref(),
                );
                let definition = context.resolved.declared(declaration.name.span)?;
                context.value_bindings.last_mut()?.insert(definition, bound);
                Some(())
            })(),
            Stmt::Expr(ast::Expr::Call {
                callee,
                args,
                bang: true,
                ..
            }) if matches!(callee.as_ref(), ast::Expr::Path(path) if path_name(path) == "write") => {
                lower_format_write(args, spec, parts, process, context)
            }
            _ => None,
        };
        if done.is_none() {
            expanded = None;
            break;
        }
    }
    context.value_bindings.pop();
    context.inline_self_values.pop();
    context.inline_functions.remove(&function.span);
    expanded
}

/// One `write!(f, "text {}", args)` inside a `Display::fmt`.
fn lower_format_write(
    args: &[ast::Expr],
    inherited: &crate::syntax::format::FormatSpec,
    parts: &mut Vec<ProcessFormatPart>,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<()> {
    let ast::Expr::StrLit { text, .. } = args.get(1)? else {
        return None;
    };
    let mut values = args.iter().skip(2);
    for part in crate::syntax::format::parts(text) {
        match part {
            crate::syntax::format::FormatPart::Text(text) => {
                parts.push(ProcessFormatPart::Text(text));
            }
            crate::syntax::format::FormatPart::Placeholder(own) => {
                let expression = values.next()?;
                let value = value_ref(expression, process, context);
                let spec = if own.is_plain() {
                    inherited.clone()
                } else {
                    own
                };
                lower_format_value(Some(expression), value, &spec, parts, process, context)?;
            }
        }
    }
    Some(())
}

/// Whether a value of this layout prints as its parts rather than as one
/// number: a struct or view, an array, or a vector of logic symbols. Numeric
/// vector families (`unsigned`, `signed`) stay numbers.
fn is_composite_layout(layout: &crate::ir::SourceLayout) -> bool {
    match &layout.kind {
        // A `Char` array is a string, printed as text.
        LayoutKind::Array { element, .. } => !matches!(
            element.kind,
            LayoutKind::Scalar {
                domain: crate::ir::ScalarDomain::Character,
                ..
            }
        ),
        LayoutKind::Struct { .. } => true,
        LayoutKind::Packed {
            family,
            element_enum,
            ..
        } => {
            element_enum.is_some()
                && !matches!(family.rsplit("::").next(), Some("unsigned" | "signed"))
        }
        _ => false,
    }
}

/// Print a composite value, Rust `Debug` style: `Name { field: value, .. }`
/// for a struct or view, `(a, b)` for a tuple, `[a, b]` for an array, and a
/// logic vector's symbols in declared order (`01XZ`). Numeric leaves inside
/// take `spec`.
fn lower_format_composite(
    value: ProcessValueId,
    layout: &crate::ir::SourceLayout,
    spec: &crate::syntax::format::FormatSpec,
    parts: &mut Vec<ProcessFormatPart>,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<()> {
    let span = context.process_ir.values[value.0 as usize].span;
    match &layout.kind {
        LayoutKind::Struct {
            name,
            view: None,
            fields,
        } if crate::types::tuple_struct_arity(name).is_some() => {
            parts.push(ProcessFormatPart::Text("(".to_owned()));
            for (index, field) in fields.iter().enumerate() {
                if index > 0 {
                    parts.push(ProcessFormatPart::Text(", ".to_owned()));
                }
                let kind = ProcessValueKind::Field {
                    base: value,
                    field: field.name.clone(),
                };
                let member = push_projection(span, kind, &field.layout, process, context);
                lower_format_value(None, member, spec, parts, process, context)?;
            }
            parts.push(ProcessFormatPart::Text(")".to_owned()));
        }
        LayoutKind::Struct { name, view, fields } => {
            let shown = view.as_deref().unwrap_or(name);
            let shown = shown.rsplit("::").next().unwrap_or(shown);
            if fields.is_empty() {
                parts.push(ProcessFormatPart::Text(shown.to_owned()));
                return Some(());
            }
            parts.push(ProcessFormatPart::Text(format!("{shown} {{ ")));
            for (index, field) in fields.iter().enumerate() {
                let separator = if index == 0 { "" } else { ", " };
                parts.push(ProcessFormatPart::Text(format!(
                    "{separator}{}: ",
                    field.name
                )));
                let kind = ProcessValueKind::Field {
                    base: value,
                    field: field.name.clone(),
                };
                let member = push_projection(span, kind, &field.layout, process, context);
                lower_format_value(None, member, spec, parts, process, context)?;
            }
            parts.push(ProcessFormatPart::Text(" }".to_owned()));
        }
        // A vector of character-literal enums (`Bit`, `Logic`) reads as its
        // symbols, `01XZ`, as VHDL's `to_string` writes it.
        LayoutKind::Array { range, element } if symbol_enum(element, context).is_some() => {
            let name = symbol_enum(element, context)?;
            for label in layout_labels((*range)?)? {
                let member = push_element(span, value, label, element, process, context);
                parts.push(ProcessFormatPart::Value {
                    value: member,
                    kind: ProcessDisplayKind::Symbol(name.clone()),
                    spec: crate::syntax::format::FormatSpec::default(),
                });
            }
        }
        LayoutKind::Array { range, element } => {
            parts.push(ProcessFormatPart::Text("[".to_owned()));
            for (position, label) in layout_labels((*range)?)?.into_iter().enumerate() {
                if position > 0 {
                    parts.push(ProcessFormatPart::Text(", ".to_owned()));
                }
                let member = push_element(span, value, label, element, process, context);
                lower_format_value(None, member, spec, parts, process, context)?;
            }
            parts.push(ProcessFormatPart::Text("]".to_owned()));
        }
        LayoutKind::Packed {
            range,
            element_enum: Some(element),
            ..
        } => {
            // An element read is the element type's whole value (a `Logic`
            // discriminant, four bits), as in `element_match`: one bit made
            // `{:?}` of any newtype over `Logic[N]` fail to generate.
            let element_layout = nominal_type_from_name(element, context.resolved)
                .and_then(|ty| process_layout_for_type(&ty, layout.span, context))
                .unwrap_or_else(|| crate::ir::SourceLayout {
                    span: layout.span,
                    kind: LayoutKind::Scalar {
                        width: 1,
                        domain: crate::ir::ScalarDomain::Enum(element.clone()),
                        nominal: Some(element.clone()),
                        value_range: None,
                    },
                });
            for label in layout_labels((*range)?)? {
                let member = push_element(span, value, label, &element_layout, process, context);
                parts.push(ProcessFormatPart::Value {
                    value: member,
                    kind: ProcessDisplayKind::Symbol(element.clone()),
                    spec: crate::syntax::format::FormatSpec::default(),
                });
            }
        }
        _ => return None,
    }
    Some(())
}

/// The symbol-table key of an enum element whose every variant is a
/// character literal (`'0'`, `'X'`), so a vector of it prints as a word.
fn symbol_enum(element: &crate::ir::SourceLayout, context: &LoweringContext<'_>) -> Option<String> {
    let LayoutKind::Scalar {
        domain: crate::ir::ScalarDomain::Enum(name),
        ..
    } = &element.kind
    else {
        return None;
    };
    let symbols = context.design.enum_syms.get(name)?;
    symbols
        .values()
        .all(|symbol| symbol.len() == 3 && symbol.starts_with('\'') && symbol.ends_with('\''))
        .then(|| name.clone())
}

/// A range's labels in declared order: `7..0` is 7, 6, ... 0.
fn layout_labels(range: crate::ir::LayoutRange) -> Option<Vec<i64>> {
    let length = i64::try_from(range.len()?).ok()?;
    let step = if range.ascending() { 1 } else { -1 };
    Some(
        (0..length)
            .map(|offset| range.left + step * offset)
            .collect(),
    )
}

/// A projection of `layout` from an existing value, typed and laid out as the
/// lowering of `value.field` would be.
fn push_projection(
    span: crate::diag::Span,
    kind: ProcessValueKind,
    layout: &crate::ir::SourceLayout,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> ProcessValueId {
    let ty = process_type_from_layout(layout, context.resolved);
    let width =
        source_value_width(&kind, ty.as_ref(), process, context).or_else(|| layout.packed_width());
    let id = push_value(span, ty, width, kind, context);
    context.process_ir.value_layouts[id.0 as usize] = Some(layout.clone());
    id
}

/// Element `label` of an array or vector value.
fn push_element(
    span: crate::diag::Span,
    base: ProcessValueId,
    label: i64,
    element: &crate::ir::SourceLayout,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> ProcessValueId {
    let index = push_value(
        span,
        Some(crate::types::Ty::Integer),
        Some(64),
        ProcessValueKind::Number(ProcessNumber::Integer(vec![label as u64])),
        context,
    );
    push_projection(
        span,
        ProcessValueKind::Index { base, index },
        element,
        process,
        context,
    )
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
            let else_layout = process_value_source_layout(*else_value, process_ir)?;
            process_layout_same_shape(then_layout, else_layout).then_some(then_layout)
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
    // `s.arr[5]` and `matrix[0][5]`: a constant index past the declared
    // labels, which the checker cannot see through a field or element. It
    // lowered to a block nothing could run (#198, #213).
    if let Some(value) = arena_constant_integer(index, &context.process_ir.values) {
        let (low, high) = (range.left.min(range.right), range.left.max(range.right));
        if value < i128::from(low) || value > i128::from(high) {
            context.rejected.borrow_mut().push(
                crate::diag::Diagnostic::error(format!(
                    "index {value} is outside `{}..{}` of this array",
                    range.left, range.right
                ))
                .with_code(crate::diag::codes::TYPE_MISMATCH)
                .at(span),
            );
        }
    }
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
    // A port read keeps its signal's width: `integer<0..15>` is 4 bits,
    // which as signed printed 13 as -3.
    if let ProcessValueKind::Signal { signals, .. } = &process_value.kind {
        let natural = signals.iter().all(|signal| {
            context
                .design
                .signals
                .get(signal.0 as usize)
                .and_then(|signal| signal.range)
                .is_some_and(|(left, right)| left.min(right) >= 0)
        });
        if natural && matches!(ty, Some(crate::types::Ty::Integer)) {
            return Some(ProcessDisplayKind::Unsigned);
        }
    }
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
        crate::types::Ty::Natural => Some(ProcessDisplayKind::Unsigned),
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
        crate::types::Ty::Tuple(_) | crate::types::Ty::Void | crate::types::Ty::Error => None,
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
                ProcessUnaryOp::Neg
                | ProcessUnaryOp::RealToInteger
                | ProcessUnaryOp::NaturalToInteger => Some(ProcessDisplayKind::Signed),
                ProcessUnaryOp::IntegerToReal
                | ProcessUnaryOp::NaturalToReal
                | ProcessUnaryOp::Real(_) => Some(ProcessDisplayKind::Real),
                ProcessUnaryOp::IntegerToNatural | ProcessUnaryOp::RealToNatural => {
                    Some(ProcessDisplayKind::Unsigned)
                }
                ProcessUnaryOp::Not => Some(ProcessDisplayKind::Unsigned),
            }),
        ProcessValueKind::Binary { operation, .. } => match operation {
            ProcessBinaryOp::FloatAdd
            | ProcessBinaryOp::FloatSub
            | ProcessBinaryOp::FloatMul
            | ProcessBinaryOp::FloatDiv
            | ProcessBinaryOp::FloatRem
            | ProcessBinaryOp::FloatPow => Some(ProcessDisplayKind::Real),
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
            | ProcessBinaryOp::SignedRem
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
    let span = ast::expr_span(expression);
    if !context.widening.contains(&span) {
        if let Some(promoted) = context.typed.promotion(span) {
            return Some(promoted.clone());
        }
    }
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

/// A value stored where an `impl Into` converts it: the impl's `into`,
/// inlined with the value as `self` (conversions proposal). The span is held
/// while the receiver lowers, so it lowers unconverted.
fn process_converted_value(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let span = ast::expr_span(expression);
    if context.widening.contains(&span) {
        return None;
    }
    let conversion = context.typed.conversion(span)?.clone();
    let function = context
        .functions
        .get_into(&conversion.target_key, &conversion.source_key)?;
    context.widening.insert(span);
    let receiver = value_ref(expression, process, context);
    context.widening.remove(&span);
    inline_process_function(
        function,
        span,
        Some(receiver),
        &[],
        process,
        context,
        Some(&conversion.target),
    )
}

/// `expression` widened to its promoted type (spec 3.17) through std's
/// `From<Source>` impl for that type, inlined with `Self` as the promoted
/// format. `None` when it does not widen.
fn process_promoted_operand(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let span = ast::expr_span(expression);
    if context.widening.contains(&span) {
        return None;
    }
    let target = context.typed.promotion(span)?.clone();
    context.widening.insert(span);
    let result = widen_operand(expression, &target, span, process, context);
    context.widening.remove(&span);
    result
}

fn widen_operand(
    expression: &ast::Expr,
    target: &crate::types::Ty,
    span: crate::diag::Span,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let crate::types::Ty::Array {
        len,
        family: Some(family),
        low,
        ..
    } = target
    else {
        return None;
    };
    let source_type = operand_type(expression, process, context)?;
    let source = process_type_key(&source_type, context)?;
    let function = context
        .functions
        .get_conversion(&context.functions.canonical_type_key(family), &source)?;
    let first_value = context.process_ir.values.len();
    let operand = value_ref_with_type(expression, process, context, Some(&source_type));
    let number = |value: i64| {
        let literal = ast::Expr::Int {
            text: value.unsigned_abs().to_string(),
            span,
        };
        if value < 0 {
            ast::Expr::Unary {
                op: ast::UnOp::Neg,
                rhs: Box::new(literal),
                span,
            }
        } else {
            literal
        }
    };
    let index = ast::Expr::Range {
        lo: Box::new(number(low + i64::from(*len) - 1)),
        hi: Box::new(number(*low)),
        span,
    };
    inline_with_self_format(
        function,
        family.clone(),
        &index,
        &[operand],
        span,
        first_value,
        process,
        context,
        Some(target),
    )
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

/// The value of an assignment: source to lower against the target's type, or
/// a value already lowered (one position of a tuple).
enum AssignedValue<'e> {
    Source(&'e ast::Expr),
    Lowered(ProcessValueId),
}

/// Assign `value` to the place `target` names: a staged signal write, an
/// immediate local store, or a scheduled one with `after`; a write that
/// drives the design at time zero settles before the process continues.
fn assign_process_value(
    target: &ast::Expr,
    value: AssignedValue<'_>,
    after: Option<&ast::Expr>,
    span: crate::diag::Span,
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let source_semantics = assignment_semantics(target, process, context);
    let target_type = context.typed.expr_type(ast::expr_span(target)).cloned();
    let source_target = target;
    let target = value_ref(source_target, process, context);
    let target_type = target_type
        .filter(|ty| !matches!(ty, crate::types::Ty::Error))
        .or_else(|| process_value_type(target, context));
    let semantics =
        process_place_assignment(target, context.process_ir).unwrap_or(source_semantics);
    let settle = after.is_none()
        && matches!(process.activation, ProcessActivation::TimeZero)
        && (assignment_drives_design_value(target, context.process_ir)
            || assignment_drives_design_source(source_target, process, context));
    let value = match value {
        AssignedValue::Source(value) => {
            let ty = literal_type(value, target_type.as_ref());
            let lowered = value_ref_with_type(
                value,
                process,
                context,
                ty.as_ref().or(target_type.as_ref()),
            );
            // A literal for a runtime array is built in a local first.
            if ty.is_some() {
                cfg_calls::frame_copy(lowered, process, context, block)
            } else {
                lowered
            }
        }
        AssignedValue::Lowered(value) => value,
    };
    // `xs[i] = v` on a runtime array writes the element in place; a write
    // into part of one (`xs[i][j] = v`) updates a copy of the element first.
    if let Some((element, rest)) = runtime_element_place(target, context) {
        let ProcessValueKind::HostCall { arguments, .. } =
            &context.process_ir.values[element.0 as usize].kind
        else {
            unreachable!()
        };
        let (handle, index) = (arguments[0], arguments[1]);
        let element_type =
            runtime_array_element(process_value_type(handle, context).as_ref()).cloned();
        if after.is_none() && element_type.is_some() {
            let value = match element_type {
                Some(element_type) if rest.is_none() => {
                    let value = runtime_array_copy(value, Some(&element_type), context);
                    symbol_encode(value, &element_type, span, process, context).unwrap_or(value)
                }
                _ => value,
            };
            let value = match rest {
                None => value,
                Some(rest) => {
                    let copy = cfg_calls::frame_copy(element, process, context, block);
                    let place = rebase_place(rest, element, copy, context);
                    process.blocks[block.0 as usize].instructions.push(
                        ProcessInstruction::Assign {
                            semantics: ProcessAssignment::ImmediateLocal,
                            driver_context: None,
                            target: place,
                            value,
                            span,
                        },
                    );
                    copy
                }
            };
            let store = push_value(
                span,
                Some(crate::types::Ty::Integer),
                Some(64),
                ProcessValueKind::HostCall {
                    operation: ProcessHostValueOp::ArrayStore,
                    arguments: vec![handle, index, value],
                },
                context,
            );
            cfg_calls::frame_copy(store, process, context, block);
            return Some(block);
        }
    }
    let value = runtime_array_copy(value, target_type.as_ref(), context);
    let driver_context = matches!(
        semantics,
        ProcessAssignment::StagedSignal | ProcessAssignment::PerPlace
    )
    .then_some(process.id.0);
    let instruction = match after {
        Some(delay) => ProcessInstruction::Schedule {
            driver_context,
            target,
            value,
            delay: value_ref(delay, process, context),
            span,
        },
        None => ProcessInstruction::Assign {
            semantics,
            driver_context,
            target,
            value,
            span,
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
            span,
        };
        Some(resume)
    } else {
        Some(block)
    }
}

/// `(q, r) = e;` in a process: `e` is evaluated once into a hidden local of
/// the tuple's type, then each named position is assigned its part, so
/// `(a, b) = (b, a);` swaps.
fn lower_tuple_assign(
    target: &ast::Expr,
    value: &ast::Expr,
    after: Option<&ast::Expr>,
    span: crate::diag::Span,
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    // `(y1, _) = ('1', '0')`: position by position, `_` skipped, when no
    // target is read on the right (a swap must read first). A wildcard of a
    // tuple literal had no place to lower to (#263).
    if after.is_none() {
        if let Some(parts) = super::control::tuple_literal_parts(target, value) {
            let written: Vec<String> = parts
                .iter()
                .filter_map(|part| match part {
                    Stmt::Assign { target, .. } => Some(crate::syntax::pretty::expr_string(target)),
                    _ => None,
                })
                .collect();
            let read = crate::syntax::pretty::expr_string(value);
            let overlaps = written.iter().any(|name| {
                read.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
                    .any(|word| word == name)
            });
            if !overlaps {
                let mut block = block;
                for part in &parts {
                    block = lower_statement(part, context, process, block)?;
                }
                return Some(block);
            }
        }
    }
    // Each position's type: the target's where it names one, the value's
    // where the target is `_`.
    fn merge(
        target: Option<&crate::types::Ty>,
        value: Option<&crate::types::Ty>,
    ) -> Option<crate::types::Ty> {
        match (target, value) {
            (Some(crate::types::Ty::Tuple(targets)), value) => {
                let values = match value {
                    Some(crate::types::Ty::Tuple(values)) => values.iter().map(Some).collect(),
                    _ => vec![None; targets.len()],
                };
                targets
                    .iter()
                    .zip(values)
                    .map(|(target, value)| merge(Some(target), value))
                    .collect::<Option<Vec<_>>>()
                    .map(crate::types::Ty::Tuple)
            }
            (Some(crate::types::Ty::Error) | None, value) => value.cloned(),
            (Some(target), _) => Some(target.clone()),
        }
    }
    // A position the checker could not type (a generic call's `T` under a
    // `_`) is read from the lowered value's own layout instead.
    fn complete(ty: &crate::types::Ty) -> bool {
        match ty {
            crate::types::Ty::Tuple(elems) => elems.iter().all(complete),
            crate::types::Ty::Error => false,
            _ => true,
        }
    }
    let checked = merge(
        context.typed.expr_type(ast::expr_span(target)),
        context.typed.expr_type(ast::expr_span(value)),
    )
    .filter(complete);
    let initializer = value_ref_with_type(value, process, context, checked.as_ref());
    let layout = checked
        .as_ref()
        .and_then(|ty| process_layout_for_type(ty, span, context))
        .or_else(|| operand_source_layout(initializer, process, context));
    let ty = checked.or_else(|| {
        layout
            .as_ref()
            .and_then(|layout| process_type_from_layout(layout, context.resolved))
    });
    let (Some(ty), Some(layout)) = (ty, layout) else {
        // Fail closed: validation rejects the invalid value, rather than the
        // rest of the process silently disappearing.
        let invalid = unsupported_process_value(span, None, context);
        process.blocks[block.0 as usize]
            .instructions
            .push(ProcessInstruction::Runtime {
                operation: ProcessRuntimeOp::Call("<tuple assignment>".to_string()),
                arguments: vec![invalid],
                format: None,
                span,
            });
        return Some(block);
    };
    let local = ProcessLocalId(process.locals.len() as u32);
    process.locals.push(ProcessLocal {
        id: local,
        name: crate::syntax::pretty::expr_string(target),
        source: None,
        span,
        scope: None,
        ty: Some(ty.clone()),
        layout: Some(layout),
    });
    process.blocks[block.0 as usize]
        .instructions
        .push(ProcessInstruction::Declare {
            local,
            initializer: Some(initializer),
            span,
        });
    let width = process.locals[local.0 as usize]
        .layout
        .as_ref()
        .and_then(SourceLayout::packed_width);
    let whole = push_value(
        span,
        Some(ty.clone()),
        width,
        ProcessValueKind::Local {
            process: process.id,
            local,
        },
        context,
    );
    assign_tuple_parts(target, whole, &ty, after, span, context, process, block)
}

/// Assign each named position of a tuple target its part of `whole`.
#[allow(clippy::too_many_arguments)]
fn assign_tuple_parts(
    target: &ast::Expr,
    whole: ProcessValueId,
    ty: &crate::types::Ty,
    after: Option<&ast::Expr>,
    span: crate::diag::Span,
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    mut block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let (ast::Expr::Tuple { elems, .. }, crate::types::Ty::Tuple(types)) = (target, ty) else {
        return None;
    };
    for (index, (position, ty)) in elems.iter().zip(types).enumerate() {
        if matches!(position, ast::Expr::Path(path)
            if matches!(path.segments.as_slice(), [only] if only.text == "_"))
        {
            continue;
        }
        let kind = ProcessValueKind::Field {
            base: whole,
            field: index.to_string(),
        };
        let width = process_layout_for_type(ty, span, context)
            .and_then(|layout| layout.packed_width())
            .or_else(|| source_value_width(&kind, Some(ty), process, context));
        let part = push_value(span, Some(ty.clone()), width, kind, context);
        block = match position {
            ast::Expr::Tuple { .. } => {
                assign_tuple_parts(position, part, ty, after, span, context, process, block)?
            }
            place => assign_process_value(
                place,
                AssignedValue::Lowered(part),
                after,
                span,
                context,
                process,
                block,
            )?,
        };
    }
    Some(block)
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
    // `match x {}` takes no arm, so control continues; a `Match` with no arms
    // has no native form and left the process unlowered (#145).
    // ponytail: the scrutinee is not evaluated, so a call in it is skipped.
    if statement.arms.is_empty() {
        return Some(block);
    }
    let scrutinee_type = context
        .typed
        .expr_type(ast::expr_span(&statement.scrutinee))
        .cloned();
    if match_needs_guards(&statement.arms, scrutinee_type.as_ref(), context) {
        return lower_guarded_match(statement, scrutinee_type.as_ref(), context, process, block);
    }
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
        // Expression bounds and tuples lower as guards
        // (`lower_guarded_match`); reaching here is a fail-closed unsupported
        // pattern.
        ast::Pattern::Bounds { .. } | ast::Pattern::Tuple { .. } => ProcessPattern::Path {
            definition: None,
            segments: Vec::new(),
        },
    }
}

/// Whether a match needs its arms as conditions rather than decoded
/// patterns: an expression range (through `Ord`), or a bit pattern on a type
/// that says how it matches (`Match`).
fn match_needs_guards(
    arms: &[ast::MatchArm],
    scrutinee_type: Option<&crate::types::Ty>,
    context: &LoweringContext<'_>,
) -> bool {
    fn guarded(pattern: &ast::Pattern, matches: bool) -> bool {
        match pattern {
            ast::Pattern::Bounds { .. } => true,
            ast::Pattern::BitPattern { .. } => matches,
            ast::Pattern::Or { alts, .. } => alts.iter().any(|pattern| guarded(pattern, matches)),
            _ => false,
        }
    }
    let matches = scrutinee_type
        .and_then(|ty| match ty {
            crate::types::Ty::Array { elem, .. } => process_type_key(elem, context),
            _ => None,
        })
        .and_then(|owner| context.functions.get_associated(&owner, "matches"))
        .is_some();
    matches!(scrutinee_type, Some(crate::types::Ty::Tuple(_)))
        || arms.iter().any(|arm| guarded(&arm.pattern, matches))
}

/// A `match` scrutinee lowered once: a value, or a tuple literal's
/// positions, which are matched one by one and never built.
enum MatchScrutinee {
    Value(ProcessValueId, Option<crate::types::Ty>),
    Tuple(Vec<MatchScrutinee>),
}

fn lower_scrutinee(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> MatchScrutinee {
    if let ast::Expr::Tuple { elems, .. } = expression {
        return MatchScrutinee::Tuple(
            elems
                .iter()
                .map(|elem| lower_scrutinee(elem, process, context))
                .collect(),
        );
    }
    let ty = context.typed.expr_type(ast::expr_span(expression)).cloned();
    MatchScrutinee::Value(value_ref(expression, process, context), ty)
}

/// Evaluate every value of a scrutinee once, in `block`, before the arms
/// read it.
fn capture_scrutinee(
    scrutinee: MatchScrutinee,
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> MatchScrutinee {
    match scrutinee {
        MatchScrutinee::Value(value, ty) => {
            MatchScrutinee::Value(capture_process_operand(value, context, process, block), ty)
        }
        MatchScrutinee::Tuple(parts) => MatchScrutinee::Tuple(
            parts
                .into_iter()
                .map(|part| capture_scrutinee(part, context, process, block))
                .collect(),
        ),
    }
}

/// The condition `pattern` puts on a scrutinee: a tuple pattern is all of
/// its positions' conditions, a position of a tuple value read as `.i`.
fn scrutinee_condition(
    pattern: &ast::Pattern,
    scrutinee: &MatchScrutinee,
    span: crate::diag::Span,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    match (pattern, scrutinee) {
        (ast::Pattern::Wildcard, _) => Some(bool_value(true, span, context)),
        (ast::Pattern::Or { alts, .. }, _) => {
            let mut any = None;
            for alternative in alts {
                let condition =
                    scrutinee_condition(alternative, scrutinee, span, process, context)?;
                any = Some(match any {
                    Some(any) => bool_binary(ProcessBinaryOp::Or, any, condition, span, context),
                    None => condition,
                });
            }
            any
        }
        (ast::Pattern::Tuple { elems, .. }, MatchScrutinee::Tuple(parts))
            if elems.len() == parts.len() =>
        {
            let mut all = None;
            for (position, part) in elems.iter().zip(parts) {
                let condition = scrutinee_condition(position, part, span, process, context)?;
                all = Some(match all {
                    Some(all) => bool_binary(ProcessBinaryOp::And, all, condition, span, context),
                    None => condition,
                });
            }
            all
        }
        (
            ast::Pattern::Tuple { .. },
            MatchScrutinee::Value(value, Some(crate::types::Ty::Tuple(types))),
        ) => {
            let parts = types
                .iter()
                .enumerate()
                .map(|(index, ty)| {
                    let kind = ProcessValueKind::Field {
                        base: *value,
                        field: index.to_string(),
                    };
                    let width = process_layout_for_type(ty, span, context)
                        .and_then(|layout| layout.packed_width())
                        .or_else(|| source_value_width(&kind, Some(ty), process, context));
                    let field = push_value(span, Some(ty.clone()), width, kind, context);
                    MatchScrutinee::Value(field, Some(ty.clone()))
                })
                .collect();
            scrutinee_condition(
                pattern,
                &MatchScrutinee::Tuple(parts),
                span,
                process,
                context,
            )
        }
        (pattern, MatchScrutinee::Value(value, ty)) => {
            pattern_condition(pattern, *value, ty.as_ref(), span, process, context)
        }
        _ => None,
    }
}

/// A packed vector's element layout, its element type's `Match::matches`
/// (`std_match` for `Logic`), and its lowest label: what a bit pattern
/// matches position by position.
fn element_match<'a>(
    scrutinee: ProcessValueId,
    process: &ProcessCfg,
    context: &LoweringContext<'a>,
) -> Option<(crate::ir::SourceLayout, &'a ast::FnDecl, i64)> {
    let layout = operand_source_layout(scrutinee, process, context)?;
    let LayoutKind::Packed {
        range: Some(range),
        element_enum: Some(element),
        ..
    } = &layout.kind
    else {
        return None;
    };
    let element_type = nominal_type_from_name(element, context.resolved)?;
    let owner = process_type_key(&element_type, context)?;
    let function = context.functions.get_associated(&owner, "matches")?;
    // An element read is the element type's whole value (a `Logic`
    // discriminant), not the vector's one value bit.
    let element_layout = process_layout_for_type(&element_type, layout.span, context)?;
    Some((element_layout, function, range.left.min(range.right)))
}

fn bool_value(
    value: bool,
    span: crate::diag::Span,
    context: &mut LoweringContext<'_>,
) -> ProcessValueId {
    push_value(
        span,
        bool_type(context.resolved),
        Some(1),
        ProcessValueKind::Number(ProcessNumber::Integer(vec![u64::from(value)])),
        context,
    )
}

fn bool_binary(
    operation: ProcessBinaryOp,
    left: ProcessValueId,
    right: ProcessValueId,
    span: crate::diag::Span,
    context: &mut LoweringContext<'_>,
) -> ProcessValueId {
    push_value(
        span,
        bool_type(context.resolved),
        Some(1),
        ProcessValueKind::Binary {
            operation,
            left,
            right,
        },
        context,
    )
}

/// `left op right` over lowered values: the left type's `Eq`/`Ord` impl,
/// else the built-in comparison.
fn compare_values(
    operator: ast::BinOp,
    left: ProcessValueId,
    right: ProcessValueId,
    span: crate::diag::Span,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let left_type = process_value_type(left, context);
    let right_type = process_value_type(right, context);
    let symbol = crate::syntax::pretty::bin_op(&operator);
    if let Some(owner) = left_type
        .as_ref()
        .and_then(|ty| process_type_key(ty, context))
    {
        let input = right_type
            .as_ref()
            .and_then(|ty| process_type_key(ty, context));
        if let Some(function) = context
            .functions
            .get_binary_operator(symbol, &owner, input.as_deref())
            .filter(|function| !context.inline_functions.contains(&function.span))
        {
            let result = bool_type(context.resolved);
            if let Some(value) = inline_process_function(
                function,
                span,
                Some(left),
                &[right],
                process,
                context,
                result.as_ref(),
            ) {
                return Some(value);
            }
        }
    }
    let operation = lower_binary_operator(&operator, left_type.as_ref(), right_type.as_ref());
    Some(bool_binary(operation, left, right, span, context))
}

/// Whether two lowered constants are written high-to-low, so a range of them
/// is the same set swapped. `None` when either is not a known number.
fn bounds_descend(
    low: ProcessValueId,
    high: ProcessValueId,
    context: &LoweringContext<'_>,
) -> Option<bool> {
    let number =
        |value: ProcessValueId| match &context.process_ir.values.get(value.0 as usize)?.kind {
            ProcessValueKind::Number(ProcessNumber::Real(bits)) => Some(f64::from_bits(*bits)),
            _ => constant_integer(value, context).map(|value| value as f64),
        };
    Some(number(low)? > number(high)?)
}

/// The condition one pattern puts on a lowered scrutinee.
fn pattern_condition(
    pattern: &ast::Pattern,
    scrutinee: ProcessValueId,
    scrutinee_type: Option<&crate::types::Ty>,
    span: crate::diag::Span,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    match pattern {
        ast::Pattern::Wildcard => Some(bool_value(true, span, context)),
        // A tuple pattern reaches here only against a non-tuple value; the
        // checker rejected it (`scrutinee_condition` takes the rest).
        ast::Pattern::Tuple { .. } => None,
        ast::Pattern::Or { alts, .. } => {
            let mut any = None;
            for alternative in alts {
                let condition = pattern_condition(
                    alternative,
                    scrutinee,
                    scrutinee_type,
                    span,
                    process,
                    context,
                )?;
                any = Some(match any {
                    Some(any) => bool_binary(ProcessBinaryOp::Or, any, condition, span, context),
                    None => condition,
                });
            }
            any
        }
        ast::Pattern::Bounds { lo, hi, .. } => {
            let mut low = lo
                .as_deref()
                .map(|bound| value_ref_with_type(bound, process, context, scrutinee_type));
            let mut high = hi
                .as_deref()
                .map(|bound| value_ref_with_type(bound, process, context, scrutinee_type));
            if let (Some(l), Some(h)) = (low, high) {
                if bounds_descend(l, h, context) == Some(true) {
                    std::mem::swap(&mut low, &mut high);
                }
            }
            let above = match low {
                Some(low) => Some(compare_values(
                    ast::BinOp::Le,
                    low,
                    scrutinee,
                    span,
                    process,
                    context,
                )?),
                None => None,
            };
            let below = match high {
                Some(high) => Some(compare_values(
                    ast::BinOp::Le,
                    scrutinee,
                    high,
                    span,
                    process,
                    context,
                )?),
                None => None,
            };
            Some(match (above, below) {
                (Some(above), Some(below)) => {
                    bool_binary(ProcessBinaryOp::And, above, below, span, context)
                }
                (Some(condition), None) | (None, Some(condition)) => condition,
                (None, None) => bool_value(true, span, context),
            })
        }
        ast::Pattern::BitPattern { text, .. } => {
            let (mask, value) = crate::syntax::bit_pattern_mask(text)?;
            let width = context
                .process_ir
                .values
                .get(scrutinee.0 as usize)?
                .bit_width?;
            if let Some((element, function, low)) = element_match(scrutinee, process, context) {
                // Each position the pattern fixes, by the element type's
                // `matches` (`std_match` for `Logic`): `scrutinee[k]
                // .matches('1')`, all together. A `-` adds nothing.
                let bit = |words: &[u64], index: u32| {
                    words
                        .get(index as usize / 64)
                        .is_some_and(|word| word >> (index % 64) & 1 == 1)
                };
                let mut all = None;
                for position in (0..width).filter(|position| bit(&mask, *position)) {
                    let symbol = if bit(&value, position) { '1' } else { '0' };
                    let member = push_element(
                        span,
                        scrutinee,
                        low + i64::from(position),
                        &element,
                        process,
                        context,
                    );
                    let member_type = context.process_ir.values[member.0 as usize].ty.clone();
                    let member_width = context.process_ir.values[member.0 as usize].bit_width;
                    let number = character_number(symbol, member_type.as_ref(), context)?;
                    let character = push_value(
                        span,
                        member_type,
                        member_width,
                        ProcessValueKind::Number(number),
                        context,
                    );
                    let result = bool_type(context.resolved);
                    let matched = inline_process_function(
                        function,
                        span,
                        Some(member),
                        &[character],
                        process,
                        context,
                        result.as_ref(),
                    )?;
                    all = Some(match all {
                        Some(all) => bool_binary(ProcessBinaryOp::And, all, matched, span, context),
                        None => matched,
                    });
                }
                return Some(all.unwrap_or_else(|| bool_value(true, span, context)));
            }
            let number = |words: Vec<u64>, context: &mut LoweringContext<'_>| {
                push_value(
                    span,
                    None,
                    Some(width),
                    ProcessValueKind::Number(ProcessNumber::Integer(words)),
                    context,
                )
            };
            let (mask, value) = (number(mask, context), number(value, context));
            let selected = push_value(
                span,
                None,
                Some(width),
                ProcessValueKind::Binary {
                    operation: ProcessBinaryOp::And,
                    left: scrutinee,
                    right: mask,
                },
                context,
            );
            Some(bool_binary(
                ProcessBinaryOp::Eq,
                selected,
                value,
                span,
                context,
            ))
        }
        ast::Pattern::Range { lo, hi, .. } => {
            let (low, high) = ((*lo).min(*hi), (*lo).max(*hi));
            let number = |n: i64, context: &mut LoweringContext<'_>| {
                let literal = ast::Expr::Int {
                    text: n.unsigned_abs().to_string(),
                    span,
                };
                let magnitude = value_ref_with_type(&literal, process, context, scrutinee_type);
                if n >= 0 {
                    return magnitude;
                }
                let zero = value_ref_with_type(
                    &ast::Expr::Int {
                        text: "0".to_string(),
                        span,
                    },
                    process,
                    context,
                    scrutinee_type,
                );
                let kind = ProcessValueKind::Binary {
                    operation: lower_binary_operator(
                        &ast::BinOp::Sub,
                        scrutinee_type,
                        scrutinee_type,
                    ),
                    left: zero,
                    right: magnitude,
                };
                let width = source_value_width(&kind, scrutinee_type, process, context);
                push_value(span, scrutinee_type.cloned(), width, kind, context)
            };
            let (low, high) = (number(low, context), number(high, context));
            let above = compare_values(ast::BinOp::Le, low, scrutinee, span, process, context)?;
            let below = compare_values(ast::BinOp::Le, scrutinee, high, span, process, context)?;
            Some(bool_binary(
                ProcessBinaryOp::And,
                above,
                below,
                span,
                context,
            ))
        }
        // An enum variant, a constant or a character: equality with the
        // value the decoded pattern names.
        ast::Pattern::Path(path) => {
            let value = match lower_pattern(pattern, scrutinee_type, context) {
                // The variant's number at the scrutinee's width, as a
                // character pattern's is below.
                ProcessPattern::Number(number) => {
                    let width = context
                        .process_ir
                        .values
                        .get(scrutinee.0 as usize)
                        .and_then(|value| value.bit_width);
                    push_value(
                        span,
                        scrutinee_type.cloned(),
                        width,
                        ProcessValueKind::Number(number),
                        context,
                    )
                }
                _ => value_ref_with_type(
                    &ast::Expr::Path(path.clone()),
                    process,
                    context,
                    scrutinee_type,
                ),
            };
            compare_values(ast::BinOp::Eq, scrutinee, value, span, process, context)
        }
        ast::Pattern::CharLit { .. } => {
            let ProcessPattern::Number(number) = lower_pattern(pattern, scrutinee_type, context)
            else {
                return None;
            };
            let width = context
                .process_ir
                .values
                .get(scrutinee.0 as usize)?
                .bit_width;
            let value = push_value(
                span,
                scrutinee_type.cloned(),
                width,
                ProcessValueKind::Number(number),
                context,
            );
            Some(bool_binary(
                ProcessBinaryOp::Eq,
                scrutinee,
                value,
                span,
                context,
            ))
        }
    }
}

/// The metavalue warning for a guarded match in a testbench: a scrutinee the
/// type's `Match::unknown` flags cannot match a bit pattern (as VHDL's
/// `std_match` reports). `None` when the type has no such check.
fn match_metavalue_warning(
    scrutinee: ProcessValueId,
    scrutinee_type: Option<&crate::types::Ty>,
    span: crate::diag::Span,
    process: &mut ProcessCfg,
    context: &mut LoweringContext<'_>,
    block: ProcessBlockId,
) -> Option<()> {
    if !matches!(process.activation, ProcessActivation::TimeZero) {
        return None;
    }
    let _ = scrutinee_type;
    let layout = operand_source_layout(scrutinee, process, context)?;
    let LayoutKind::Packed {
        range: Some(range),
        element_enum: Some(element),
        ..
    } = &layout.kind
    else {
        return None;
    };
    let element_type = nominal_type_from_name(element, context.resolved)?;
    let owner = process_type_key(&element_type, context)?;
    let check = context.functions.get_associated(&owner, "unknown")?;
    // An element read is the element type's whole value (a `Logic`
    // discriminant), not the vector's one value bit.
    let element_layout = process_layout_for_type(&element_type, layout.span, context)?;
    let result = bool_type(context.resolved);
    let mut unknown = None;
    for label in layout_labels(*range)? {
        let member = push_element(span, scrutinee, label, &element_layout, process, context);
        let flagged = inline_process_function(
            check,
            span,
            Some(member),
            &[],
            process,
            context,
            result.as_ref(),
        )?;
        unknown = Some(match unknown {
            Some(any) => bool_binary(ProcessBinaryOp::Or, any, flagged, span, context),
            None => flagged,
        });
    }
    let unknown = unknown?;
    let known = push_value(
        span,
        bool_type(context.resolved),
        Some(1),
        ProcessValueKind::Unary {
            operation: ProcessUnaryOp::Not,
            operand: unknown,
        },
        context,
    );
    let message = "match on a value holding a metavalue: its bit-pattern arms cannot match";
    let text = push_value(
        span,
        None,
        Some(u32::try_from(message.len() * 8).ok()?),
        ProcessValueKind::String(message.to_string()),
        context,
    );
    process.blocks[block.0 as usize]
        .instructions
        .push(ProcessInstruction::Runtime {
            operation: ProcessRuntimeOp::Warn,
            arguments: vec![known, text],
            format: None,
            span,
        });
    Some(())
}

/// A match whose arms are conditions (`match_needs_guards`): a first-match
/// chain of branches, one block testing each arm. The scrutinee is evaluated
/// once, before the chain.
fn lower_guarded_match(
    statement: &ast::MatchStmt,
    scrutinee_type: Option<&crate::types::Ty>,
    context: &mut LoweringContext<'_>,
    process: &mut ProcessCfg,
    block: ProcessBlockId,
) -> Option<ProcessBlockId> {
    let scrutinee = lower_scrutinee(&statement.scrutinee, process, context);
    let scrutinee = capture_scrutinee(scrutinee, context, process, block);
    if let MatchScrutinee::Value(value, _) = &scrutinee {
        if statement.arms.iter().any(|arm| {
            matches!(
                arm.pattern,
                ast::Pattern::BitPattern { .. } | ast::Pattern::Or { .. }
            )
        }) {
            match_metavalue_warning(
                *value,
                scrutinee_type,
                statement.span,
                process,
                context,
                block,
            );
        }
    }
    let mut test = block;
    let mut tails = Vec::new();
    let mut fell_through = true;
    // A tuple match the checker proved covered has no fall-through: its
    // last arm is what remains.
    let covered = context.typed.covers_tuple_match(statement.span);
    for (index, arm) in statement.arms.iter().enumerate() {
        let body = process.push_block();
        let last = covered && index + 1 == statement.arms.len();
        if last || pattern_has_wildcard(&arm.pattern) {
            process.blocks[test.0 as usize].terminator = ProcessTerminator::Goto(body);
            fell_through = false;
        } else {
            let condition =
                scrutinee_condition(&arm.pattern, &scrutinee, arm.span, process, context)?;
            let next = process.push_block();
            process.blocks[test.0 as usize].terminator = ProcessTerminator::Branch {
                condition,
                then_block: body,
                else_block: next,
            };
            test = next;
        }
        if let Some(tail) = lower_statements(&arm.body.stmts, context, process, body) {
            tails.push(tail);
        }
        if !fell_through {
            break;
        }
    }
    if fell_through {
        tails.push(test);
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
    // `for b in v` over a packed vector's elements: one body per label, each
    // reading `b` as `v[label]`, as hardware lowers it. The native loop
    // iterates ranges and arrays only, so this left the process unlowered.
    // ponytail: the iterable is re-read per element; a stored `v` written by
    // the body would be seen mid-loop.
    if !matches!(iterable, ast::Expr::Range { .. }) && crate::ir::expr_path(iterable).is_some() {
        let first_value = context.process_ir.values.len();
        let value = value_ref(iterable, process, context);
        let range = match process_value_source_layout(value, context.process_ir).map(|l| &l.kind) {
            Some(LayoutKind::Packed {
                range: Some(range), ..
            }) => Some(*range),
            _ => None,
        };
        truncate_process_values(context, first_value);
        if let Some(range) = range {
            let statements: Vec<Stmt> = crate::ir::loop_range(range.left, range.right)
                .into_iter()
                .flat_map(|label| {
                    let element = ast::Expr::Index {
                        base: Box::new(iterable.clone()),
                        index: Box::new(ast::Expr::Int {
                            text: label.to_string(),
                            span: variable.span,
                        }),
                        span: variable.span,
                    };
                    let map = std::collections::HashMap::from([(variable.text.clone(), element)]);
                    body.stmts
                        .iter()
                        .map(|statement| crate::ir::subst_stmt_paths(statement, &map))
                        .collect::<Vec<_>>()
                })
                .collect();
            return lower_statements(&statements, context, process, block);
        }
    }
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
        scope: Some(span),
        ty,
        layout: None,
    });

    let iterable = value_ref(iterable, process, context);
    let (block, iterable) = cfg_calls::lower_loop_iterable(iterable, block, process, context);
    if let Some(SourceLayout {
        kind: LayoutKind::Array { element, .. },
        ..
    }) = process_value_source_layout(iterable, context.process_ir)
    {
        process.locals[local.0 as usize].layout = Some((**element).clone());
    }
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
    if let Some(layout) = declaration
        .ty
        .as_ref()
        .and_then(|declared| generic_struct_layout(declared, declaration.span, context))
    {
        return Some(layout);
    }
    let mut layout = process_layout_for_type(ty?, declaration.span, context)?;
    if let Some(declared) = &declaration.ty {
        apply_process_declared_ranges(
            &mut layout,
            declared,
            context,
            &mut std::collections::HashSet::new(),
        )?;
    }
    Some(layout)
}

/// `Wrapper<8>`: a generic struct's fields with its arguments substituted.
/// Its checked type names only `Wrapper`, so a local of it had no layout and
/// did not lower (#196).
fn generic_struct_layout(
    declared: &ast::Type,
    span: crate::diag::Span,
    context: &LoweringContext<'_>,
) -> Option<crate::ir::SourceLayout> {
    let ast::Type::Generic { base, args, .. } = declared else {
        return None;
    };
    let ast::Type::Path(path) = base.as_ref() else {
        return None;
    };
    let definition = context.resolved.resolved(path.span)?;
    let declaration = context
        .modules
        .iter()
        .flat_map(|module| &module.items)
        .find_map(|item| match item {
            ast::Item::Struct(declaration)
                if !declaration.fields.is_empty()
                    && context.resolved.declared(declaration.name.span) == Some(definition) =>
            {
                Some(declaration)
            }
            _ => None,
        })?;
    let mut types = std::collections::HashMap::new();
    let mut values = std::collections::HashMap::new();
    for (parameter, argument) in declaration.params.params.iter().zip(args) {
        let name = parameter.name.text.clone();
        match argument {
            ast::GenericArg::PositionalType(ty) => {
                types.insert(name, ty.clone());
            }
            ast::GenericArg::Positional(value) if parameter.bound.is_some() => {
                values.insert(name, value.clone());
            }
            ast::GenericArg::Positional(value) => {
                types.insert(name, crate::ir::expr_to_type(value)?);
            }
            _ => return None,
        }
    }
    let fields = declaration
        .fields
        .iter()
        .map(|field| {
            let ty = crate::ir::subst_type_values(
                &crate::ir::subst_type_params(&field.ty, &types),
                &values,
            );
            let layout = generic_struct_layout(&ty, span, context).or_else(|| {
                process_layout_for_type(&process_declared_type(&ty, context)?, span, context)
            })?;
            Some(crate::ir::LayoutField {
                name: field.name.text.clone(),
                direction: None,
                layout,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(crate::ir::SourceLayout {
        span,
        kind: LayoutKind::Struct {
            name: context.functions.type_head_key(base)?,
            view: None,
            fields,
        },
    })
}

/// Checked types retain array lengths, not their written labels. Restore the
/// declaration's ranges recursively before local reads and metadata offsets
/// consume that representation. Alias cycles fail closed without a depth cap.
fn apply_process_declared_ranges(
    layout: &mut SourceLayout,
    declared: &ast::Type,
    context: &LoweringContext<'_>,
    aliases: &mut std::collections::HashSet<crate::resolve::DefId>,
) -> Option<()> {
    match declared {
        ast::Type::Path(path) => {
            if let Some((definition, aliased)) =
                context.resolved.resolved(path.span).and_then(|definition| {
                    context
                        .type_aliases
                        .get(&definition)
                        .map(|ty| (definition, *ty))
                })
            {
                if !aliases.insert(definition) {
                    return None;
                }
                apply_process_declared_ranges(layout, aliased, context, aliases)?;
                aliases.remove(&definition);
            } else if let Some((definition, base)) =
                context.resolved.resolved(path.span).and_then(|definition| {
                    sized_struct_base(definition, context).map(|base| (definition, base))
                })
            {
                // `W` for `struct W(Logic[7..0])` takes the wrapped range.
                if !aliases.insert(definition) {
                    return None;
                }
                apply_process_declared_ranges(layout, base, context, aliases)?;
                aliases.remove(&definition);
            }
        }
        ast::Type::Indexed { base, index, .. } => {
            if let Some(ast::Expr::Range { lo, hi, .. }) = index.as_deref() {
                let left =
                    crate::ir::eval_const_fns(lo, context.constant_integers, context.functions, 0)?;
                let right =
                    crate::ir::eval_const_fns(hi, context.constant_integers, context.functions, 0)?;
                let declared_range = crate::ir::LayoutRange { left, right };
                match &mut layout.kind {
                    LayoutKind::Packed { width, range, .. } => {
                        *width = u32::try_from(declared_range.len()?).ok()?;
                        *range = Some(declared_range);
                    }
                    LayoutKind::Array { range, .. } => *range = Some(declared_range),
                    _ => {}
                }
            }
            if let LayoutKind::Array { element, .. } = &mut layout.kind {
                apply_process_declared_ranges(element, base, context, aliases)?;
            }
        }
        // A tuple's positions take their element types' ranges.
        ast::Type::Generic { base, args, .. } if tuple_type_base(base).is_some() => {
            if let LayoutKind::Struct { fields, .. } = &mut layout.kind {
                for (field, arg) in fields.iter_mut().zip(args) {
                    if let Some(ty) = arg.as_type() {
                        apply_process_declared_ranges(&mut field.layout, &ty, context, aliases)?;
                    }
                }
            }
        }
        // `integer<0..9>`: the checked type is `integer`, the range a
        // constraint the local keeps for its runtime check (#165).
        ast::Type::Generic { base, args, .. }
            if matches!(base.as_ref(), ast::Type::Path(path)
                if path.segments.last().is_some_and(|segment| segment.text == "integer")) =>
        {
            if let (
                [ast::GenericArg::Positional(ast::Expr::Range { lo, hi, .. })],
                LayoutKind::Scalar { value_range, .. },
            ) = (args.as_slice(), &mut layout.kind)
            {
                let bound = |bound: &ast::Expr| {
                    crate::ir::eval_const_fns(
                        bound,
                        context.constant_integers,
                        context.functions,
                        0,
                    )
                };
                if let (Some(left), Some(right)) = (bound(lo), bound(hi)) {
                    *value_range = Some((left.min(right), left.max(right)));
                }
            }
        }
        ast::Type::Generic { base, .. } => {
            apply_process_declared_ranges(layout, base, context, aliases)?;
        }
        ast::Type::View { target, .. } => {
            apply_process_declared_ranges(layout, target, context, aliases)?;
        }
        ast::Type::Tuple { .. } => {}
    }
    Some(())
}

/// A declaration whose width reads a bound value (`let r:
/// unsigned[value'length]` in an inlined body), with that width folded to a
/// literal: declared types evaluate only module constants.
fn sized_by_value(
    declaration: &ast::LetDecl,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ast::LetDecl> {
    let Some(ast::Type::Indexed {
        base,
        index: Some(index),
        span,
    }) = declaration.ty.as_ref()
    else {
        return None;
    };
    if matches!(
        index.as_ref(),
        ast::Expr::Int { .. } | ast::Expr::Range { .. }
    ) || crate::ir::eval_const_fns(index, context.constant_integers, context.functions, 0)
        .is_some()
    {
        return None;
    }
    let first = context.process_ir.values.len();
    let value = value_ref_with_type(index, process, context, Some(&crate::types::Ty::Integer));
    let width = arena_constant_integer(value, &context.process_ir.values);
    truncate_process_values(context, first);
    let mut sized = declaration.clone();
    sized.ty = Some(ast::Type::Indexed {
        base: base.clone(),
        index: Some(Box::new(ast::Expr::Int {
            text: width?.to_string(),
            span: ast::expr_span(index),
        })),
        span: *span,
    });
    Some(sized)
}

fn push_local(
    process: &mut ProcessCfg,
    declaration: &ast::LetDecl,
    context: &LoweringContext<'_>,
) -> ProcessLocalId {
    let id = ProcessLocalId(process.locals.len() as u32);
    let declared = declaration
        .ty
        .as_ref()
        .and_then(|ty| process_declared_type(ty, context));
    // A sized declaration is the local's type: `let u: signed[8] = 0 - 100;`
    // is a `signed[8]`, not the `integer` its initializer computes in. A
    // width-generic one (`let r: unsigned = v;`) takes the initializer's.
    let ty = declared
        .clone()
        .filter(|ty| {
            !matches!(ty, crate::types::Ty::Array { len: 0, .. }) && is_concrete_type(ty, context)
        })
        .or_else(|| match (&declared, declaration.value.as_ref()) {
            // `let xs: T[] = [a, b, c];` takes the literal's length.
            (
                Some(crate::types::Ty::Array {
                    len: 0,
                    family: None,
                    ..
                }),
                Some(ast::Expr::Array { elems, .. }),
            ) if elems.is_empty() => declared.clone(),
            (
                Some(crate::types::Ty::Array {
                    elem,
                    len: 0,
                    family: None,
                    low,
                }),
                Some(ast::Expr::Array { elems, .. }),
            ) => Some(crate::types::Ty::Array {
                elem: elem.clone(),
                len: u32::try_from(elems.len()).ok()?,
                family: None,
                low: *low,
            }),
            _ => None,
        })
        .or_else(|| {
            declaration
                .value
                .as_ref()
                .and_then(|value| context.typed.expr_type(ast::expr_span(value)))
                .filter(|ty| !matches!(ty, crate::types::Ty::Error))
                .cloned()
        })
        .or(declared);
    let layout = process_local_layout(declaration, ty.as_ref(), context);
    process.locals.push(ProcessLocal {
        id,
        name: declaration.name.text.clone(),
        source: context.resolved.declared(declaration.name.span),
        span: declaration.span,
        scope: None,
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

/// A conversion's source key: a type's own, or `Logic[]` for an anonymous
/// array of `Logic`, as `From<Logic[]>` registers it.
fn conversion_source_key(ty: &crate::types::Ty, context: &LoweringContext<'_>) -> Option<String> {
    match ty {
        crate::types::Ty::Array {
            family: None, elem, ..
        } => Some(format!("{}[]", process_type_key(elem, context)?)),
        other => process_type_key(other, context),
    }
}

fn process_type_key(ty: &crate::types::Ty, context: &LoweringContext<'_>) -> Option<String> {
    match ty {
        crate::types::Ty::Integer => Some("integer".into()),
        crate::types::Ty::Natural => Some("natural".into()),
        crate::types::Ty::Real => Some("real".into()),
        crate::types::Ty::Char => Some("Char".into()),
        crate::types::Ty::Named(definition) => context.functions.nominal_type_key(*definition),
        crate::types::Ty::Array {
            family: Some(family),
            ..
        } => Some(context.functions.canonical_type_key(family)),
        crate::types::Ty::Array { family: None, .. }
        | crate::types::Ty::Tuple(_)
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
    // The graph is a DAG: an unrolled loop's selects share their operands,
    // so a walk without `seen` revisits them exponentially.
    let mut pending = vec![id];
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(value) = context.process_ir.values.get(id.0 as usize) else {
            continue;
        };
        if matches!(value.ty, Some(crate::types::Ty::Real))
            || process_value_source_layout(id, context.process_ir).is_some_and(layout_is_real)
        {
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
            ProcessValueKind::Unary {
                operation: ProcessUnaryOp::Real(_) | ProcessUnaryOp::IntegerToReal,
                ..
            } => return true,
            ProcessValueKind::Binary {
                operation:
                    ProcessBinaryOp::FloatAdd
                    | ProcessBinaryOp::FloatSub
                    | ProcessBinaryOp::FloatMul
                    | ProcessBinaryOp::FloatDiv
                    | ProcessBinaryOp::FloatRem
                    | ProcessBinaryOp::FloatPow,
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
        qualifier: None,
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
        "natural" => crate::types::Ty::Natural,
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
    // `real(x)` for a type with `impl From<S> for real` (`real(ufixed)`) is
    // that conversion, not the kernel reading of its word.
    if let Some(function) = operand_type
        .clone()
        .or_else(|| process_value_type(operand, context))
        .and_then(|ty| process_type_key(&ty, context))
        .and_then(|source| context.functions.get_conversion(&name, &source))
    {
        return inline_process_function(
            function,
            ast::expr_span(expression),
            None,
            &[operand],
            process,
            context,
            Some(&target),
        );
    }
    let operand_is_real = matches!(operand_type, Some(crate::types::Ty::Real))
        || process_value_is_real(operand, context);
    let natural_operand = matches!(operand_type, Some(crate::types::Ty::Natural));
    let integer_operand = matches!(operand_type, Some(crate::types::Ty::Integer));
    let checked = |operation| ProcessValueKind::Unary { operation, operand };
    let kind = if name == "natural" && operand_is_real {
        checked(ProcessUnaryOp::RealToNatural)
    } else if name == "natural" && integer_operand {
        checked(ProcessUnaryOp::IntegerToNatural)
    } else if name == "integer" && natural_operand {
        checked(ProcessUnaryOp::NaturalToInteger)
    } else if name == "real" && natural_operand {
        checked(ProcessUnaryOp::NaturalToReal)
    } else if name == "integer" && operand_is_real {
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
        qualifier: None,
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
    // A string literal fills a newtype's field (`B("0110")`); the raw resize
    // shapes it there.
    if matches!(argument, ast::Expr::StrLit { .. })
        && process_layout_for_type(&target_type, *span, context)
            .is_some_and(|layout| matches!(layout.kind, LayoutKind::Struct { ref fields, .. } if fields.len() == 1))
    {
        return None;
    }
    let target = process_type_key(&target_type, context)?;
    // The checker types an unsized conversion's argument as its target
    // (`unsigned(v)`); an anonymous array argument is the array it is.
    let probe = context.process_ir.values.len();
    let lowered = value_ref_with_type(argument, process, context, None);
    let own = process_value_type(lowered, context)
        .filter(|ty| matches!(ty, crate::types::Ty::Array { family: None, .. }));
    truncate_process_values(context, probe);
    let source_type = own.or_else(|| {
        context
            .typed
            .expr_type(ast::expr_span(argument))
            .filter(|ty| !matches!(ty, crate::types::Ty::Error))
            .cloned()
    })?;
    let source = conversion_source_key(&source_type, context)?;
    let first_value = context.process_ir.values.len();
    let operand = value_ref_with_type(argument, process, context, Some(&source_type));

    if let Some(function) = context.functions.get_conversion(&target, &source) {
        let result = inline_process_function(
            function,
            *span,
            None,
            &[operand],
            process,
            context,
            Some(&target_type),
        );
        if result.is_some() {
            return result;
        }
        // A body that is not one value (a loop, a statement `if`) expands in
        // the CFG as any call does; the raw resize below used to stand in for
        // it silently. The CFG finds the body by these two types.
        truncate_process_values(context, first_value);
        let callee = value_ref(callee, process, context);
        let operand = value_ref_with_type(argument, process, context, Some(&source_type));
        let kind = ProcessValueKind::Call {
            callee,
            type_arguments: Vec::new(),
            arguments: vec![operand],
            bang: false,
        };
        let width = source_value_width(&kind, Some(&target_type), process, context);
        return Some(push_value(*span, Some(target_type), width, kind, context));
    }

    if target == source || process_enum_conversion_is_total(context.design, &target, &source) {
        // A width-generic target (`unsigned(v)` for a `v: signed`) keeps the
        // operand's width: only the family changes.
        let target_type = match (&target_type, &source_type) {
            (
                crate::types::Ty::Array {
                    len: 0,
                    elem,
                    family,
                    ..
                },
                crate::types::Ty::Array { len, .. },
            ) => crate::types::Ty::Array {
                elem: elem.clone(),
                len: *len,
                family: family.clone(),
                low: 0,
            },
            _ => target_type,
        };
        let kind = ProcessValueKind::RawResize { operand };
        let width = source_value_width(&kind, Some(&target_type), process, context)
            .or(context.process_ir.values[operand.0 as usize].bit_width)?;
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
        qualifier: None,
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
            // A widening `resize(x, n)` is `F[n](x)` for `x`'s family `F`, as
            // in hardware: `From<signed> for signed` sign-extends.
            let widening = context.process_ir.values[operand.0 as usize]
                .bit_width
                .is_some_and(|operand_width| width > operand_width);
            if let (
                Some(crate::types::Ty::Array {
                    family: Some(family),
                    ..
                }),
                true,
            ) = (&operand_type, widening)
            {
                let owner = context.functions.canonical_type_key(family);
                let function = conversion_source_key(operand_type.as_ref()?, context)
                    .and_then(|source| context.functions.get_conversion(&owner, &source));
                if let Some(function) = function {
                    if let Some(value) = inline_with_self_format(
                        function,
                        family.clone(),
                        &args[1],
                        &[operand],
                        *span,
                        first_width_value,
                        process,
                        context,
                        target,
                    ) {
                        return Some(value);
                    }
                }
            }
            let target = match operand_type {
                Some(crate::types::Ty::Array { elem, family, .. }) => crate::types::Ty::Array {
                    elem,
                    family,
                    len: width,
                    low: 0,
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
            let eval = |bound: &ast::Expr| {
                crate::ir::eval_const_fns(bound, context.constant_integers, context.functions, 0)
            };
            let (len, low) = match index.as_ref() {
                ast::Expr::Range { lo, hi, .. } => {
                    let (left, right) = eval(lo).zip(eval(hi))?;
                    (
                        u32::try_from(left.abs_diff(right) + 1).ok()?,
                        left.min(right),
                    )
                }
                width => (u32::try_from(eval(width)?).ok()?, 0),
            };
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
                low,
            }
        }
        ast::Expr::Path(path) => {
            let definition = context.resolved.resolved(path.span)?;
            if context.resolved.def(definition)?.kind != crate::resolve::DefKind::Struct {
                return None;
            }
            // `unsigned(v)` on a `signed`: a packed family without a width
            // changes the family and keeps the operand's width.
            let family = context.resolved.qualified_name(definition)?;
            let packed = context.design.array_element_of_family.contains_key(&family)
                || family
                    .rsplit("::")
                    .next()
                    .is_some_and(|leaf| context.design.array_element_of_family.contains_key(leaf));
            if packed {
                // A string literal is the vector under the family: `B("0110")`
                // for `struct B(Bit[4])` lowered "0110" as text.
                let operand_type = match (&args[0], target) {
                    (
                        ast::Expr::StrLit { .. },
                        Some(crate::types::Ty::Array { elem, len, low, .. }),
                    ) => Some(crate::types::Ty::Array {
                        elem: elem.clone(),
                        len: *len,
                        family: None,
                        low: *low,
                    }),
                    _ => context
                        .typed
                        .expr_type(ast::expr_span(&args[0]))
                        .filter(|ty| !matches!(ty, crate::types::Ty::Error))
                        .cloned(),
                };
                let operand =
                    value_ref_with_type(&args[0], process, context, operand_type.as_ref());
                let width = context.process_ir.values[operand.0 as usize].bit_width?;
                let (elem, len) = match process_value_type(operand, context) {
                    Some(crate::types::Ty::Array { elem, len, .. }) if len > 0 => (elem, len),
                    _ => (Box::new(crate::types::Ty::Error), width),
                };
                let target = crate::types::Ty::Array {
                    elem,
                    family: Some(family),
                    len,
                    low: 0,
                };
                return Some(push_converted(
                    *span,
                    target,
                    width,
                    ProcessValueKind::RawResize { operand },
                    context,
                ));
            }
            match target {
                Some(crate::types::Ty::Named(target_definition))
                    if *target_definition == definition =>
                {
                    crate::types::Ty::Named(definition)
                }
                // A newtype over a vector is checked as that vector of its
                // own family (`struct B(Bit[4])` is `B`-family `Bit[4]`).
                Some(
                    vector @ crate::types::Ty::Array {
                        family: Some(name), ..
                    },
                ) if name.rsplit("::").next() == family.rsplit("::").next() => vector.clone(),
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
    // A string literal reads through the newtype's field: `B("0110")` with
    // `struct B(Bit[4])` lowered "0110" as text, which no raw resize can
    // carry.
    let field_type = match (&args[0], &target) {
        (ast::Expr::StrLit { .. }, crate::types::Ty::Array { elem, len, low, .. }) => {
            Some(crate::types::Ty::Array {
                elem: elem.clone(),
                len: *len,
                family: None,
                low: *low,
            })
        }
        (ast::Expr::StrLit { .. }, _) => {
            process_layout_for_type(&target, *span, context).and_then(|layout| match layout.kind {
                LayoutKind::Struct { fields, .. } if fields.len() == 1 => {
                    process_type_from_layout(&fields[0].layout, context.resolved)
                }
                _ => None,
            })
        }
        _ => None,
    };
    let operand_type = field_type
        .or_else(|| literal_arithmetic(&args[0]).map(|_| target.clone()))
        .or_else(|| {
            context
                .typed
                .expr_type(ast::expr_span(&args[0]))
                .filter(|ty| !matches!(ty, crate::types::Ty::Error))
                .cloned()
        });
    let operand = value_ref_with_type(&args[0], process, context, operand_type.as_ref());
    // `Wrapper(Inner { .x = 42 })`, `Pair((1, 2))`: a newtype derives its
    // base's fields, so a constructed base is the newtype built from the same
    // fields. A raw resize of an aggregate construct did not lower (#176).
    if let (crate::types::Ty::Named(_), ProcessValueKind::Construct { fields, spread, .. }) =
        (&target, &context.process_ir.values[operand.0 as usize].kind)
    {
        let kind = ProcessValueKind::Construct {
            ty: Some(target.clone()),
            fields: fields.clone(),
            spread: *spread,
        };
        let width = context.process_ir.values[operand.0 as usize].bit_width;
        let layout = process_layout_for_type(&target, *span, context);
        let value = push_value(*span, Some(target), width, kind, context);
        context.process_ir.value_layouts[value.0 as usize] = layout;
        return Some(value);
    }
    let kind = ProcessValueKind::RawResize { operand };
    let width = source_value_width(&kind, Some(&target), process, context)?;
    Some(push_converted(*span, target, width, kind, context))
}

/// A conversion's result with its target's vector layout, which a method on
/// it reads (`unsigned[8](x).reverse_bits()` slices `self[self'low..]`).
fn push_converted(
    span: crate::diag::Span,
    target: crate::types::Ty,
    width: u32,
    kind: ProcessValueKind,
    context: &mut LoweringContext<'_>,
) -> ProcessValueId {
    let layout = matches!(
        target,
        crate::types::Ty::Array {
            family: Some(_),
            ..
        }
    )
    .then(|| process_aggregate_layout_for_type(&target, span, context))
    .flatten()
    .filter(|layout| layout.packed_width() == Some(width));
    let value = push_value(span, Some(target), Some(width), kind, context);
    if layout.is_some() {
        context.process_ir.value_layouts[value.0 as usize] = layout;
    }
    value
}

/// Whether `base` is a struct with an `index_assign` method (`IndexAssign`).
fn index_assign_contract(base: &ast::Expr, context: &LoweringContext<'_>) -> bool {
    let Some(ty @ crate::types::Ty::Named(definition)) =
        context.typed.expr_type(ast::expr_span(base)).cloned()
    else {
        return false;
    };
    context.resolved.kind_of(definition) == Some(crate::resolve::DefKind::Struct)
        && process_type_key(&ty, context).is_some_and(|owner| {
            context
                .functions
                .get_associated(&owner, "index_assign")
                .is_some()
        })
}

/// `base[index]` through the struct's own `Index` impl for the index's type
/// (`Index<integer, _>`, `Index<Range, _>`), inlined with `base` as `self`.
fn custom_index_value(
    expression: &ast::Expr,
    base: &ast::Expr,
    index: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    if matches!(index, ast::Expr::PartialRange { .. }) {
        return None;
    }
    let ty @ crate::types::Ty::Named(definition) =
        context.typed.expr_type(ast::expr_span(base)).cloned()?
    else {
        return None;
    };
    if context.resolved.kind_of(definition) != Some(crate::resolve::DefKind::Struct) {
        return None;
    }
    let owner = process_type_key(&ty, context)?;
    let input = if matches!(index, ast::Expr::Range { .. }) {
        "Range".to_string()
    } else {
        context
            .typed
            .expr_type(ast::expr_span(index))
            .and_then(|ty| process_type_key(ty, context))
            .unwrap_or_else(|| "integer".to_string())
    };
    let function = context
        .functions
        .get_binary_operator("[]", &owner, Some(&input))?;
    let span = ast::expr_span(expression);
    let receiver = value_ref(base, process, context);
    let argument = match index {
        // The contract's `Range { left, right }` value, which the impl reads
        // as a struct (`r.left`).
        ast::Expr::Range { lo, hi, span } => {
            let range_type = context
                .typed
                .expr_type(*span)
                .filter(|ty| matches!(ty, crate::types::Ty::Named(_)))
                .cloned()?;
            let integer = Some(crate::types::Ty::Integer);
            let left = value_ref_with_type(lo, process, context, integer.as_ref());
            let right = value_ref_with_type(hi, process, context, integer.as_ref());
            let field = |name: &str, value| ProcessAggregateField {
                name: Some(name.to_string()),
                value: Some(value),
                span: *span,
            };
            let layout = process_aggregate_layout_for_type(&range_type, *span, context);
            let width = layout.as_ref().and_then(|layout| layout.packed_width());
            let value = push_value(
                *span,
                Some(range_type.clone()),
                width,
                ProcessValueKind::Construct {
                    ty: Some(range_type),
                    fields: vec![field("left", left), field("right", right)],
                    spread: None,
                },
                context,
            );
            context.process_ir.value_layouts[value.0 as usize] = layout;
            value
        }
        // An integer index binds the contract's `integer` parameter, a
        // literal included (`2` alone is two bits wide).
        _ if input == "integer" => {
            value_ref_with_type(index, process, context, Some(&crate::types::Ty::Integer))
        }
        _ => value_ref(index, process, context),
    };
    let result = context.typed.expr_type(span).cloned();
    inline_process_function(
        function,
        span,
        Some(receiver),
        &[argument],
        process,
        context,
        result.as_ref(),
    )
}

/// `float<32, 23>(1.5)`: a family's `From<Source>` impl, inlined with `Self`
/// standing for the format being built, so the body can read `Self'high`.
/// `None` when the family has no impl for the argument's type, and the
/// conversion is the kernel's raw resize.
fn lower_process_family_from(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    return_type: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    let ast::Expr::Call {
        callee,
        type_args,
        qualifier: None,
        args,
        bang: false,
        span,
    } = expression
    else {
        return None;
    };
    let (ast::Expr::Index { base, index, .. }, [argument], true) =
        (callee.as_ref(), args.as_slice(), type_args.is_empty())
    else {
        return None;
    };
    let ast::Expr::Path(path) = base.as_ref() else {
        return None;
    };
    let definition = context.resolved.resolved(path.span)?;
    let family = context.resolved.qualified_name(definition)?;
    let target = context.functions.canonical_type_key(&family);
    let first_value = context.process_ir.values.len();
    // The checker reads `family[range](x)` as a kernel conversion and records
    // no type for a literal argument, so the value says what it is.
    let checked = operand_type(argument, process, context);
    if let Some(value) = lower_process_wide_packed_resize(
        &family,
        index,
        argument,
        checked.as_ref(),
        *span,
        process,
        context,
        return_type,
    ) {
        return Some(value);
    }
    let operand = value_ref_with_type(argument, process, context, checked.as_ref());
    let source_type = checked
        .or_else(|| process_value_type(operand, context))
        .or_else(|| {
            Some(if process_value_is_real(operand, context) {
                crate::types::Ty::Real
            } else {
                crate::types::Ty::Integer
            })
        })?;
    let source = conversion_source_key(&source_type, context);
    let Some(function) = source
        .as_ref()
        .and_then(|source| context.functions.get_conversion(&target, source))
    else {
        truncate_process_values(context, first_value);
        return None;
    };
    // From an anonymous array's elements: an ordinary call, as in hardware.
    // A body of statements (a loop of element writes) is a call the CFG
    // expands, found by its two types as `named_conversion`'s is.
    if source.is_some_and(|source| source.ends_with("[]")) {
        let target_type = return_type
            .filter(|ty| !matches!(ty, crate::types::Ty::Error))
            .cloned()?;
        // The body builds the operand's own width (`unsigned[value'length]`);
        // a wider or narrower format resizes that, as `resize(unsigned(a), 8)`
        // does. Typed as the format, `unsigned[8](a)` of a `Logic[4]` did
        // not lower.
        let natural = match (&target_type, process_value_type(operand, context)) {
            (
                crate::types::Ty::Array {
                    elem,
                    family: family @ Some(_),
                    len,
                    ..
                },
                Some(crate::types::Ty::Array {
                    len: operand_len, ..
                }),
            ) if operand_len > 0 && operand_len != *len => Some(crate::types::Ty::Array {
                elem: elem.clone(),
                family: family.clone(),
                len: operand_len,
                low: 0,
            }),
            _ => None,
        };
        if let Some(natural) = natural {
            let converted = lower_logic_vector_call(
                function, definition, operand, *span, &natural, process, context,
            )?;
            let width = source_value_width(
                &ProcessValueKind::RawResize { operand: converted },
                Some(&target_type),
                process,
                context,
            )?;
            return Some(push_converted(
                *span,
                target_type,
                width,
                ProcessValueKind::RawResize { operand: converted },
                context,
            ));
        }
        return lower_logic_vector_call(
            function,
            definition,
            operand,
            *span,
            &target_type,
            process,
            context,
        );
    }
    inline_with_self_format(
        function,
        family,
        index,
        &[operand],
        *span,
        first_value,
        process,
        context,
        return_type,
    )
}

/// The leaf name of a std `signed`/`unsigned` family key, if it is one.
pub(super) fn std_integer_vector_family(family: &str) -> Option<&'static str> {
    match family.rsplit("::").next()? {
        "signed" => Some("signed"),
        "unsigned" => Some("unsigned"),
        _ => None,
    }
}

/// The two's-complement words of `1 << bit`.
pub(super) fn single_bit_words(bit: u32) -> Vec<u64> {
    let mut words = vec![0u64; bit as usize / 64 + 1];
    words[bit as usize / 64] = 1u64 << (bit % 64);
    words
}

/// `signed[N](x)` / `unsigned[N](x)` for a packed `signed`/`unsigned` `x`
/// when either side is wider than the 64-bit kernel word. std's `From`
/// bodies read their operand through `integer(..)`, which keeps only 64
/// bits, so `signed[129](signed[81] ..)` lost every bit above 63. Resize the
/// word itself instead: zero-extend (or truncate), and sign-extend a
/// `signed` source as `(x ^ s) - s` evaluated at the target width, `s`
/// being the source's sign bit.
#[allow(clippy::too_many_arguments)]
fn lower_process_wide_packed_resize(
    family: &str,
    index: &ast::Expr,
    argument: &ast::Expr,
    checked: Option<&crate::types::Ty>,
    span: crate::diag::Span,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    return_type: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    std_integer_vector_family(family)?;
    let Some(crate::types::Ty::Array {
        family: Some(source_family),
        len: source_len,
        ..
    }) = checked
    else {
        return None;
    };
    let source_signed = std_integer_vector_family(source_family)? == "signed";
    let eval = |bound: &ast::Expr| {
        crate::ir::eval_const_fns(bound, context.constant_integers, context.functions, 0)
    };
    let (left, right) = match index {
        ast::Expr::Range { lo, hi, .. } => eval(lo).zip(eval(hi))?,
        width => eval(width).map(|width| (width - 1, 0))?,
    };
    let target_width = u32::try_from(left.abs_diff(right) + 1).ok()?;
    let first_value = context.process_ir.values.len();
    let operand = value_ref_with_type(argument, process, context, checked);
    let source_width = context.process_ir.values[operand.0 as usize]
        .bit_width
        .filter(|width| *width > 0)
        .unwrap_or(*source_len);
    if source_width == 0 || target_width.max(source_width) <= 64 {
        truncate_process_values(context, first_value);
        return None;
    }
    let elem = match return_type {
        Some(crate::types::Ty::Array { elem, .. }) => elem.clone(),
        _ => Box::new(crate::types::Ty::Error),
    };
    let target_type = crate::types::Ty::Array {
        elem,
        family: Some(family.to_owned()),
        len: target_width,
        low: left.min(right),
    };
    let resized = push_value(
        span,
        None,
        Some(target_width),
        ProcessValueKind::RawResize { operand },
        context,
    );
    let kind = if source_signed && target_width > source_width {
        let sign = push_value(
            span,
            None,
            Some(target_width),
            ProcessValueKind::Number(ProcessNumber::Integer(single_bit_words(source_width - 1))),
            context,
        );
        let flipped = push_value(
            span,
            None,
            Some(target_width),
            ProcessValueKind::Binary {
                operation: ProcessBinaryOp::Xor,
                left: resized,
                right: sign,
            },
            context,
        );
        ProcessValueKind::Binary {
            operation: ProcessBinaryOp::Sub,
            left: flipped,
            right: sign,
        }
    } else {
        ProcessValueKind::RawResize { operand: resized }
    };
    Some(push_converted(
        span,
        target_type,
        target_width,
        kind,
        context,
    ))
}

/// A conversion from an anonymous array's elements, typed `target`: inlined
/// where its body allows, else a call the CFG expands.
fn lower_logic_vector_call(
    function: &ast::FnDecl,
    definition: crate::resolve::DefId,
    operand: ProcessValueId,
    span: crate::diag::Span,
    target: &crate::types::Ty,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    if let Some(value) = inline_process_function(
        function,
        span,
        None,
        &[operand],
        process,
        context,
        Some(target),
    ) {
        return Some(value);
    }
    let callee = push_value(
        span,
        None,
        None,
        ProcessValueKind::Definition(definition),
        context,
    );
    let kind = ProcessValueKind::Call {
        callee,
        type_arguments: Vec::new(),
        arguments: vec![operand],
        bang: false,
    };
    let width = source_value_width(&kind, Some(target), process, context);
    Some(push_value(span, Some(target.clone()), width, kind, context))
}

/// A family result uses its Self format; kernel-number helpers do not.
fn returns_self_format(function: &ast::FnDecl, layout: &SourceLayout) -> bool {
    let LayoutKind::Packed { family, .. } = &layout.kind else {
        return false;
    };
    function
        .ret
        .as_ref()
        .and_then(crate::ir::lower_helpers::type_head_name)
        .is_some_and(|head| head == "Self" || family.rsplit("::").next() == Some(head))
}

/// Inline `function` with `Self` standing for the format `family[index]`, as
/// a `From` conversion to that format and an associated call on it
/// (`ufixed<6, 2>::resize(x)`) both need. Values pushed from `first_value`
/// on are dropped when the format or the inlining fails.
#[allow(clippy::too_many_arguments)]
fn inline_with_self_format(
    function: &ast::FnDecl,
    family: String,
    index: &ast::Expr,
    operands: &[ProcessValueId],
    span: crate::diag::Span,
    first_value: usize,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    return_type: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    let eval = |bound: &ast::Expr| {
        crate::ir::eval_const_fns(bound, context.constant_integers, context.functions, 0)
    };
    let Some((left, right)) = (match index {
        ast::Expr::Range { lo, hi, .. } => eval(lo).zip(eval(hi)),
        width => eval(width).map(|width| (width - 1, 0)),
    }) else {
        truncate_process_values(context, first_value);
        return None;
    };
    let Some(len) = u32::try_from(left.abs_diff(right) + 1).ok() else {
        truncate_process_values(context, first_value);
        return None;
    };
    let elem = match return_type {
        Some(crate::types::Ty::Array { elem, .. }) => elem.clone(),
        _ => Box::new(crate::types::Ty::Error),
    };
    let target_type = crate::types::Ty::Array {
        elem,
        family: Some(family),
        len,
        low: 0,
    };
    let Some(mut layout) = process_layout_for_type(&target_type, span, context) else {
        truncate_process_values(context, first_value);
        return None;
    };
    if let LayoutKind::Packed { width, range, .. } = &mut layout.kind {
        *width = len;
        *range = Some(crate::ir::LayoutRange { left, right });
    }
    // Only a result of the type itself is a word of the requested format;
    // an integer helper keeps its declared inspection type.
    let in_format = returns_self_format(function, &layout);
    let format = push_value(
        span,
        Some(target_type.clone()),
        Some(len),
        ProcessValueKind::Number(ProcessNumber::Integer(vec![0])),
        context,
    );
    context.process_ir.value_layouts[format.0 as usize] = Some(layout);
    context.self_formats.push(format);
    let result = inline_process_function(
        function,
        span,
        None,
        operands,
        process,
        context,
        if in_format {
            Some(&target_type)
        } else {
            return_type
        },
    );
    context.self_formats.pop();
    match result {
        Some(result) if in_format => {
            // Source result types retain the explicit target range, rather
            // than a generic width-only layout or the source operand's range.
            // The native result/argument widths remain executable decisions.
            let shared = context
                .process_ir
                .calls
                .last()
                .filter(|call| call.value == result)
                .map(|call| context.process_ir.functions[call.function.0 as usize].result);
            let layout = context.process_ir.value_layouts[format.0 as usize]
                .as_ref()
                .unwrap()
                .clone();
            for value in std::iter::once(result).chain(shared) {
                if let ProcessValueKind::InlineCall { source, .. } =
                    &mut context.process_ir.values[value.0 as usize].kind
                {
                    source.result_layout = Some(layout.clone());
                }
            }
            Some(inherit_receiver_layout(result, format, context))
        }
        Some(result) => Some(result),
        None => {
            truncate_process_values(context, first_value);
            None
        }
    }
}

/// `ufixed<6, 2>::resize(x, ..)`: an associated function called on an
/// applied type, inlined with `Self` standing for that type's format.
fn lower_process_qualified_call(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    return_type: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    let ast::Expr::Call {
        callee,
        qualifier: Some(qualifier),
        args,
        bang: false,
        span,
        ..
    } = expression
    else {
        return None;
    };
    let ast::Type::Indexed {
        base,
        index: Some(index),
        ..
    } = qualifier.as_ref()
    else {
        return None;
    };
    let ast::Type::Path(path) = base.as_ref() else {
        return None;
    };
    let function = context.functions.get(callee)?;
    let definition = context.resolved.resolved(path.span)?;
    let family = context.resolved.qualified_name(definition)?;
    let first_value = context.process_ir.values.len();
    let operands: Vec<ProcessValueId> = args
        .iter()
        .map(|argument| {
            let checked = operand_type(argument, process, context);
            value_ref_with_type(argument, process, context, checked.as_ref())
        })
        .collect();
    inline_with_self_format(
        function,
        family,
        index,
        &operands,
        *span,
        first_value,
        process,
        context,
        return_type,
    )
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
        qualifier: None,
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
        ast::Expr::Path(path)
            if matches!(path.segments.last()?.text.as_str(), "default" | "new") =>
        {
            context
                .resolved
                .resolved(path.segments.get(path.segments.len().checked_sub(2)?)?.span)
        }
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
        qualifier: None,
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
    let function = context.functions.get(callee)?;
    if function.body.is_some() || function.params.iter().any(|parameter| parameter.is_self) {
        return None;
    }
    // A `core::intrinsics` function is a native operation, not a call.
    if let Some(intrinsic) = context.functions.intrinsic(function) {
        let real = crate::types::Ty::Real;
        let operands = args
            .iter()
            .map(|argument| value_ref_with_type(argument, process, context, Some(&real)))
            .collect::<Vec<_>>();
        let kind = match (intrinsic, operands.as_slice()) {
            (crate::ir::Intrinsic::Real(function), [operand]) => ProcessValueKind::Unary {
                operation: ProcessUnaryOp::Real(function),
                operand: *operand,
            },
            (crate::ir::Intrinsic::Pow, [left, right]) => ProcessValueKind::Binary {
                operation: ProcessBinaryOp::FloatPow,
                left: *left,
                right: *right,
            },
            _ => return None,
        };
        return Some(push_value(*span, Some(real), Some(64), kind, context));
    }
    let parameters = function
        .params
        .iter()
        .filter(|parameter| !parameter.is_self)
        .collect::<Vec<_>>();
    if parameters.len() != args.len() {
        return None;
    }

    // The declared type's leaf, through any aliases (`std::ffi::c_double`).
    let alias_leaf = |declared: &ast::Type| {
        let mut ty = declared;
        // The bound stops an alias cycle, which the checker reports.
        for _ in 0..64 {
            let ast::Type::Path(path) = ty else {
                break;
            };
            match context
                .resolved
                .resolved(path.span)
                .and_then(|id| context.type_aliases.get(&id))
            {
                Some(alias) => ty = alias,
                None => break,
            }
        }
        type_leaf(ty).map(str::to_string)
    };
    let abi_kind = |declared: Option<&ast::Type>, actual: Option<&crate::types::Ty>| {
        let leaf = declared.and_then(alias_leaf);
        (
            leaf.as_deref() == Some("real") || matches!(actual, Some(crate::types::Ty::Real)),
            leaf.as_deref() == Some("integer") || matches!(actual, Some(crate::types::Ty::Integer)),
        )
    };
    let c_scalar = |ty: Option<&ast::Type>| {
        crate::ir::CScalar::of(ty?, context.resolved, |path| {
            context
                .type_aliases
                .get(&context.resolved.resolved(path.span)?)
                .copied()
        })
    };
    let c_arguments = parameters
        .iter()
        .map(|parameter| c_scalar(parameter.ty.as_ref()))
        .collect();
    let c_result = c_scalar(function.ret.as_ref());
    let mut arguments = Vec::with_capacity(args.len());
    let mut float_arguments = Vec::with_capacity(args.len());
    let mut integer_arguments = Vec::with_capacity(args.len());
    let mut pointer_arguments = Vec::with_capacity(args.len());
    for (argument, parameter) in args.iter().zip(parameters) {
        let actual = context.typed.expr_type(ast::expr_span(argument)).cloned();
        let (float, integer) = abi_kind(parameter.ty.as_ref(), actual.as_ref());
        // A struct, fixed array or wide packed value crosses by pointer
        // (checked by the types pass).
        pointer_arguments.push(
            actual
                .as_ref()
                .is_some_and(|ty| ty.is_c_aggregate() && !is_c_handle(ty, context.resolved)),
        );
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
    // A C function with no result is called for its effect and reads as a
    // 0 word, which the statement that called it drops.
    let void_result = declared_return.is_none();
    let ty = if void_result {
        Some(crate::types::Ty::Integer)
    } else {
        return_type
            .cloned()
            .or_else(|| declared_return.and_then(|ty| declared_process_type(ty, context.resolved)))
    };
    let aggregate_result = ty
        .as_ref()
        .is_some_and(|ty| ty.is_c_aggregate() && !is_c_handle(ty, context.resolved));
    let kind = ProcessValueKind::ForeignCall {
        name: function.name.text.clone(),
        arguments,
        float_arguments,
        integer_arguments,
        float_result,
        integer_result,
        void_result,
        c_arguments,
        c_result,
        pointer_arguments,
        aggregate_result,
    };
    // A struct result is the whole aggregate, laid out by its type.
    let layout = aggregate_result
        .then(|| process_layout_for_type(ty.as_ref()?, *span, context))
        .flatten();
    let width = match &layout {
        Some(layout) => layout.packed_width(),
        None => source_value_width(&kind, ty.as_ref(), process, context),
    };
    let call = push_value(*span, ty, width, kind, context);
    if layout.is_some() {
        context.process_ir.value_layouts[call.0 as usize] = layout;
    }
    Some(call)
}

/// Whether `ty` is `std::ffi::Handle`: an opaque C pointer, which crosses
/// `extern "C"` by value rather than by pointer as other structs do.
fn is_c_handle(ty: &crate::types::Ty, resolved: &Resolved) -> bool {
    matches!(ty, crate::types::Ty::Named(id) if resolved.lang_of(*id) == Some("c_handle"))
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
        qualifier: None,
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
        | ProcessHostValueOp::ReadArray
        | ProcessHostValueOp::ReadUtf8Fixed
        | ProcessHostValueOp::ReadBinary
        | ProcessHostValueOp::FileExists
        | ProcessHostValueOp::StringLength
        | ProcessHostValueOp::StringIndex
        | ProcessHostValueOp::StringEqualsUtf8
        | ProcessHostValueOp::FramePush
        | ProcessHostValueOp::FramePop
        | ProcessHostValueOp::FrameRestore
        | ProcessHostValueOp::ArrayStore
        | ProcessHostValueOp::ArrayCopy
        | ProcessHostValueOp::ArraySlice
        | ProcessHostValueOp::ArrayPush
        | ProcessHostValueOp::ReadSymbols
        | ProcessHostValueOp::ArrayFrom
        | ProcessHostValueOp::StringFrom => return None,
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
        qualifier: None,
        args,
        bang: false,
        span,
    } = expression
    else {
        return None;
    };
    let [argument] = args.as_slice() else {
        return None;
    };
    // Inside an expression (`total + read<integer>(path)`) there is no
    // contextual type; the requested one is the result's.
    let requested = match type_args.as_slice() {
        [ast::GenericArg::PositionalType(requested)] => process_declared_type(requested, context),
        _ => None,
    };
    let return_type = return_type.or(requested.as_ref());
    let direct_string = matches!(type_args.as_slice(), [ast::GenericArg::PositionalType(requested)]
            if type_leaf(requested) == Some("string"));
    let operation = if builtin_callee_is(callee, "read", context) {
        if process_type_is_string(return_type) || return_type.is_none() && direct_string {
            match return_type {
                Some(crate::types::Ty::Array { len, .. }) if *len != 0 => {
                    ProcessHostValueOp::ReadUtf8Fixed
                }
                _ => ProcessHostValueOp::ReadUtf8,
            }
        } else if runtime_array_element(return_type)
            .is_some_and(|element| runtime_symbols(element, *span, context).is_some())
        {
            ProcessHostValueOp::ReadSymbols
        } else if runtime_array_element(return_type).is_some() {
            ProcessHostValueOp::ReadArray
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
    let path = match argument {
        ast::Expr::StrLit {
            text: path,
            span: path_span,
        } => {
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
                low: 0,
            };
            let path_kind = ProcessValueKind::String(path);
            let path_width = source_value_width(&path_kind, Some(&path_type), process, context);
            push_value(*path_span, Some(path_type), path_width, path_kind, context)
        }
        // Computed when the test runs; the backend resolves it against the
        // source directory as the literal above is.
        _ => value_ref(argument, process, context),
    };
    let mut arguments = vec![path];
    if operation == ProcessHostValueOp::ReadSymbols {
        let symbols = runtime_symbols(runtime_array_element(return_type)?, *span, context)?;
        let alphabet = symbols
            .iter()
            .map(|(symbol, _)| *symbol)
            .collect::<String>();
        let ty = crate::types::Ty::Array {
            elem: Box::new(crate::types::Ty::Char),
            family: None,
            len: u32::try_from(symbols.len()).ok()?,
            low: 0,
        };
        let kind = ProcessValueKind::String(alphabet);
        let width = source_value_width(&kind, Some(&ty), process, context);
        arguments.push(push_value(*span, Some(ty), width, kind, context));
    }
    if operation == ProcessHostValueOp::ReadArray {
        // One element consumes ceil(width / 8) bytes, as a fixed read does.
        let element = runtime_array_element(return_type)?;
        let width = process_layout_for_type(element, *span, context)?.packed_width()?;
        if width == 0 {
            return None;
        }
        let bytes =
            ProcessValueKind::Number(ProcessNumber::Integer(vec![u64::from(width.div_ceil(8))]));
        arguments.push(push_value(
            *span,
            Some(crate::types::Ty::Integer),
            Some(64),
            bytes,
            context,
        ));
    }
    let kind = ProcessValueKind::HostCall {
        operation,
        arguments,
    };
    let ty = return_type.cloned().or_else(|| match operation {
        ProcessHostValueOp::ReadUtf8 => Some(crate::types::Ty::Array {
            elem: Box::new(crate::types::Ty::Char),
            family: None,
            len: 0,
            low: 0,
        }),
        ProcessHostValueOp::FileExists => bool_type(context.resolved),
        _ => None,
    });
    let width = source_value_width(&kind, ty.as_ref(), process, context);
    Some(push_value(*span, ty, width, kind, context))
}

/// A character enum's symbols with their discriminants, in code order: the
/// code a runtime array (`read<Logic[]>`) holds per element is the position.
fn runtime_symbols(
    element: &crate::types::Ty,
    span: crate::diag::Span,
    context: &LoweringContext<'_>,
) -> Option<Vec<(char, u64)>> {
    let layout = process_layout_for_type(element, span, context)?;
    let name = symbol_enum(&layout, context)?;
    let mut symbols = context
        .design
        .enum_syms
        .get(&name)?
        .iter()
        .map(|(discriminant, symbol)| Some((symbol.chars().nth(1)?, *discriminant)))
        .collect::<Option<Vec<_>>>()?;
    symbols.sort_by_key(|(_, discriminant)| *discriminant);
    Some(symbols)
}

/// The element a symbol code stands for: a select over the enum's values,
/// so its X/Z plane is the constant's.
fn symbol_decode(
    code: ProcessValueId,
    element: &crate::types::Ty,
    span: crate::diag::Span,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let symbols = runtime_symbols(element, span, context)?;
    let constant = |discriminant: u64, context: &mut LoweringContext<'_>| {
        let kind = ProcessValueKind::Number(ProcessNumber::Integer(vec![discriminant]));
        let width = source_value_width(&kind, Some(element), process, context);
        push_value(span, Some(element.clone()), width, kind, context)
    };
    let (_, last) = *symbols.last()?;
    let mut result = constant(last, context);
    for (code_number, (_, discriminant)) in symbols.iter().enumerate().rev().skip(1) {
        let number = push_value(
            span,
            Some(crate::types::Ty::Integer),
            Some(64),
            ProcessValueKind::Number(ProcessNumber::Integer(vec![code_number as u64])),
            context,
        );
        let condition = bool_binary(ProcessBinaryOp::Eq, code, number, span, context);
        let then_value = constant(*discriminant, context);
        let width = context.process_ir.values[then_value.0 as usize].bit_width;
        result = push_value(
            span,
            Some(element.clone()),
            width,
            ProcessValueKind::Select {
                condition,
                then_value,
                else_value: result,
            },
            context,
        );
    }
    Some(result)
}

/// The symbol code of `value`, a character enum element: the position of
/// the variant it equals.
fn symbol_encode(
    value: ProcessValueId,
    element: &crate::types::Ty,
    span: crate::diag::Span,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let symbols = runtime_symbols(element, span, context)?;
    let code = |number: usize, context: &mut LoweringContext<'_>| {
        push_value(
            span,
            Some(crate::types::Ty::Integer),
            Some(64),
            ProcessValueKind::Number(ProcessNumber::Integer(vec![number as u64])),
            context,
        )
    };
    let mut result = code(symbols.len() - 1, context);
    for (code_number, (_, discriminant)) in symbols.iter().enumerate().rev().skip(1) {
        let kind = ProcessValueKind::Number(ProcessNumber::Integer(vec![*discriminant]));
        let width = context.process_ir.values[value.0 as usize]
            .bit_width
            .or_else(|| source_value_width(&kind, Some(element), process, context));
        let variant = push_value(span, Some(element.clone()), width, kind, context);
        let condition = bool_binary(ProcessBinaryOp::Eq, value, variant, span, context);
        let then_value = code(code_number, context);
        result = push_value(
            span,
            Some(crate::types::Ty::Integer),
            Some(64),
            ProcessValueKind::Select {
                condition,
                then_value,
                else_value: result,
            },
            context,
        );
    }
    Some(result)
}

/// The runtime array element read (`StringIndex`) a place is rooted in, and
/// the projection over it when the place is part of the element.
fn runtime_element_place(
    target: ProcessValueId,
    context: &LoweringContext<'_>,
) -> Option<(ProcessValueId, Option<ProcessValueId>)> {
    // A character enum element reads through `symbol_decode`, whose first
    // condition compares the element's code.
    if let ProcessValueKind::Select { condition, .. } =
        &context.process_ir.values[target.0 as usize].kind
    {
        if let ProcessValueKind::Binary { left, .. } =
            &context.process_ir.values[condition.0 as usize].kind
        {
            if matches!(
                context.process_ir.values[left.0 as usize].kind,
                ProcessValueKind::HostCall {
                    operation: ProcessHostValueOp::StringIndex,
                    ..
                }
            ) {
                return Some((*left, None));
            }
        }
    }
    let mut current = target;
    loop {
        match &context.process_ir.values[current.0 as usize].kind {
            ProcessValueKind::HostCall {
                operation: ProcessHostValueOp::StringIndex,
                ..
            } => return Some((current, (current != target).then_some(target))),
            ProcessValueKind::Field { base, .. }
            | ProcessValueKind::Index { base, .. }
            | ProcessValueKind::PackedSlice { base, .. } => current = *base,
            _ => return None,
        }
    }
}

/// `place` with its root `from` replaced by `to`.
fn rebase_place(
    place: ProcessValueId,
    from: ProcessValueId,
    to: ProcessValueId,
    context: &mut LoweringContext<'_>,
) -> ProcessValueId {
    if place == from {
        return to;
    }
    let mut node = context.process_ir.values[place.0 as usize].clone();
    match &mut node.kind {
        ProcessValueKind::Field { base, .. }
        | ProcessValueKind::Index { base, .. }
        | ProcessValueKind::PackedSlice { base, .. } => {
            *base = rebase_place(*base, from, to, context);
        }
        _ => unreachable!("runtime_element_place walks only projections"),
    }
    push_value(node.span, node.ty, node.bit_width, node.kind, context)
}

/// The fixed type of an array literal given to an unconstrained `T[]`
/// (`xs = [7, 8, 9]` is an `integer[3]`, converted to a runtime array).
fn literal_type(value: &ast::Expr, ty: Option<&crate::types::Ty>) -> Option<crate::types::Ty> {
    let ast::Expr::Array { elems, .. } = value else {
        return None;
    };
    match ty? {
        crate::types::Ty::Array {
            elem,
            len: 0,
            family: None,
            low,
        } if !elems.is_empty() => Some(crate::types::Ty::Array {
            elem: elem.clone(),
            len: u32::try_from(elems.len()).ok()?,
            family: None,
            low: *low,
        }),
        _ => None,
    }
}

/// `value` assigned to a runtime array of type `ty`: its own copy, unless it
/// is fresh from a read or a copy already.
fn runtime_array_copy(
    value: ProcessValueId,
    ty: Option<&crate::types::Ty>,
    context: &mut LoweringContext<'_>,
) -> ProcessValueId {
    // A fixed string assigned to a runtime one (`names.push("de")`) becomes
    // a runtime string of its text.
    let runtime_string = matches!(
        ty,
        Some(crate::types::Ty::Array { elem, len: 0, family: None, .. })
            if matches!(elem.as_ref(), crate::types::Ty::Char)
    );
    if runtime_string
        && (matches!(
            process_value_type(value, context),
            Some(crate::types::Ty::Array { elem, len, family: None, .. })
                if len > 0 && matches!(elem.as_ref(), crate::types::Ty::Char)
        ) || matches!(
            &context.process_ir.values[value.0 as usize].kind,
            ProcessValueKind::String(text) if !text.is_empty()
        ))
    {
        let span = context.process_ir.values[value.0 as usize].span;
        return push_value(
            span,
            ty.cloned(),
            Some(64),
            ProcessValueKind::HostCall {
                operation: ProcessHostValueOp::StringFrom,
                arguments: vec![value],
            },
            context,
        );
    }
    let Some(element) = runtime_array_element(ty).cloned() else {
        return value;
    };
    // A fixed array (`[1, 2]`, an `integer[4]` local) becomes a new runtime
    // array of its elements.
    let span = context.process_ir.values[value.0 as usize].span;
    if let Some(crate::types::Ty::Array {
        len, family: None, ..
    }) = process_value_type(value, context).filter(
        |fixed| matches!(fixed, crate::types::Ty::Array { len, family: None, .. } if *len > 0),
    ) {
        let width = process_layout_for_type(&element, span, context)
            .and_then(|layout| layout.packed_width())
            .filter(|_| runtime_symbols(&element, span, context).is_none());
        if let Some(width) = width {
            let number = |n: u64, context: &mut LoweringContext<'_>| {
                push_value(
                    span,
                    Some(crate::types::Ty::Integer),
                    Some(64),
                    ProcessValueKind::Number(ProcessNumber::Integer(vec![n])),
                    context,
                )
            };
            let count = number(u64::from(len), context);
            let element_width = number(u64::from(width), context);
            return push_value(
                span,
                ty.cloned(),
                Some(64),
                ProcessValueKind::HostCall {
                    operation: ProcessHostValueOp::ArrayFrom,
                    arguments: vec![value, count, element_width],
                },
                context,
            );
        }
    }
    let mut fresh = value;
    while let ProcessValueKind::InlineCall { operand, .. } =
        context.process_ir.values[fresh.0 as usize].kind
    {
        fresh = operand;
    }
    if matches!(
        context.process_ir.values[fresh.0 as usize].kind,
        ProcessValueKind::HostCall {
            operation: ProcessHostValueOp::ReadArray
                | ProcessHostValueOp::ReadSymbols
                | ProcessHostValueOp::ArrayCopy
                | ProcessHostValueOp::ArraySlice
                | ProcessHostValueOp::ArrayFrom,
            ..
        }
    ) {
        return value;
    }
    // `[]`: the empty array, handle zero.
    if matches!(
        &context.process_ir.values[fresh.0 as usize].kind,
        ProcessValueKind::Array(elements) if elements.is_empty()
    ) {
        let span = context.process_ir.values[value.0 as usize].span;
        return push_value(
            span,
            ty.cloned(),
            Some(64),
            ProcessValueKind::Number(ProcessNumber::Integer(vec![0])),
            context,
        );
    }
    // Arrays nested in it are copied too: `integer[][]` is one level deep.
    let mut levels = 0u64;
    let mut inner = runtime_array_element(ty);
    while let Some(element) = inner.and_then(|element| runtime_array_element(Some(element))) {
        levels += 1;
        inner = Some(element);
    }
    let depth = push_value(
        span,
        Some(crate::types::Ty::Integer),
        Some(64),
        ProcessValueKind::Number(ProcessNumber::Integer(vec![levels])),
        context,
    );
    push_value(
        span,
        ty.cloned(),
        Some(64),
        ProcessValueKind::HostCall {
            operation: ProcessHostValueOp::ArrayCopy,
            arguments: vec![value, depth],
        },
        context,
    )
}

/// The element type of a runtime-sized array request (`unsigned[16][]`), not
/// a string: `Char[]` is read as UTF-8.
fn runtime_array_element(ty: Option<&crate::types::Ty>) -> Option<&crate::types::Ty> {
    match ty? {
        crate::types::Ty::Array {
            elem,
            len: 0,
            family: None,
            ..
        } if !matches!(elem.as_ref(), crate::types::Ty::Char) => Some(elem),
        _ => None,
    }
}

/// A runtime-owned string or array is held as a 64-bit handle: an
/// unconstrained array of characters, or of packed elements up to 64 bits.
fn runtime_handle_width(layout: Option<&SourceLayout>) -> Option<u32> {
    let Some(LayoutKind::Array {
        range: None,
        element,
    }) = layout.map(|layout| &layout.kind)
    else {
        return None;
    };
    let character = matches!(
        element.kind,
        LayoutKind::Scalar {
            domain: crate::ir::ScalarDomain::Character,
            ..
        }
    );
    // An element that is a runtime array itself (`integer[][]`) is a handle.
    (character
        || element.packed_width().is_some_and(|width| width > 0)
        || runtime_handle_width(Some(element)).is_some())
    .then_some(64)
}

fn process_value_is_runtime_string(
    id: ProcessValueId,
    process: &ProcessCfg,
    context: &LoweringContext<'_>,
) -> bool {
    let Some(value) = context.process_ir.values.get(id.0 as usize) else {
        return false;
    };
    match &value.kind {
        ProcessValueKind::InlineCall { operand, .. } => {
            process_value_is_runtime_string(*operand, process, context)
        }
        // An element of `integer[][]` is a runtime array itself.
        ProcessValueKind::HostCall {
            operation: ProcessHostValueOp::StringIndex,
            ..
        } if value.ty.as_ref().is_some_and(|ty| {
            matches!(
                ty,
                crate::types::Ty::Array {
                    len: 0,
                    family: None,
                    ..
                }
            )
        }) =>
        {
            true
        }
        ProcessValueKind::HostCall {
            operation:
                ProcessHostValueOp::ReadUtf8
                | ProcessHostValueOp::ReadArray
                | ProcessHostValueOp::ArrayCopy
                | ProcessHostValueOp::ArraySlice
                | ProcessHostValueOp::ArrayPush
                | ProcessHostValueOp::ReadSymbols
                | ProcessHostValueOp::ArrayFrom
                | ProcessHostValueOp::StringFrom,
            ..
        } => true,
        ProcessValueKind::Storage(storage) => context
            .process_ir
            .storages
            .get(storage.0 as usize)
            .and_then(|storage| storage.initializer)
            .is_some_and(|initializer| {
                process_value_is_runtime_string(initializer, process, context)
            }),
        // A runtime array local is one whatever it was initialized with (an
        // unset one, `[]`, holds the empty array).
        ProcessValueKind::Local {
            process: owner,
            local,
        } if *owner == process.id
            && process.locals.get(local.0 as usize).is_some_and(|local| {
                matches!(
                    local.ty,
                    Some(crate::types::Ty::Array {
                        len: 0,
                        family: None,
                        ..
                    })
                )
            }) =>
        {
            true
        }
        ProcessValueKind::Local {
            process: owner,
            local,
        } if *owner == process.id => {
            // The active CFG is not published yet. Follow its declaration's
            // representation, not an unconstrained alias: even literal empty
            // strings use fixed storage rather than a runtime handle.
            // ponytail: linear declaration lookup; cache binding representation
            // if profiling identifies many string operations in huge CFGs.
            process
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .find_map(|instruction| match instruction {
                    ProcessInstruction::Declare {
                        local: declared,
                        initializer,
                        ..
                    } if declared == local => Some(initializer.map_or_else(
                        || {
                            process.locals.get(local.0 as usize).is_some_and(|local| {
                                runtime_handle_width(local.layout.as_ref()).is_some()
                            })
                        },
                        |initializer| {
                            process_value_is_runtime_string(initializer, process, context)
                        },
                    )),
                    _ => None,
                })
                .unwrap_or(false)
        }
        ProcessValueKind::Select {
            then_value,
            else_value,
            ..
        } => {
            process_value_is_runtime_string(*then_value, process, context)
                && process_value_is_runtime_string(*else_value, process, context)
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
    let mut args = args.as_slice();
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
        // `Trait::method(s)` is `s.method()`: the impl is the receiver
        // type's. Looked up as a plain path it named the trait's declaration
        // and the call did not lower (#159).
        ast::Expr::Path(path)
            if path.segments.len() >= 2
                && !args.is_empty()
                && context
                    .resolved
                    .resolved(path.segments[path.segments.len() - 2].span)
                    .and_then(|id| context.resolved.kind_of(id))
                    == Some(crate::resolve::DefKind::Trait) =>
        {
            let receiver = value_ref(&args[0], process, context);
            let owner = context
                .process_ir
                .values
                .get(receiver.0 as usize)
                .and_then(|value| value.ty.as_ref())
                .and_then(|ty| process_type_key(ty, context))?;
            let method = &path.segments[path.segments.len() - 1].text;
            args = &args[1..];
            let qualified = context
                .resolved
                .resolved(path.segments[path.segments.len() - 2].span)
                .and_then(|id| context.functions.trait_key(id))
                .and_then(|key| {
                    context
                        .functions
                        .get_associated(&format!("{owner}#{key}"), method)
                });
            (
                qualified.or_else(|| context.functions.get_associated(&owner, method))?,
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
    let arguments = lower_call_arguments(args, &parameter_types, receiver, process, context);
    let result = inline_process_function(
        function,
        ast::expr_span(expression),
        receiver,
        &arguments,
        process,
        context,
        return_type,
    );
    if result.is_none() {
        truncate_process_values(context, first_value);
    }
    // `-> Logic[]`: the result has the shape the body sets.
    if let Some(result) = result {
        if process_value_source_layout(result, context.process_ir).is_none() {
            if let Some(layout) = unsized_call_layout(
                function,
                ast::expr_span(expression),
                &arguments,
                process,
                context,
            ) {
                if context.process_ir.values[result.0 as usize].bit_width == layout.packed_width()
                    || context.process_ir.values[result.0 as usize]
                        .bit_width
                        .is_none()
                {
                    context.process_ir.values[result.0 as usize].bit_width = layout.packed_width();
                    context.process_ir.value_layouts[result.0 as usize] = Some(layout);
                }
            }
        }
    }
    // `y.negate().to_real()`: a method returning its receiver's type keeps
    // the receiver's format, as an operator does.
    // Its width too: `a.rem(m)` on a `signed[8]` is a `signed[8]`, so a
    // comparison on it reads `self'length` as 8 rather than the word's 64.
    if let (Some(result), Some(receiver)) = (result, receiver) {
        if returns_receiver_type(function, receiver, context) {
            let receiver_type = context.process_ir.values[receiver.0 as usize].ty.clone();
            let result = if matches!(receiver_type, Some(crate::types::Ty::Array { len, .. }) if len > 0)
            {
                narrow_to_type(result, receiver_type.as_ref(), context)
            } else {
                result
            };
            return Some(inherit_receiver_layout(result, receiver, context));
        }
    }
    result
}

/// Lower a call's arguments. A width-generic one (`rhs: signed`) takes the
/// width of a sized value of its family already in hand, the receiver's or
/// an earlier argument's: at its own minimum width, `u.saturating_sub(100)`
/// read `100` as a 7-bit signed, which is -28.
fn lower_call_arguments(
    args: &[ast::Expr],
    declared: &[Option<crate::types::Ty>],
    receiver: Option<ProcessValueId>,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Vec<ProcessValueId> {
    let mut sized = receiver
        .and_then(|receiver| process_value_type(receiver, context))
        .into_iter()
        .collect::<Vec<_>>();
    let mut arguments = Vec::with_capacity(args.len());
    for (argument, declared) in args.iter().zip(declared) {
        let ty = match declared {
            Some(family @ crate::types::Ty::Array { len: 0, .. }) => {
                let key = process_type_key(family, context);
                sized
                    .iter()
                    .find(|ty| {
                        matches!(ty, crate::types::Ty::Array { len, .. } if *len > 0)
                            && key.is_some()
                            && process_type_key(ty, context) == key
                    })
                    .cloned()
                    .or_else(|| declared.clone())
            }
            declared => declared.clone(),
        };
        let value = value_ref_with_type(argument, process, context, ty.as_ref());
        sized.extend(process_value_type(value, context));
        arguments.push(value);
    }
    arguments
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

/// A width-generic function (`fn f(v: unsigned) -> unsigned`) returns the
/// shape of its first argument of the return type, as a method returns its
/// receiver's: the declared type alone has no width.
fn same_type_argument(
    function: &ast::FnDecl,
    arguments: &[ProcessValueId],
    context: &LoweringContext<'_>,
) -> Option<ProcessValueId> {
    let key = |ty: &ast::Type| {
        declared_process_type(ty, context.resolved).and_then(|ty| process_type_key(&ty, context))
    };
    let declared = key(function.ret.as_ref()?)?;
    function
        .params
        .iter()
        .filter(|parameter| !parameter.is_self)
        .zip(arguments)
        .find(|(parameter, _)| parameter.ty.as_ref().and_then(key).as_ref() == Some(&declared))
        .map(|(_, argument)| *argument)
}

/// Inline one already-selected Siox function over already-lowered operands,
/// and, when the function is large or `#[inline(never)]`, record that the
/// result is also a call of one shared copy (`ProcessIr::calls`). The
/// expansion always stays: it defines the value, and a backend calls the
/// shared copy only where the two are interchangeable.
#[allow(clippy::too_many_arguments)]
fn inline_process_function(
    function: &ast::FnDecl,
    call: crate::diag::Span,
    receiver: Option<ProcessValueId>,
    arguments: &[ProcessValueId],
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    return_type: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    let first_value = context.process_ir.values.len();
    let result = expand_process_function(
        function,
        call,
        receiver,
        arguments,
        process,
        context,
        return_type,
    )?;
    // A recursive call cannot be expanded: the expansion left it a call.
    // Such a function is always shared, and its calls call the shared copy.
    let mut recursive = self_calls(
        function,
        first_value..context.process_ir.values.len(),
        context,
    );
    // `blowup((x, x))` in `blowup<T>`: a self-call with other argument types
    // is no call of this copy, and treated as one it lowered to a block
    // nothing could run (#175).
    let ty = |value: &ProcessValueId| context.process_ir.values[value.0 as usize].ty.clone();
    let width = |value: &ProcessValueId| context.process_ir.values[value.0 as usize].bit_width;
    if let Some((call_value, _)) = recursive.iter().find(|(_, called)| {
        called.iter().zip(arguments).any(|(a, b)| {
            matches!((ty(a), ty(b)), (Some(a), Some(b)) if a != b)
                || matches!((width(a), width(b)), (Some(a), Some(b)) if a != b)
                // An aggregate built for a scalar parameter.
                || matches!(
                    context.process_ir.values[a.0 as usize].kind,
                    ProcessValueKind::Construct { .. }
                ) && operand_source_layout(*b, process, context)
                    .is_some_and(|layout| matches!(layout.kind, LayoutKind::Scalar { .. }))
        })
    }) {
        context.recursive_calls.borrow_mut().push((
            function.name.text.clone(),
            context.process_ir.values[call_value.0 as usize].span,
            function.span,
            true,
        ));
        return None;
    }
    let share = !recursive.is_empty()
        || match function.inline {
            ast::Inline::Always | ast::Inline::Hint => false,
            ast::Inline::Never => true,
            ast::Inline::Auto => context.process_ir.values.len() - first_value > SHARE_ABOVE_VALUES,
        };
    let operands = receiver
        .into_iter()
        .chain(arguments.iter().copied())
        .collect::<Vec<_>>();
    // Only a value the expansion built can stand for the call.
    if !share
        || (result.0 as usize) < first_value
        || operands.iter().any(|operand| operand.0 >= result.0)
    {
        return Some(result);
    }
    let body = context.process_ir.values.len();
    let Some(shared) = shared_process_function(
        function,
        &operands,
        receiver.is_some(),
        process,
        context,
        return_type,
    ) else {
        return Some(result);
    };
    recursive.extend(self_calls(
        function,
        body..context.process_ir.values.len(),
        context,
    ));
    let width = |value: ProcessValueId| context.process_ir.values[value.0 as usize].bit_width;
    let callee = &context.process_ir.functions[shared.0 as usize];
    if width(callee.result) == width(result) {
        context.process_ir.calls.push(crate::ir::ProcessCall {
            value: result,
            function: shared,
            arguments: operands,
        });
        for (value, arguments) in recursive {
            context.process_ir.calls.push(crate::ir::ProcessCall {
                value,
                function: shared,
                arguments,
            });
            context.self_calls.borrow_mut().insert(value);
        }
    }
    Some(result)
}

/// The calls of `function` itself among the values in `range`: recursion,
/// which expanding `function` left as calls. Free functions only.
fn self_calls(
    function: &ast::FnDecl,
    range: std::ops::Range<usize>,
    context: &LoweringContext<'_>,
) -> Vec<(ProcessValueId, Vec<ProcessValueId>)> {
    let Some(definition) = context.resolved.declared(function.name.span) else {
        return Vec::new();
    };
    range
        .filter_map(|index| match &context.process_ir.values[index].kind {
            ProcessValueKind::Call {
                callee,
                type_arguments,
                arguments,
                bang: false,
            } if type_arguments.is_empty()
                && matches!(
                    context.process_ir.values[callee.0 as usize].kind,
                    ProcessValueKind::Definition(called) if called == definition
                ) =>
            {
                Some((ProcessValueId(index as u32), arguments.clone()))
            }
            _ => None,
        })
        .collect()
}

/// The shared copy of `function` for these operands' types, built once per
/// signature over `Parameter` leaves. `None` when the body cannot stand
/// alone: it reads state, has an effect, checks at run time, or depends on a
/// value of its caller.
fn shared_process_function(
    function: &ast::FnDecl,
    operands: &[ProcessValueId],
    has_receiver: bool,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    return_type: Option<&crate::types::Ty>,
) -> Option<crate::ir::ProcessFunctionId> {
    let layouts = operands
        .iter()
        .map(|operand| operand_source_layout(*operand, process, context))
        .collect::<Vec<_>>();
    // The signature is the operands' shapes, never where they were declared.
    let mut key = format!("{:?}->{:?}", function.span, return_type);
    if let Some(layout) = context
        .self_formats
        .last()
        .and_then(|value| process_value_source_layout(*value, context.process_ir))
        .filter(|layout| returns_self_format(function, layout))
    {
        // Equal bit widths do not imply equal target formats (or source
        // return labels). Kernel-number helpers still share across formats.
        key.push_str(&format!("|Self:{:?}", layout.kind));
    }
    for (operand, layout) in operands.iter().zip(&layouts) {
        let value = &context.process_ir.values[operand.0 as usize];
        let shape = layout.as_ref().map(|layout| &layout.kind);
        key.push_str(&format!(
            "|{:?}/{:?}/{:?}",
            value.ty, value.bit_width, shape
        ));
    }
    if let Some(known) = context.shared_functions.borrow().get(&key) {
        return *known;
    }
    let id = crate::ir::ProcessFunctionId(context.process_ir.functions.len() as u32);
    // A parameter has its type's full width, not its first argument's
    // natural one: a constant `float(0)` is one bit wide, and an expanded
    // body would have re-read it at the width each use needs. An argument
    // without such a type cannot be passed faithfully, so its signature is
    // not shared.
    let mut widths = Vec::with_capacity(operands.len());
    for (operand, layout) in operands.iter().zip(&layouts) {
        let value = &context.process_ir.values[operand.0 as usize];
        let natural = value.bit_width.unwrap_or(0);
        let width = match (&value.ty, layout.as_ref()) {
            (
                Some(
                    crate::types::Ty::Integer | crate::types::Ty::Natural | crate::types::Ty::Real,
                ),
                _,
            ) => Some(64),
            (_, Some(layout)) => layout.packed_width(),
            _ => None,
        }
        .filter(|width| *width >= natural && *width > 0);
        match width {
            Some(width) => widths.push(width),
            None => {
                context.shared_functions.borrow_mut().insert(key, None);
                return None;
            }
        }
    }
    let first_value = context.process_ir.values.len();
    let mut parameters = Vec::with_capacity(operands.len());
    for (index, operand) in operands.iter().enumerate() {
        let value = &context.process_ir.values[operand.0 as usize];
        let (span, ty, width) = (value.span, value.ty.clone(), Some(widths[index]));
        let layout = layouts[index].clone();
        let parameter = push_value(
            span,
            ty,
            width,
            ProcessValueKind::Parameter {
                function: id,
                index: index as u32,
            },
            context,
        );
        context.process_ir.value_layouts[parameter.0 as usize] = layout;
        parameters.push(parameter);
    }
    let (receiver, arguments) = if has_receiver {
        (Some(parameters[0]), &parameters[1..])
    } else {
        (None, &parameters[..])
    };
    let result = expand_process_function(
        function,
        function.span,
        receiver,
        arguments,
        process,
        context,
        return_type,
    )
    .filter(|result| standalone_body(function, first_value, *result, context));
    let shared = match result {
        Some(result) => {
            context
                .process_ir
                .functions
                .push(crate::ir::ProcessFunction {
                    id,
                    name: function.name.text.clone(),
                    span: function.span,
                    parameters,
                    result,
                });
            Some(id)
        }
        None => {
            truncate_process_values(context, first_value);
            None
        }
    };
    context.shared_functions.borrow_mut().insert(key, shared);
    shared
}

/// An operand's source layout, including a local of the process being
/// lowered, which is not in the arena's finished processes yet.
fn operand_source_layout(
    operand: ProcessValueId,
    process: &ProcessCfg,
    context: &LoweringContext<'_>,
) -> Option<crate::ir::SourceLayout> {
    if let Some(layout) = process_value_source_layout(operand, context.process_ir) {
        return Some(layout.clone());
    }
    match &context.process_ir.values.get(operand.0 as usize)?.kind {
        ProcessValueKind::Local {
            process: owner,
            local,
        } if *owner == process.id => process.locals.get(local.0 as usize)?.layout.clone(),
        _ => None,
    }
}

/// A literal argument (`hi(0xA5)`) has a type but no layout of its own, so
/// the body could not slice or index it as the parameter (`x[7..4]`); it
/// takes the parameter's declared layout, written ranges included. A place
/// or computed argument keeps its own.
fn give_literal_parameter_layout(
    parameter: &ast::FnParam,
    argument: ProcessValueId,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) {
    let literal = context
        .process_ir
        .values
        .get(argument.0 as usize)
        .is_some_and(|value| {
            matches!(
                value.kind,
                ProcessValueKind::Number(_) | ProcessValueKind::BitString { .. }
            )
        });
    if !literal || process_value_source_layout(argument, context.process_ir).is_some() {
        return;
    }
    let layout = process_binding_layout(
        parameter.ty.as_ref(),
        parameter.span,
        argument,
        process,
        context,
    );
    if let (Some(layout), Some(slot)) = (
        layout,
        context
            .process_ir
            .value_layouts
            .get_mut(argument.0 as usize),
    ) {
        *slot = Some(layout);
    }
}

/// Inspection belongs to the source declaration, not the caller's narrower
/// storage or the executable consumer format. Generic/unsized declarations
/// retain their instantiated operand layout without changing value lowering.
fn process_binding_layout(
    declared: Option<&ast::Type>,
    span: crate::diag::Span,
    value: ProcessValueId,
    process: &ProcessCfg,
    context: &LoweringContext<'_>,
) -> Option<SourceLayout> {
    declared
        .and_then(|ty| process_declared_type(ty, context))
        .filter(|ty| is_concrete_type(ty, context))
        .and_then(|ty| {
            let mut layout = process_layout_for_type(&ty, span, context)?;
            apply_process_declared_ranges(
                &mut layout,
                declared?,
                context,
                &mut std::collections::HashSet::new(),
            )?;
            (layout.packed_width()? > 0).then_some(layout)
        })
        .or_else(|| operand_source_layout(value, process, context))
        .or_else(|| {
            // Constructors retain their checked target type even without a
            // stored/projection layout. Generic inspection must use that
            // instantiated type, not the operand's raw integer bit pattern.
            let ty = context
                .process_ir
                .values
                .get(value.0 as usize)?
                .ty
                .as_ref()?;
            let layout = process_layout_for_type(ty, span, context)?;
            (layout.packed_width()? > 0).then_some(layout)
        })
}

/// Whether the values built from `first` on (one function body over its
/// parameters, with any bodies it shares in turn) compute `result` from
/// those values alone, without state, effects or run-time checks.
fn standalone_body(
    function: &ast::FnDecl,
    first: usize,
    result: ProcessValueId,
    context: &LoweringContext<'_>,
) -> bool {
    // Its own recursive calls become calls of the shared copy being built.
    let recursive = self_calls(function, first..context.process_ir.values.len(), context)
        .into_iter()
        .map(|(value, _)| value.0 as usize)
        .collect::<std::collections::HashSet<_>>();
    (result.0 as usize) >= first
        && context.process_ir.values[first..]
            .iter()
            .enumerate()
            .all(|(offset, value)| {
                recursive.contains(&(first + offset))
                    || !matches!(
                        value.kind,
                        ProcessValueKind::Signal { .. }
                            | ProcessValueKind::Storage(_)
                            | ProcessValueKind::StorageState { .. }
                            | ProcessValueKind::Local { .. }
                            | ProcessValueKind::ForeignCall { .. }
                            | ProcessValueKind::HostCall { .. }
                            | ProcessValueKind::Call { .. }
                            | ProcessValueKind::CheckedIndex { .. }
                            | ProcessValueKind::Invalid
                    ) && !crate::ir::process::any_process_value_dependency(
                        &value.kind,
                        |dependency| (dependency.0 as usize) < first,
                    )
            })
}

/// Expand one already-selected Siox function over already-lowered operands.
/// Calls and operators share this implementation so receiver binding,
/// overload bodies, recursion recovery, and result typing cannot drift.
#[allow(clippy::too_many_arguments)]
fn expand_process_function(
    function: &ast::FnDecl,
    call: crate::diag::Span,
    receiver: Option<ProcessValueId>,
    arguments: &[ProcessValueId],
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    return_type: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    let body = function.body.as_ref()?;
    let first_expanded = context.process_ir.values.len();
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
    let generic_argument = generic_return_argument(function, arguments);
    let generic_return =
        generic_argument.and_then(|argument| process_value_type(argument, context));

    let mut bindings = std::collections::HashMap::new();
    for (parameter, argument) in parameters.into_iter().zip(arguments.iter().copied()) {
        let name = parameter.name.as_ref()?;
        let definition = context.resolved.declared(name.span)?;
        give_literal_parameter_layout(parameter, argument, process, context);
        bindings.insert(definition, argument);
    }
    if !context.inline_functions.insert(function.span) {
        return None;
    }

    // The body computes in its declared return type when that is a kernel
    // number (`-> integer`): the caller's type narrows at the boundary.
    // Evaluated in the caller's `ufixed<8, 4>`, `return n / d;` divided
    // 8-bit operands. A family (`-> ufixed`), an abstract parameter or no
    // declaration leaves it to the caller's type, then the arguments'.
    let declared = function
        .ret
        .as_ref()
        .and_then(|ty| declared_process_type(ty, context.resolved))
        .filter(|ty| is_concrete_type(ty, context));
    let caller_type = return_type
        .filter(|ty| is_concrete_type(ty, context))
        .cloned();
    let return_type = match declared {
        Some(
            declared @ (crate::types::Ty::Integer
            | crate::types::Ty::Natural
            | crate::types::Ty::Real),
        ) => Some(declared),
        declared => return_type
            .filter(|ty| is_concrete_type(ty, context))
            .cloned()
            .or(generic_return)
            .or(declared),
    };
    context.value_bindings.push(bindings);
    context.inline_self_values.push(receiver);
    context.inline_return_types.push(return_type);
    context.inline_value_locals.push(Vec::new());
    context.inline_value_branches.push(Vec::new());
    let statements = body.stmts.iter().collect::<Vec<_>>();
    let result = inline_value_statements(&statements, process, context);
    let branches = context.inline_value_branches.pop().unwrap();
    let mut locals = context.inline_value_locals.pop().unwrap();
    for local in &mut locals {
        // Branches fill their smaller scopes before returning, as CFG locals do.
        local.scope.get_or_insert(body.span);
    }
    context.inline_return_types.pop();
    context.inline_self_values.pop();
    context.value_bindings.pop();
    context.inline_functions.remove(&function.span);
    // ...and that argument's format: `abs(x)` on a `ufixed<8, 4>` is one.
    let (result, result_span) = result?;
    if drops_effect(first_expanded, result, context) {
        return None;
    }
    let result = match (Some(result), generic_argument) {
        (Some(result), Some(argument)) => Some(inherit_receiver_layout(result, argument, context)),
        (result, _) => result,
    }?;
    // Keep the declared numeric evaluation and caller-family boundary from
    // fixed-point lowering; source provenance wraps that same executable value.
    let result = match caller_type {
        Some(
            caller @ crate::types::Ty::Array {
                family: Some(_), ..
            },
        ) => retype(result, &caller, context),
        _ => result,
    };
    let parameters = function
        .params
        .iter()
        .zip(receiver.into_iter().chain(arguments.iter().copied()))
        .map(|(parameter, value)| crate::ir::ProcessValueBinding {
            name: parameter
                .name
                .as_ref()
                .map_or_else(|| "self".to_string(), |name| name.text.clone()),
            span: parameter.span,
            scope: None,
            layout: process_binding_layout(
                parameter.ty.as_ref(),
                parameter.span,
                value,
                process,
                context,
            ),
            value: Some(value),
            fields: Vec::new(),
        })
        .collect();
    let name = context
        .resolved
        .declared(function.name.span)
        .and_then(|definition| context.resolved.qualified_name(definition))
        .unwrap_or_else(|| function.name.text.clone());
    let node = &context.process_ir.values[result.0 as usize];
    let ty = node.ty.clone();
    let width = node.bit_width;
    let layout = process_value_source_layout(result, context.process_ir).cloned();
    let id = push_value(
        call,
        ty,
        width,
        ProcessValueKind::InlineCall {
            operand: result,
            source: Box::new(crate::ir::ProcessValueInline {
                name,
                function: function.span,
                call,
                result_span,
                result_layout: process_binding_layout(
                    function.ret.as_ref(),
                    function.span,
                    result,
                    process,
                    context,
                ),
                aggregate: None,
                branches,
                parameters,
                locals,
            }),
        },
        context,
    );
    context.process_ir.value_layouts[id.0 as usize] = layout;
    Some(id)
}

/// `value` as a `ty`: narrowed to a sized array's width, otherwise relabelled
/// with the type at its own width. Unchanged when it already has that type.
fn retype(
    value: ProcessValueId,
    ty: &crate::types::Ty,
    context: &mut LoweringContext<'_>,
) -> ProcessValueId {
    let current = context.process_ir.values[value.0 as usize].ty.clone();
    if current.as_ref() == Some(ty) {
        return value;
    }
    let key = |ty: &crate::types::Ty| process_type_key(ty, context);
    if current.as_ref().and_then(key).is_some() && current.as_ref().and_then(key) == key(ty) {
        return value;
    }
    if matches!(ty, crate::types::Ty::Array { len, .. } if *len > 0) {
        return narrow_to_type(value, Some(ty), context);
    }
    let process_value = &context.process_ir.values[value.0 as usize];
    let (span, width) = (process_value.span, process_value.bit_width);
    let retyped = push_value(
        span,
        Some(ty.clone()),
        width,
        ProcessValueKind::RawResize { operand: value },
        context,
    );
    if let Some(layout) = process_value_source_layout(value, context.process_ir).cloned() {
        context.process_ir.value_layouts[retyped.0 as usize] = Some(layout);
    }
    retyped
}

/// The caller operand that determines a generic function's return type and
/// layout. Value expressions and CFG inlines share the same inference rule.
fn generic_return_argument(
    function: &ast::FnDecl,
    arguments: &[ProcessValueId],
) -> Option<ProcessValueId> {
    let generic_name = |ty: &ast::Type| match ty {
        ast::Type::Path(path) if path.segments.len() == 1 => {
            let name = &path.segments[0].text;
            (function.generics.params.iter())
                .any(|param| &param.name.text == name)
                .then(|| name.clone())
        }
        _ => None,
    };
    function
        .ret
        .as_ref()
        .and_then(generic_name)
        .and_then(|name| {
            function
                .params
                .iter()
                .filter(|parameter| !parameter.is_self)
                .zip(arguments)
                .find(|(parameter, _)| {
                    parameter.ty.as_ref().and_then(generic_name).as_deref() == Some(name.as_str())
                })
                .map(|(_, argument)| *argument)
        })
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
        ..
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
        (*range, element.packed_width()?)
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
            ..
        },
        crate::types::Ty::Array {
            elem: right_element,
            len: right_len,
            family: None,
            ..
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
                span,
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
            inline_process_function(
                function,
                span,
                Some(operand),
                &[],
                process,
                context,
                Some(elem),
            )
        })
        .collect::<Option<Vec<_>>>()?;
    let result_type = return_type
        .filter(|ty| matches!(ty, crate::types::Ty::Array { family: None, .. }))
        .cloned()
        .unwrap_or_else(|| operand_type.clone());
    push_process_array(span, result_type, elements, context)
}

/// Inline a binary operator's selected source trait-method body. Symbols remain
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
    let right_type = operand_type(rhs, process, context);
    // A literal on the left takes the right operand's type, as one on the
    // right takes the left's: `255 xor a`, and `102 / (c and c)`, which
    // otherwise divided as unsigned kernel words.
    let literal_left = matches!(lhs, ast::Expr::Int { .. })
        && matches!(
            right_type,
            Some(crate::types::Ty::Array {
                family: Some(_),
                ..
            })
        );
    // A character literal beside an enum is that enum's variant: `u and
    // '1'` found no `And<Char>` for `Logic` and fell back to raw
    // discriminant arithmetic, `'U' and '1'` giving `'0'` (#190).
    let named = |ty: &Option<crate::types::Ty>| matches!(ty, Some(crate::types::Ty::Named(_)));
    let char_left = matches!(lhs, ast::Expr::CharLit { .. }) && named(&right_type);
    let left_type = match operand_type(lhs, process, context) {
        Some(crate::types::Ty::Integer) | None if literal_left => right_type.clone()?,
        _ if char_left => right_type.clone()?,
        left => left?,
    };
    let right_type = if matches!(rhs, ast::Expr::CharLit { .. }) && named(&Some(left_type.clone()))
    {
        Some(left_type.clone())
    } else {
        right_type
    };
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
    // A literal receiver (`255 xor a`) has no declaration to give it a
    // layout, and the operator's body reads its elements.
    if literal_left && context.process_ir.value_layouts[left.0 as usize].is_none() {
        let layout = process_layout_for_type(&left_type, ast::expr_span(lhs), context);
        context.process_ir.value_layouts[left.0 as usize] = layout;
    }
    // A width-generic operand (`x: signed` in a function) has its argument's
    // width. A literal on the other side takes that one: typed as the bare
    // family, `x - 1` read `1` as a one-bit signed, which is -1.
    let left_type = match (&left_type, process_value_type(left, context)) {
        (
            crate::types::Ty::Array { len: 0, .. },
            Some(concrete @ crate::types::Ty::Array { len, .. }),
        ) if len > 0 => concrete,
        _ => left_type,
    };
    // Operator selection lets a kernel integer adopt the receiver family.
    // Bind the value under that same contextual type: source implementations
    // legitimately inspect `rhs'length`, and keeping a literal/expression as
    // an unbounded integer here would make the selected `signed[N]` contract
    // disappear between overload resolution and body inlining.
    // Not where the selected impl declares the operand `integer`
    // (`impl Add<integer, Complex> for Complex`): `z + 2` bound `2` as a
    // `Complex`.
    let declared_integer = function
        .params
        .iter()
        .find(|parameter| !parameter.is_self)
        .and_then(|parameter| parameter.ty.as_ref())
        .and_then(crate::ir::lower_helpers::type_head_name)
        == Some("integer");
    let right_context = if matches!(right_type, Some(crate::types::Ty::Integer))
        && !matches!(left_type, crate::types::Ty::Integer)
        && !declared_integer
    {
        Some(&left_type)
    } else {
        right_type.as_ref().or(Some(&left_type))
    };
    let right = value_ref_with_type(rhs, process, context, right_context);
    // An operator declared to return its own type (`-> float`, `-> Self`)
    // returns the left operand's, also where the checker recorded none (an
    // argument of a conversion): `float`'s bodies compute in kernel words,
    // and that word's type must not stand in for the format.
    let own_type = function
        .ret
        .as_ref()
        .and_then(crate::ir::lower_helpers::type_head_name)
        .is_some_and(|head| head == "Self" || owner.rsplit("::").next() == Some(head));
    let return_type = return_type.or(own_type.then_some(&left_type));
    let result = inline_process_function(
        function,
        ast::expr_span(lhs),
        Some(left),
        &[right],
        process,
        context,
        return_type,
    );
    if result.is_none() {
        truncate_process_values(context, first_value);
    }
    result.map(|result| inherit_receiver_layout(result, left, context))
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
        ast::expr_span(lhs),
        Some(left),
        &[right],
        process,
        context,
        bool_type(context.resolved).as_ref(),
    );
    if result.is_none() {
        truncate_process_values(context, first_value);
    }
    // A numeric family compared against a metavalue is false (`!=` true),
    // as hardware marks it: `"01X1" == "0101"` compared value planes only.
    // Not inside an inlined body: an operator impl compares kernel words
    // (`signed`'s sign test), whose wrapped form had no lowering.
    // ponytail: user functions don't get the rule either; marking only
    // operator bodies needs to know which inline frame is an impl's.
    // Only the design's own comparisons, in the process's file: std's
    // operator bodies (a sign test, a float's fields) compare kernel words.
    let own = ast::expr_span(lhs).file == process.span.file;
    // ponytail: an operand still holding a call (a function result) keeps
    // the plain comparison; the CFG expands that call once, for the inner
    // comparison, and a second reference from the rule lost its layout.
    let holds_call = |root: ProcessValueId| {
        let mut seen = std::collections::HashSet::new();
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            let Some(node) = context.process_ir.values.get(id.0 as usize) else {
                continue;
            };
            if matches!(node.kind, ProcessValueKind::Call { .. }) {
                return true;
            }
            pending.extend(crate::ir::process_value_dependencies(&node.kind));
        }
        false
    };
    let own = own && !holds_call(left) && !holds_call(right);
    let numeric_family = own
        && matches!(left_type, crate::types::Ty::Array { family: Some(family), .. }
        if context.design.array_element_of_family.contains_key(family)
            || family.rsplit("::").next().is_some_and(|leaf| {
                context.design.array_element_of_family.contains_key(leaf)
            }));
    // The rule reads each operand's companion through its layout; a computed
    // one (`(a or b) != a`) has none of its own.
    if numeric_family {
        for operand in [left, right] {
            // Only the narrowing copy: the operand values themselves are the
            // comparison's receiver and argument too.
            let narrowed = matches!(
                context.process_ir.values[operand.0 as usize].kind,
                ProcessValueKind::RawResize { .. }
            ) && arena_constant_integer(operand, &context.process_ir.values)
                .is_none();
            if narrowed && context.process_ir.value_layouts[operand.0 as usize].is_none() {
                let span = context.process_ir.values[operand.0 as usize].span;
                context.process_ir.value_layouts[operand.0 as usize] =
                    process_layout_for_type(left_type, span, context);
            }
        }
    }
    match result {
        Some(inner) if numeric_family => Some(push_value(
            ast::expr_span(lhs).to(ast::expr_span(rhs)),
            bool_type(context.resolved),
            Some(1),
            ProcessValueKind::MetaCompare {
                not_equal: matches!(operator, ast::BinOp::Ne),
                operands: vec![left, right],
                inner,
            },
            context,
        )),
        other => other,
    }
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
    // The source's layout carries over at the declared width only: an
    // `integer` local returned from `-> float` is a word of the float.
    if let Some(layout) = process_value_source_layout(value, context.process_ir)
        .filter(|layout| layout.packed_width().is_none_or(|width| width == *len))
        .cloned()
    {
        context.process_ir.value_layouts[narrowed.0 as usize] = Some(layout);
    }
    narrowed
}

/// An operator that returns its receiver's own type keeps the receiver's
/// format: `a + b` on `ufixed<8, 4>` is a `ufixed<8, 4>`, so a body applied
/// to that result (`(a + b) * c`, reading `self'low`) still finds its index
/// range. Without a layout the result was a bare word with no bounds.
fn inherit_receiver_layout(
    result: ProcessValueId,
    receiver: ProcessValueId,
    context: &mut LoweringContext<'_>,
) -> ProcessValueId {
    if process_value_source_layout(result, context.process_ir).is_some() {
        return result;
    }
    // The same nominal type, sized or not: a product typed `float` takes the
    // format of its `float<32, 23>` operand.
    let same_type = {
        let key = |id: ProcessValueId| {
            let ty = context.process_ir.values.get(id.0 as usize)?.ty.as_ref()?;
            process_type_key(ty, context)
        };
        key(result).is_some() && key(result) == key(receiver)
    };
    let Some(layout) = process_value_source_layout(receiver, context.process_ir).cloned() else {
        return result;
    };
    if same_type {
        // An inlined body can calculate in a wider temporary representation.
        // The returned receiver-format value wraps at its declared width;
        // its intermediate does not acquire that narrower layout by mutation.
        let result = match layout.packed_width() {
            Some(width)
                if context.process_ir.values[result.0 as usize].bit_width != Some(width) =>
            {
                let value = &context.process_ir.values[result.0 as usize];
                push_value(
                    value.span,
                    value.ty.clone(),
                    Some(width),
                    ProcessValueKind::RawResize { operand: result },
                    context,
                )
            }
            _ => result,
        };
        if let Some(slot) = context.process_ir.value_layouts.get_mut(result.0 as usize) {
            *slot = Some(layout);
        }
        return result;
    }
    result
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
    let result = inline_process_function(
        function,
        ast::expr_span(rhs),
        Some(operand),
        &[],
        process,
        context,
        return_type,
    );
    if result.is_none() {
        truncate_process_values(context, first_value);
    }
    // `(-y).to_real()` reads the operand's format, as a binary result does.
    result.map(|result| inherit_receiver_layout(result, operand, context))
}

/// Evaluate a pure function statement sequence symbolically. `return`, local
/// aliases, and branching cover the expression-shaped Siox functions shared
/// by std and hardware lowering; other statements deliberately leave the call
/// explicit and fail closed in native Process lowering.
fn inline_value_statements(
    statements: &[&Stmt],
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<(ProcessValueId, crate::diag::Span)> {
    let (statement, rest) = statements.split_first()?;
    match *statement {
        Stmt::Return {
            value: Some(value), ..
        } => {
            let return_type = context.inline_return_types.last().cloned().flatten();
            Some((
                value_ref_with_type(value, process, context, return_type.as_ref()),
                ast::expr_span(value),
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
            let layout = process_binding_layout(
                declaration.ty.as_ref(),
                declaration.span,
                value,
                process,
                context,
            );
            if let Some(locals) = context.inline_value_locals.last_mut() {
                locals.push(crate::ir::ProcessValueBinding {
                    name: declaration.name.text.clone(),
                    span: declaration.span,
                    scope: None,
                    layout,
                    value: Some(value),
                    fields: Vec::new(),
                });
            }
            inline_value_statements(rest, process, context)
        }
        Stmt::If(statement) => inline_value_if(statement, rest, process, context),
        Stmt::Match(statement) => inline_value_match(statement, rest, process, context),
        _ => None,
    }
}

/// Whether an expansion from `first` on built a foreign or host call that its
/// `result` does not read. A value form keeps only what the result reads, so
/// such an effect (`let printed = putchar(65);`, never used) would vanish;
/// the statement order of a CFG expansion keeps it.
fn drops_effect(first: usize, result: ProcessValueId, context: &LoweringContext<'_>) -> bool {
    let values = &context.process_ir.values;
    let effects = (first..values.len())
        .filter(|index| {
            matches!(
                values[*index].kind,
                ProcessValueKind::ForeignCall { .. } | ProcessValueKind::HostCall { .. }
            )
        })
        .collect::<Vec<_>>();
    if effects.is_empty() {
        return false;
    }
    let mut seen = std::collections::HashSet::new();
    let mut pending = vec![result];
    while let Some(id) = pending.pop() {
        if (id.0 as usize) >= first && seen.insert(id.0 as usize) {
            crate::ir::process::for_each_process_value_dependency(
                &values[id.0 as usize].kind,
                |dependency| pending.push(dependency),
            );
        }
    }
    effects.iter().any(|effect| !seen.contains(effect))
}

/// A shape attribute (`'length`, `'high`, ..) of a value whose layout is
/// known, which is a constant.
fn shape_attribute(
    base: ProcessValueId,
    attribute: &str,
    process: &ProcessCfg,
    context: &LoweringContext<'_>,
) -> Option<i64> {
    let range = operand_source_layout(base, process, context)?.index_range()?;
    match attribute {
        "length" => i64::try_from(range.len()?).ok(),
        "left" => Some(range.left),
        "right" => Some(range.right),
        "high" => Some(range.left.max(range.right)),
        "low" => Some(range.left.min(range.right)),
        _ => None,
    }
}

/// The integer a value always has, when lowering can tell. A 64-bit number
/// is a kernel integer, so its top bit is the sign.
fn constant_integer(value: ProcessValueId, context: &LoweringContext<'_>) -> Option<i64> {
    let folded = crate::ir::arena_constant_integer(value, &context.process_ir.values)?;
    if context.process_ir.values.get(value.0 as usize)?.bit_width == Some(64) {
        return Some(folded as u64 as i64);
    }
    i64::try_from(folded).ok()
}

/// Inline one branch in a fresh lexical binding scope, appending the source
/// continuation so a branch without an early return falls through normally.
/// The sequence borrows both: copying the statements here re-copied the whole
/// rest of a function at every nested `if`.
fn inline_value_branch(
    branch: &[Stmt],
    extent: Option<crate::diag::Span>,
    continuation: &[&Stmt],
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<(ProcessValueId, crate::diag::Span)> {
    let first_local = context.inline_value_locals.last()?.len();
    let mut statements = Vec::with_capacity(branch.len() + continuation.len());
    statements.extend(branch);
    statements.extend_from_slice(continuation);
    context
        .value_bindings
        .push(std::collections::HashMap::new());
    let result = inline_value_statements(&statements, process, context);
    context.value_bindings.pop();
    if let Some(extent) = extent {
        for local in &mut context.inline_value_locals.last_mut()?[first_local..] {
            // The appended continuation belongs to its original enclosing block.
            if local.scope.is_none()
                && local.span.file == extent.file
                && local.span.start >= extent.start
                && local.span.end <= extent.end
            {
                local.scope = Some(extent);
            }
        }
    }
    result
}

/// Turn a function-body `if` into a dependency-ordered Process selection.
fn inline_value_if(
    statement: &ast::IfStmt,
    continuation: &[&Stmt],
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<(ProcessValueId, crate::diag::Span)> {
    let condition = value_ref(&statement.cond, process, context);
    let then_value = inline_value_branch(
        &statement.then.stmts,
        Some(statement.then.span),
        continuation,
        process,
        context,
    )?;
    let else_value = match statement.else_.as_deref() {
        Some(ElseBranch::Block(block)) => inline_value_branch(
            &block.stmts,
            Some(block.span),
            continuation,
            process,
            context,
        )?,
        // `else if` is the branch `{ if … }`: inline it in its own scope.
        Some(ElseBranch::If(inner)) => {
            context
                .value_bindings
                .push(std::collections::HashMap::new());
            let value = inline_value_if(inner, continuation, process, context);
            context.value_bindings.pop();
            value?
        }
        None => inline_value_branch(&[], None, continuation, process, context)?,
    };
    inline_select_value(statement.span, condition, then_value, else_value, context)
}

/// Turn a function-body `match` into first-match-priority selections. Pattern
/// decoding happens while enum identities and typed character literals are
/// still available; the resulting Process graph contains only executable
/// comparisons and selects.
fn inline_value_match(
    statement: &ast::MatchStmt,
    continuation: &[&Stmt],
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<(ProcessValueId, crate::diag::Span)> {
    let scrutinee = value_ref(&statement.scrutinee, process, context);
    let mut result = inline_value_branch(&[], None, continuation, process, context);
    for arm in statement.arms.iter().rev() {
        let value = inline_value_branch(
            &arm.body.stmts,
            Some(arm.body.span),
            continuation,
            process,
            context,
        )?;
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
        ast::Pattern::BitPattern { .. }
        | ast::Pattern::Range { .. }
        | ast::Pattern::Bounds { .. }
        | ast::Pattern::Tuple { .. } => return None,
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
    (then_value, then_span): (ProcessValueId, crate::diag::Span),
    (else_value, else_span): (ProcessValueId, crate::diag::Span),
    context: &mut LoweringContext<'_>,
) -> Option<(ProcessValueId, crate::diag::Span)> {
    let then_node = context.process_ir.values.get(then_value.0 as usize)?;
    let else_node = context.process_ir.values.get(else_value.0 as usize)?;
    let ty = then_node.ty.clone().or_else(|| else_node.ty.clone());
    let width = match (then_node.bit_width, else_node.bit_width) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    };
    let shared_layout = process_value_source_layout(then_value, context.process_ir)
        .zip(process_value_source_layout(else_value, context.process_ir))
        .and_then(|(left, right)| process_layout_same_shape(left, right).then(|| left.clone()));
    let selection = push_value(
        span,
        ty,
        width,
        ProcessValueKind::Select {
            condition,
            then_value,
            else_value,
        },
        context,
    );
    if let Some(layout) = shared_layout {
        if let Some(slot) = context
            .process_ir
            .value_layouts
            .get_mut(selection.0 as usize)
        {
            *slot = Some(layout);
        }
    }
    if let Some(branches) = context.inline_value_branches.last_mut() {
        branches.push((selection, [then_span, else_span]));
    }
    Some((selection, span))
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
    let crate::types::Ty::Array {
        elem, len, family, ..
    } = ty?
    else {
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
                '0' => 0,
                '1' => 1,
                // Anything else, the weak `'L'`/`'H'` too, is a value of its
                // own: the string path keeps its discriminant in the
                // companion (`"H01L"` read back as `"1010"`).
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
/// `5i`: a suffix whose impl builds a struct (`impl Suffix<"i", real> for
/// Complex`) inlines that impl's `suffix`, as hardware does. A number-valued
/// suffix (`10ns`) is folded as a constant instead.
fn struct_suffix_literal(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<crate::ir::ProcessValueId> {
    let ast::Expr::SuffixLit { text, suffix, span } = expression else {
        return None;
    };
    if normalized_suffix(text, &suffix.text, context).is_some() {
        return None;
    }
    let ty @ crate::types::Ty::Named(definition) = context.typed.expr_type(*span).cloned()? else {
        return None;
    };
    // `time` literals fold to a constant; one left over overflowed, which
    // validation reports, where an inlined body wrapped it.
    if context.resolved.kind_of(definition) != Some(crate::resolve::DefKind::Struct)
        || context.resolved.lang_of(definition) == Some("time")
    {
        return None;
    }
    // This literal's impl, by its symbol: any `suffix` of the type picked
    // `ms`'s for an `ns`.
    let leaf = context.resolved.def(definition)?.name.clone();
    let function = context
        .suffixes
        .get(&suffix.text)?
        .iter()
        .find(|entry| entry.target == leaf)?
        .function
        .clone();
    let function = &function;
    let parameter = function
        .params
        .iter()
        .find(|parameter| !parameter.is_self)?
        .ty
        .as_ref()
        .and_then(crate::ir::lower_helpers::type_head_name)?;
    let (number, number_type, width) = match parameter {
        "real" => (
            ProcessNumber::Real(real_literal(text)?.to_bits()),
            crate::types::Ty::Real,
            64,
        ),
        "integer" => (parse_number(text, None), crate::types::Ty::Integer, 64),
        _ => return None,
    };
    let argument = push_value(
        *span,
        Some(number_type),
        Some(width),
        ProcessValueKind::Number(number),
        context,
    );
    inline_process_function(
        function,
        *span,
        None,
        &[argument],
        process,
        context,
        Some(&ty),
    )
}

fn value_ref_with_type(
    expression: &ast::Expr,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    contextual_type: Option<&crate::types::Ty>,
) -> crate::ir::ProcessValueId {
    if let Some(value) = process_converted_value(expression, process, context) {
        return value;
    }
    if let Some(value) = struct_suffix_literal(expression, process, context) {
        return value;
    }
    // Arithmetic on literals alone is exact in the vector it fills, as a
    // literal is: `0 - 17262465143367426459` into `signed[65]` wrapped in
    // the 64-bit kernel (also when promoted there) and came out positive.
    if !matches!(expression, ast::Expr::Int { .. }) {
        let span = ast::expr_span(expression);
        let target = context
            .typed
            .promotion(span)
            .or(contextual_type)
            .or_else(|| context.typed.expr_type(span))
            .cloned();
        if let (
            Some(
                ty @ crate::types::Ty::Array {
                    family: Some(_),
                    len,
                    ..
                },
            ),
            Some(folded),
        ) = (target, literal_arithmetic(expression))
        {
            if (1..=128).contains(&len) {
                let pattern = if len == 128 {
                    folded as u128
                } else {
                    (folded as u128) & ((1u128 << len) - 1)
                };
                let words = vec![pattern as u64, (pattern >> 64) as u64];
                let kind = ProcessValueKind::Number(ProcessNumber::Integer(words));
                return push_value(span, Some(ty), Some(len), kind, context);
            }
        }
    }
    if let Some(value) = process_promoted_operand(expression, process, context) {
        return value;
    }
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
    // `regs.index(2)` spelled out selects its `Index` overload as `regs[2]`
    // does: by name alone, the last declared `index` was taken.
    if let ast::Expr::Call { callee, args, .. } = expression {
        if let (ast::Expr::Field { base, field, .. }, [argument]) =
            (callee.as_ref(), args.as_slice())
        {
            if field.text == "index" {
                if let Some(value) =
                    custom_index_value(expression, base, argument, process, context)
                {
                    return value;
                }
            }
        }
    }
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
        // A family's `From` impl for the argument's type comes before the
        // kernel's raw resize (spec 3.17): `signed[16](s)` sign-extends.
        if let Some(value) = lower_process_family_from(expression, process, context, ty.as_ref()) {
            return value;
        }
        if let Some(value) = lower_process_kernel_conversion(expression, process, context) {
            return value;
        }
        if let Some(value) =
            lower_process_named_conversion(expression, process, context, ty.as_ref())
        {
            return value;
        }
        if let Some(value) = lower_process_qualified_call(expression, process, context, ty.as_ref())
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
                lower_process_real_promotion(op, lhs, rhs, span, process, context, ty.as_ref())
            {
                return value;
            }
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
                let is_self_type = matches!(base.as_ref(), ast::Expr::Path(path)
                    if path.segments.len() == 1 && path.segments[0].text == "Self");
                let base = match context.self_formats.last().copied() {
                    Some(format) if is_self_type => format,
                    _ => value_ref(base, process, context),
                };
                if attr.text == "length" && process_value_is_runtime_string(base, process, context)
                {
                    ProcessValueKind::HostCall {
                        operation: ProcessHostValueOp::StringLength,
                        arguments: vec![base],
                    }
                } else if let Some(number) = shape_attribute(base, &attr.text, process, context) {
                    return push_value(
                        span,
                        Some(crate::types::Ty::Integer),
                        Some(64),
                        ProcessValueKind::Number(ProcessNumber::Integer(vec![number as u64])),
                        context,
                    );
                } else {
                    ProcessValueKind::Attribute {
                        base,
                        attribute: attr.text.clone(),
                    }
                }
            }
        }
        // A slice of an element-wise array (`a[0..3]` into a `Bit[4]`): its
        // elements as written, a concatenation of one. It did not lower.
        ast::Expr::Index { base, index, .. }
            // Sized only, the array and its slice: a runtime `T[]` is sliced
            // below, at run time.
            if matches!(
                index.as_ref(),
                ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
            ) && (matches!(
                ty.as_ref(),
                Some(crate::types::Ty::Array { family: None, len, .. }) if *len > 0
            ) || matches!(
                // An argument for a `T[]` parameter has the slice's own
                // sized type, not the parameter's unsized one (#237).
                (ty.as_ref(), context.typed.expr_type(span)),
                (
                    Some(crate::types::Ty::Array { family: None, len: 0, .. }),
                    Some(crate::types::Ty::Array { family: None, len, .. }),
                ) if *len > 0
            )) && matches!(
                context.typed.expr_type(ast::expr_span(base)),
                Some(crate::types::Ty::Array { len, .. }) if *len > 0
            ) =>
        {
            if matches!(ty, Some(crate::types::Ty::Array { len: 0, .. })) {
                ty = context.typed.expr_type(span).cloned();
            }
            match element_array_concat(
                std::slice::from_ref(expression),
                ty.as_ref(),
                process,
                context,
            ) {
                Some(elements) => ProcessValueKind::Array(elements),
                None => {
                    reject_runtime_slice(expression, index, span, process, context);
                    ProcessValueKind::Invalid
                }
            }
        }
        ast::Expr::Index { base, index, .. } => {
            // A struct's own `Index` contract (`regs[2]`), as hardware lowers
            // it. Lowered as an element read, it did not lower at all.
            if let Some(value) = custom_index_value(expression, base, index, process, context) {
                return value;
            }
            if let Some(signals) = signal_reference(expression, process, context) {
                ProcessValueKind::Signal {
                    signals,
                    state: ProcessSignalState::Current,
                }
            } else {
                let base = value_ref(base, process, context);
                // An element of a computed vector (`(not u)[2]`) decodes
                // through the vector's layout, which an operator result
                // does not record.
                if context.process_ir.value_layouts[base.0 as usize].is_none() {
                    let value = &context.process_ir.values[base.0 as usize];
                    if let (
                        Some(
                            ty @ crate::types::Ty::Array {
                                family: Some(_),
                                len,
                                ..
                            },
                        ),
                        Some(width),
                    ) = (value.ty.clone(), value.bit_width)
                    {
                        // Only an operator or call result: places and their
                        // projections own declaration layouts (`15..8`).
                        let computed = matches!(
                            value.kind,
                            ProcessValueKind::Unary { .. }
                                | ProcessValueKind::Binary { .. }
                                | ProcessValueKind::InlineCall { .. }
                                | ProcessValueKind::Call { .. }
                        );
                        if len == width && computed {
                            let layout =
                                process_aggregate_layout_for_type(&ty, value.span, context);
                            context.process_ir.value_layouts[base.0 as usize] = layout;
                        }
                    }
                }
                if process_value_is_runtime_string(base, process, context) {
                    // `s[1..3]`: a new string (or array) of those elements.
                    // As an index, the range had no native form (#160).
                    let bounds = match index.as_ref() {
                        ast::Expr::Range { lo, hi, .. } => Some((Some(&**lo), Some(&**hi))),
                        ast::Expr::PartialRange { lo, hi, .. } => {
                            Some((lo.as_deref(), hi.as_deref()))
                        }
                        _ => None,
                    };
                    if let Some((lo, hi)) = bounds {
                        let integer = Some(crate::types::Ty::Integer);
                        let mut bound = |bound: Option<&ast::Expr>, last: bool| match bound {
                            Some(bound) => {
                                value_ref_with_type(bound, process, context, integer.as_ref())
                            }
                            None if !last => push_value(
                                span,
                                integer.clone(),
                                Some(64),
                                ProcessValueKind::Number(ProcessNumber::Integer(vec![0])),
                                context,
                            ),
                            None => {
                                let length = push_value(
                                    span,
                                    integer.clone(),
                                    Some(64),
                                    ProcessValueKind::HostCall {
                                        operation: ProcessHostValueOp::StringLength,
                                        arguments: vec![base],
                                    },
                                    context,
                                );
                                let one = push_value(
                                    span,
                                    integer.clone(),
                                    Some(64),
                                    ProcessValueKind::Number(ProcessNumber::Integer(vec![1])),
                                    context,
                                );
                                push_value(
                                    span,
                                    integer.clone(),
                                    Some(64),
                                    ProcessValueKind::Binary {
                                        operation: ProcessBinaryOp::Sub,
                                        left: length,
                                        right: one,
                                    },
                                    context,
                                )
                            }
                        };
                        let left = bound(lo, false);
                        let right = bound(hi, true);
                        // A handle, typed as the runtime array it came from:
                        // the checked `Char[3]` of `r[1..3]` is not its form.
                        let sliced = process_value_type(base, context).or_else(|| {
                            ty.clone()
                                .filter(|ty| !matches!(ty, crate::types::Ty::Error))
                        });
                        return push_value(
                            span,
                            sliced,
                            Some(64),
                            ProcessValueKind::HostCall {
                                operation: ProcessHostValueOp::ArraySlice,
                                arguments: vec![base, left, right],
                            },
                            context,
                        );
                    }
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
                    // An attribute's base (`rows[i]'length`) has no recorded
                    // type: the element type is the array's.
                    let ty = ty
                        .filter(|ty| !matches!(ty, crate::types::Ty::Error))
                        .or_else(|| {
                            runtime_array_element(process_value_type(base, context).as_ref())
                                .cloned()
                        })
                        .or(Some(crate::types::Ty::Char));
                    // A character enum element is held as its symbol's code.
                    if let Some(element) = ty.as_ref() {
                        if runtime_symbols(element, index_span, context).is_some() {
                            let code = push_value(
                                index_span,
                                Some(crate::types::Ty::Integer),
                                Some(64),
                                kind,
                                context,
                            );
                            return symbol_decode(code, element, index_span, process, context)
                                .unwrap_or(code);
                        }
                    }
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
                let bounds = base_layout
                    .as_ref()
                    .and_then(|layout| packed_slice_bounds(index, layout, context));
                // `self[self'right..self'left]` in a function: attribute
                // bounds fold once the receiver's layout is known.
                let bounds = match (bounds, index.as_ref()) {
                    (None, ast::Expr::Range { lo, hi, .. })
                        if base_layout.as_ref().is_some_and(|layout| {
                            matches!(&layout.kind, LayoutKind::Packed { range: Some(_), .. })
                        }) =>
                    {
                        let integer = Some(&crate::types::Ty::Integer);
                        let left = value_ref_with_type(lo, process, context, integer);
                        let right = value_ref_with_type(hi, process, context, integer);
                        constant_integer(left, context).zip(constant_integer(right, context))
                    }
                    (bounds, _) => bounds,
                };
                if let Some((left, right)) = bounds {
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
                let packed = base_layout
                    .as_ref()
                    .is_some_and(|layout| matches!(&layout.kind, LayoutKind::Packed { .. }));
                // `v[hi..lo]` with bounds known only at run time has no width
                // to give the value. Read as an element index of a range, it
                // lowered to a block nothing could run (#173, #195).
                if packed
                    && matches!(
                        index.as_ref(),
                        ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
                    )
                {
                    context.rejected.borrow_mut().push(
                        crate::diag::Diagnostic::error(format!(
                            "`{}` has bounds known only at run time",
                            crate::syntax::pretty::expr_string(expression)
                        ))
                        .with_code(crate::diag::codes::UNSUPPORTED_EXPR)
                        .at(span)
                        .help(
                            "packed-vector slice bounds must be constant; shift and resize \
                             instead, e.g. `resize(v >> lo, N)`",
                        ),
                    );
                    return push_value(span, ty, None, ProcessValueKind::Invalid, context);
                }
                let index = value_ref(index, process, context);
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
                // The selected element is a source enum, not a raw bit.
                // Named enum widths come from the elaborated declaration;
                // Ty::bit_width alone cannot represent a Logic discriminant.
                let width = source_value_width(&kind, ty.as_ref(), process, context)
                    .or_else(|| packed.then_some(1));
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
            } else if let Some((left, right)) = logic_string_comparison(
                lhs,
                rhs,
                left_type.as_ref(),
                right_type.as_ref(),
                process,
                context,
            ) {
                (left, right)
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
                let operands = if process_value_is_runtime_string(left, process, context)
                    && process_value_is_string_literal(right, context)
                {
                    Some((left, right))
                } else if process_value_is_runtime_string(right, process, context)
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
            then_value: value_ref_with_type(then, process, context, ty.as_ref()),
            else_value: value_ref_with_type(els, process, context, ty.as_ref()),
        },
        ast::Expr::Match {
            scrutinee, arms, ..
        } => {
            let scrutinee_type = context.typed.expr_type(ast::expr_span(scrutinee)).cloned();
            // Arms that are conditions (`match_needs_guards`) fold into a
            // first-match chain of selects, last arm innermost.
            if match_needs_guards(arms, scrutinee_type.as_ref(), context) {
                let scrutinee = lower_scrutinee(scrutinee, process, context);
                // Without a `_`, the arms are taken to cover the scrutinee
                // (the checker says when they do not): the last one is the
                // fallback rather than a value no arm produces.
                let mut result = (missing_value(span, context), span);
                let covered = !arms.iter().any(|arm| pattern_has_wildcard(&arm.pattern));
                for (position, arm) in arms.iter().enumerate().rev() {
                    let last = covered && position + 1 == arms.len();
                    let value = match arm.value_expr() {
                        Some(value) => (
                            value_ref_with_type(value, process, context, ty.as_ref()),
                            ast::expr_span(value),
                        ),
                        None => (missing_value(arm.span, context), arm.span),
                    };
                    result = if last || pattern_has_wildcard(&arm.pattern) {
                        value
                    } else {
                        let Some(condition) = scrutinee_condition(
                            &arm.pattern,
                            &scrutinee,
                            arm.span,
                            process,
                            context,
                        ) else {
                            return unsupported_process_value(span, ty.as_ref(), context);
                        };
                        inline_select_value(arm.span, condition, value, result, context)
                            .unwrap_or_else(|| {
                                (unsupported_process_value(span, ty.as_ref(), context), span)
                            })
                    };
                }
                return result.0;
            }
            let scrutinee = value_ref(scrutinee, process, context);
            let arms = arms
                .iter()
                .map(|arm| {
                    let value = match arm.value_expr() {
                        Some(value) => value_ref_with_type(value, process, context, ty.as_ref()),
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
            let callee_expression = callee.as_ref();
            let callee = value_ref(callee, process, context);
            let receiver = match context.process_ir.values[callee.0 as usize].kind {
                ProcessValueKind::Field { base, .. } => Some(base),
                _ => None,
            };
            // The callee's declared parameters type the arguments, as when
            // the call inlines as a value: a literal has no checked type.
            let function = match callee_expression {
                ast::Expr::Field { field, .. } => receiver
                    .and_then(|receiver| process_value_type(receiver, context))
                    .and_then(|ty| process_type_key(&ty, context))
                    .and_then(|owner| context.functions.get_associated(&owner, &field.text)),
                callee => context.functions.get(callee),
            };
            let declared = match function {
                Some(function)
                    if function.params.iter().filter(|p| !p.is_self).count() == args.len() =>
                {
                    function
                        .params
                        .iter()
                        .filter(|parameter| !parameter.is_self)
                        .map(|parameter| {
                            parameter
                                .ty
                                .as_ref()
                                .and_then(|ty| process_declared_type(ty, context))
                        })
                        .collect::<Vec<_>>()
                }
                _ => args
                    .iter()
                    .map(|argument| context.typed.expr_type(ast::expr_span(argument)).cloned())
                    .collect(),
            };
            let arguments = lower_call_arguments(args, &declared, receiver, process, context);
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
        ast::Expr::Construct {
            args,
            spread,
            ty: written,
            ..
        } => {
            // `Pair { .. }` names its type: an argument of a generic method
            // (`d.transform(Pair { .. })`) had no expected type, and the
            // construct had no layout (#234).
            if ty.is_none() {
                ty = written
                    .as_ref()
                    .and_then(|written| process_declared_type(written, context));
            }
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
        // A tuple is a value only as a `match` scrutinee or an assignment
        // target, which are lowered position by position; everywhere else it
        // was rewritten to a struct literal before lowering.
        ast::Expr::Tuple { .. } => ProcessValueKind::Invalid,
        ast::Expr::Concat { parts, .. } => {
            match element_array_concat(parts, ty.as_ref(), process, context) {
                Some(elements) => ProcessValueKind::Array(elements),
                None => ProcessValueKind::Concat(
                    parts
                        .iter()
                        .map(|part| value_ref(part, process, context))
                        .collect(),
                ),
            }
        }
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

    // A local of the CFG under construction is not in process_ir.processes
    // yet, so later lookups through the arena cannot find its declaration.
    // Give the read its declared type now: a `print!` argument has no
    // checked expression type, and an untyped `unsigned` local had no
    // display kind, so its `print!` was rejected.
    if ty.is_none() {
        if let ProcessValueKind::Local { local, .. } = &kind {
            ty = process.locals.get(local.0 as usize).and_then(|local| {
                local.ty.clone().or_else(|| {
                    local
                        .layout
                        .as_ref()
                        .and_then(|layout| process_type_from_layout(layout, context.resolved))
                })
            });
        }
    }
    ty = ty.or_else(|| process_kind_type(&kind, context));
    let width = source_value_width(&kind, ty.as_ref(), process, context);
    // The CFG under construction is not yet in process_ir.processes. Bind
    // local reads to its declaration now so nested fields/indices retain the
    // concrete layout during source lowering, not only after CFG publication.
    let local_layout = match &kind {
        ProcessValueKind::Local { local, .. } => process
            .locals
            .get(local.0 as usize)
            .and_then(|local| local.layout.clone()),
        ProcessValueKind::Call {
            callee: callee_value,
            arguments,
            ..
        } => match expression {
            // A deferred method call returning its receiver's type has the
            // receiver's format, as an inlined one does (`x.sqrt().to_real()`
            // reads the result's `self'high`).
            ast::Expr::Call { callee, .. } => match (
                callee.as_ref(),
                &context.process_ir.values[callee_value.0 as usize].kind,
            ) {
                (ast::Expr::Field { field, .. }, ProcessValueKind::Field { base, .. }) => {
                    let receiver = *base;
                    process_value_type(receiver, context)
                        .and_then(|ty| process_type_key(&ty, context))
                        .and_then(|owner| context.functions.get_associated(&owner, &field.text))
                        .filter(|function| returns_receiver_type(function, receiver, context))
                        .and_then(|_| {
                            process_value_source_layout(receiver, context.process_ir).cloned()
                        })
                }
                _ => context.functions.get(callee).and_then(|function| {
                    unsized_call_layout(
                        function,
                        ast::expr_span(expression),
                        arguments,
                        process,
                        context,
                    )
                }),
            },
            _ => None,
        },
        // `Boxed { .val = x }` of a generic struct, inside an inlined
        // `wrap<T>`: `Boxed<T>` has no declared layout, so its field values
        // give it one. Without it the construct did not lower (#204).
        ProcessValueKind::Construct {
            ty: Some(struct_type @ crate::types::Ty::Named(_)),
            fields,
            spread: None,
        } if process_aggregate_layout_for_type(struct_type, span, context).is_none() => (|| {
            let fields = fields
                .iter()
                .map(|field| {
                    Some(crate::ir::LayoutField {
                        name: field.name.clone()?,
                        direction: None,
                        layout: operand_source_layout(field.value?, process, context)?,
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Some(SourceLayout {
                span,
                kind: LayoutKind::Struct {
                    name: process_type_key(struct_type, context)?,
                    view: None,
                    fields,
                },
            })
        })(
        ),
        _ => None,
    };
    let width = match (&kind, &local_layout) {
        (ProcessValueKind::Call { .. }, Some(layout)) if layout.packed_width().is_some() => {
            layout.packed_width()
        }
        _ => width.or_else(|| local_layout.as_ref().and_then(SourceLayout::packed_width)),
    };
    let id = push_value(span, ty, width, kind, context);
    if local_layout.is_some() {
        context.process_ir.value_layouts[id.0 as usize] = local_layout;
    }
    id
}

/// The shape of a call to a function with an unsized result (`-> Logic[]`),
/// which its body sets: a returned `let`'s declared type, or a returned
/// parameter's argument.
fn unsized_call_layout(
    function: &ast::FnDecl,
    span: crate::diag::Span,
    arguments: &[ProcessValueId],
    process: &ProcessCfg,
    context: &LoweringContext<'_>,
) -> Option<SourceLayout> {
    let declared = process_declared_type(function.ret.as_ref()?, context)?;
    if !matches!(declared, crate::types::Ty::Array { len: 0, .. }) {
        return None;
    }
    let parameters = function
        .params
        .iter()
        .filter(|parameter| !parameter.is_self);
    super::source_bindings::returned_shapes(function)
        .into_iter()
        .find_map(|(name, ty)| match ty {
            Some(ty) => {
                let declared = process_declared_type(ty, context)?;
                let mut layout = process_layout_for_type(&declared, span, context)?;
                apply_process_declared_ranges(&mut layout, ty, context, &mut Default::default())?;
                layout.bit_width().is_some().then_some(layout)
            }
            None => {
                let index = parameters.clone().position(|parameter| {
                    parameter.name.as_ref().is_some_and(|n| n.text == name)
                })?;
                operand_source_layout(*arguments.get(index)?, process, context)
            }
        })
}

/// Insert one already-lowered value node.
fn truncate_process_values(context: &mut LoweringContext<'_>, length: usize) {
    context.cfg_call_cache.truncate(length);
    context.process_ir.values.truncate(length);
    context.process_ir.value_layouts.truncate(length);
    // Shared bodies and recorded calls built in the removed range go too.
    // Bodies are appended as they complete, so only a tail is ever removed.
    let process_ir = &mut *context.process_ir;
    process_ir
        .functions
        .retain(|function| (function.result.0 as usize) < length);
    let functions = process_ir.functions.len() as u32;
    process_ir
        .calls
        .retain(|call| (call.value.0 as usize) < length && call.function.0 < functions);
    context
        .shared_functions
        .borrow_mut()
        .retain(|_, shared| shared.is_none_or(|id| id.0 < functions));
}

/// `{ v[3..0], '1' }` into a `Bit[N]`/`Logic[N]`: the parts' elements in
/// order (a slice as written, a whole array from `'left`), as the elements
/// of an array value. `None` for a packed (numeric) target.
fn element_array_concat(
    parts: &[ast::Expr],
    ty: Option<&crate::types::Ty>,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
) -> Option<Vec<crate::ir::ProcessValueId>> {
    let Some(crate::types::Ty::Array {
        elem, family: None, ..
    }) = ty
    else {
        return None;
    };
    // Any element with a packed form: a nominal one (`Bit`), a kernel one
    // (`integer[4]`, #158) or a vector (`unsigned[8][4]`).
    let element_layout = process_layout_for_type(elem, ast::expr_span(&parts[0]), context);
    // A struct or array element too (`Pair[4]`, `unsigned[8][4][4]`, #182):
    // anything with a packed width.
    let element_width = match element_layout.as_ref().map(|layout| &layout.kind) {
        Some(
            LayoutKind::Scalar { .. }
            | LayoutKind::Packed { .. }
            | LayoutKind::Struct { .. }
            | LayoutKind::Array { .. },
        ) => element_layout.as_ref()?.packed_width()?,
        _ => return None,
    };
    let range_of = |value: crate::ir::ProcessValueId, context: &mut LoweringContext<'_>| {
        match operand_source_layout(value, process, context)?.kind {
            LayoutKind::Array {
                range: Some(range), ..
            } => Some((range.left, range.right)),
            _ => None,
        }
    };
    let mut elements = Vec::new();
    for part in parts {
        let span = ast::expr_span(part);
        let (base, labels) = match part {
            ast::Expr::Index { base, index, .. }
                if matches!(
                    index.as_ref(),
                    ast::Expr::Range { .. } | ast::Expr::PartialRange { .. }
                ) =>
            {
                let base = value_ref(base, process, context);
                let (declared_left, declared_right) = range_of(base, context)?;
                let (left, right) = match index.as_ref() {
                    ast::Expr::Range { lo, hi, .. } => (Some(&**lo), Some(&**hi)),
                    ast::Expr::PartialRange { lo, hi, .. } => (lo.as_deref(), hi.as_deref()),
                    _ => unreachable!(),
                };
                let mut bound = |bound: Option<&ast::Expr>, default| match bound {
                    Some(bound) => constant_integer(value_ref(bound, process, context), context),
                    None => Some(default),
                };
                let left = bound(left, declared_left)?;
                let right = bound(right, declared_right)?;
                (base, crate::ir::loop_range(left, right))
            }
            _ => {
                let value = value_ref_with_type(part, process, context, Some(elem));
                match range_of(value, context) {
                    Some((left, right)) => (value, crate::ir::loop_range(left, right)),
                    None => {
                        elements.push(value);
                        continue;
                    }
                }
            }
        };
        for label in labels {
            let index = push_value(
                span,
                Some(crate::types::Ty::Integer),
                Some(64),
                ProcessValueKind::Number(ProcessNumber::Integer(vec![label as u64])),
                context,
            );
            let element = push_value(
                span,
                Some(elem.as_ref().clone()),
                Some(element_width),
                ProcessValueKind::Index { base, index },
                context,
            );
            context.process_ir.value_layouts[element.0 as usize] = element_layout.clone();
            elements.push(element);
        }
    }
    Some(elements)
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
        // Entity-qualified ports do not enter through a lexical storage node.
        // Carry their declaration's range/nominal shape before indexing; a
        // backend cannot recover a missing source slice from a Range value.
        ProcessValueKind::Signal { signals, state }
            if signals.len() == 1 && !matches!(state, ProcessSignalState::Event) =>
        {
            context
                .design
                .signals
                .get(signals[0].0 as usize)
                .and_then(|signal| context.design.source_layouts.get(&signal.path))
                .cloned()
        }
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
        // An element of `string[]` or `integer[][]` is a runtime handle the
        // backend recognizes by its layout (printing it, reading its text).
        ProcessValueKind::HostCall {
            operation: ProcessHostValueOp::StringIndex,
            ..
        } if matches!(
            ty,
            Some(crate::types::Ty::Array {
                len: 0,
                family: None,
                ..
            })
        ) =>
        {
            ty.as_ref()
                .and_then(|ty| process_layout_for_type(ty, span, context))
        }
        _ => None,
    };
    let width = width.or_else(|| layout.as_ref().and_then(SourceLayout::packed_width));
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
                    .filter_map(SourceLayout::packed_width);
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
                .and_then(SourceLayout::packed_width)
                .or_else(|| runtime_handle_width(local.layout.as_ref()))
                .or_else(|| local.ty.as_ref().and_then(typed_width))
                .filter(|width| *width != 0);
        }
        ProcessValueKind::Storage(storage) => {
            let storage = context.process_ir.storages.get(storage.0 as usize)?;
            return storage
                .layout
                .as_ref()
                .and_then(SourceLayout::packed_width)
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
                .and_then(SourceLayout::packed_width)
                .or_else(|| runtime_handle_width(storage.layout.as_ref()))
                .or_else(|| storage.ty.as_ref().and_then(typed_width))
                .filter(|width| *width != 0);
        }
        // A DUT port read (`dut.k`) is its signals, typed since the checker
        // names a port's type: a ranged `integer<0..15>` is 4 bits, not 64.
        ProcessValueKind::Signal {
            signals,
            state: ProcessSignalState::Current | ProcessSignalState::Old,
        } => {
            return signals
                .iter()
                .map(|signal| Some(context.design.signals.get(signal.0 as usize)?.width))
                .sum::<Option<u32>>()
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
                        | ProcessBinaryOp::Rem
                        | ProcessBinaryOp::SignedAdd
                        | ProcessBinaryOp::SignedSub
                        | ProcessBinaryOp::SignedMul
                        | ProcessBinaryOp::SignedDiv
                        | ProcessBinaryOp::SignedRem
                        | ProcessBinaryOp::Shr
                        | ProcessBinaryOp::ArithmeticShr,
                    left,
                    right,
                } => Some(value_width(left)?.max(value_width(right)?)),
                // A constant shifted by a constant keeps every bit (`1 << 100`
                // is multiword); any other kernel integer keeps its width, so
                // `(1 << f) + x` stays the word its local is declared as.
                ProcessValueKind::Binary {
                    operation: ProcessBinaryOp::Shl,
                    left,
                    right,
                } => match crate::ir::process::arena_constant_integer(
                    *left,
                    &context.process_ir.values,
                ) {
                    Some(constant) if constant >= 0 => {
                        shifted_width((128 - constant.leading_zeros()).max(1), *right, context)
                    }
                    _ => value_width(left),
                },
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
        // A parameter is pushed with its argument's width.
        ProcessValueKind::Parameter { .. } => None,
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
            ProcessUnaryOp::RealToInteger
            | ProcessUnaryOp::IntegerToReal
            | ProcessUnaryOp::IntegerToNatural
            | ProcessUnaryOp::NaturalToInteger
            | ProcessUnaryOp::RealToNatural
            | ProcessUnaryOp::NaturalToReal
            | ProcessUnaryOp::Real(_) => Some(64),
            ProcessUnaryOp::Neg => {
                let natural = value_width(operand)?;
                let literal_sign_headroom = matches!(
                    context
                        .design
                        .process_ir
                        .values
                        .get(operand.0 as usize)
                        .map(|value| &value.kind),
                    Some(ProcessValueKind::Number(ProcessNumber::Integer(_)))
                );
                Some(if literal_sign_headroom {
                    natural.saturating_add(1).min(64.max(natural))
                } else {
                    natural
                })
            }
            ProcessUnaryOp::Not => value_width(operand),
        },
        ProcessValueKind::RawResize { operand } | ProcessValueKind::InlineCall { operand, .. } => {
            value_width(operand)
        }
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
            | ProcessBinaryOp::FloatDiv
            | ProcessBinaryOp::FloatRem
            | ProcessBinaryOp::FloatPow => Some(64),
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
                .packed_width()
        }
        ProcessValueKind::Index { base, .. } => {
            match &process_value_source_layout(*base, context.process_ir)?.kind {
                LayoutKind::Array { element, .. } => element.packed_width(),
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
        .rev()
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

/// The exact value of arithmetic on integer literals alone (`0 - 5`):
/// `None` for anything else or on `i128` overflow. Division truncates toward
/// zero and `%` takes the dividend's sign.
pub(super) fn literal_arithmetic(expression: &ast::Expr) -> Option<i128> {
    match expression {
        ast::Expr::Int { text, .. } => {
            let ProcessNumber::Integer(words) = parse_number(text, None) else {
                return None;
            };
            let value = match words.as_slice() {
                [] => 0u128,
                [low] => u128::from(*low),
                [low, high] => u128::from(*low) | (u128::from(*high) << 64),
                _ => return None,
            };
            i128::try_from(value).ok()
        }
        ast::Expr::Unary {
            op: ast::UnOp::Neg,
            rhs,
            ..
        } => literal_arithmetic(rhs)?.checked_neg(),
        ast::Expr::Binary { op, lhs, rhs, .. } => {
            let (left, right) = (literal_arithmetic(lhs)?, literal_arithmetic(rhs)?);
            match op {
                ast::BinOp::Add => left.checked_add(right),
                ast::BinOp::Sub => left.checked_sub(right),
                ast::BinOp::Mul => left.checked_mul(right),
                ast::BinOp::Div => left.checked_div(right),
                ast::BinOp::Rem => left.checked_rem(right),
                _ => None,
            }
        }
        _ => None,
    }
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
            | ProcessBinaryOp::FloatRem
            | ProcessBinaryOp::FloatPow
            | ProcessBinaryOp::FloatEq
            | ProcessBinaryOp::FloatNe
            | ProcessBinaryOp::FloatLt
            | ProcessBinaryOp::FloatLe
            | ProcessBinaryOp::FloatGt
            | ProcessBinaryOp::FloatGe
    )
}

/// `real op x` (or `x op real`) for an `x` whose type has `impl From<S> for
/// real`: `x` promotes to `real` through that conversion, the top of the
/// numeric ladder, and the operation is the float one. `None` when the
/// operands do not mix `real` with such a type; an `integer` side keeps its
/// own coercion.
fn lower_process_real_promotion(
    op: &ast::BinOp,
    lhs: &ast::Expr,
    rhs: &ast::Expr,
    span: crate::diag::Span,
    process: &ProcessCfg,
    context: &mut LoweringContext<'_>,
    ty: Option<&crate::types::Ty>,
) -> Option<ProcessValueId> {
    let real = crate::types::Ty::Real;
    let left_type = operand_type(lhs, process, context);
    let right_type = operand_type(rhs, process, context);
    let left_real = matches!(left_type, Some(crate::types::Ty::Real));
    if left_real == matches!(right_type, Some(crate::types::Ty::Real)) {
        return None;
    }
    let (other, other_type, real_side) = if left_real {
        (rhs, right_type, lhs)
    } else {
        (lhs, left_type, rhs)
    };
    let function = other_type
        .as_ref()
        .filter(|ty| !matches!(ty, crate::types::Ty::Integer))
        .and_then(|ty| process_type_key(ty, context))
        .and_then(|key| context.functions.get_conversion("real", &key))?;
    let first_value = context.process_ir.values.len();
    let other_value = value_ref_with_type(other, process, context, other_type.as_ref());
    let Some(converted) = inline_process_function(
        function,
        span,
        None,
        &[other_value],
        process,
        context,
        Some(&real),
    ) else {
        truncate_process_values(context, first_value);
        return None;
    };
    let real_value = value_ref_with_type(real_side, process, context, Some(&real));
    let (left, right) = if left_real {
        (real_value, converted)
    } else {
        (converted, real_value)
    };
    let kind = ProcessValueKind::Binary {
        operation: lower_binary_operator(op, Some(&real), Some(&real)),
        left,
        right,
    };
    let ty = ty.cloned().or(Some(real));
    let width = source_value_width(&kind, ty.as_ref(), process, context);
    Some(push_value(span, ty, width, kind, context))
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
    // family beside that same literal selects signed operations, and a
    // `natural` unsigned ones (`n + 1`).
    let types = [left, right].into_iter().flatten().collect::<Vec<_>>();
    let signed = types
        .iter()
        .find_map(|ty| match ty {
            crate::types::Ty::Array {
                family: Some(family),
                ..
            } => Some(family.rsplit("::").next() == Some("signed")),
            crate::types::Ty::Natural => Some(false),
            _ => None,
        })
        .unwrap_or_else(|| types.iter().copied().any(process_type_is_signed));
    let signed_vector = types.iter().any(|ty| {
        matches!(ty, crate::types::Ty::Array { family: Some(family), .. }
            if family.rsplit("::").next() == Some("signed"))
    });
    match operator {
        ast::BinOp::Add if real => ProcessBinaryOp::FloatAdd,
        ast::BinOp::Sub if real => ProcessBinaryOp::FloatSub,
        ast::BinOp::Mul if real => ProcessBinaryOp::FloatMul,
        ast::BinOp::Div if real => ProcessBinaryOp::FloatDiv,
        ast::BinOp::Rem if real => ProcessBinaryOp::FloatRem,
        ast::BinOp::Eq if real => ProcessBinaryOp::FloatEq,
        ast::BinOp::Ne if real => ProcessBinaryOp::FloatNe,
        ast::BinOp::Lt if real => ProcessBinaryOp::FloatLt,
        ast::BinOp::Le if real => ProcessBinaryOp::FloatLe,
        ast::BinOp::Gt if real => ProcessBinaryOp::FloatGt,
        ast::BinOp::Ge if real => ProcessBinaryOp::FloatGe,
        ast::BinOp::Add if signed => ProcessBinaryOp::SignedAdd,
        ast::BinOp::Sub if signed => ProcessBinaryOp::SignedSub,
        ast::BinOp::Mul if signed => ProcessBinaryOp::SignedMul,
        // A built-in `/` or `%` on `signed` vectors is reached only inside
        // std's sign-handling bodies, which divide magnitudes: the unsigned
        // word operation, as in hardware. Signed, the magnitude of -128 read
        // back as -128 and `-128 % 87` came out 41.
        ast::BinOp::Div if signed && !signed_vector => ProcessBinaryOp::SignedDiv,
        ast::BinOp::Rem if signed && !signed_vector => ProcessBinaryOp::SignedRem,
        ast::BinOp::Shr if signed => ProcessBinaryOp::ArithmeticShr,
        ast::BinOp::Lt if signed => ProcessBinaryOp::SignedLt,
        ast::BinOp::Le if signed => ProcessBinaryOp::SignedLe,
        ast::BinOp::Gt if signed => ProcessBinaryOp::SignedGt,
        ast::BinOp::Ge if signed => ProcessBinaryOp::SignedGe,
        ast::BinOp::Add => ProcessBinaryOp::Add,
        ast::BinOp::Sub => ProcessBinaryOp::Sub,
        ast::BinOp::Mul => ProcessBinaryOp::Mul,
        ast::BinOp::Div => ProcessBinaryOp::Div,
        ast::BinOp::Rem => ProcessBinaryOp::Rem,
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

/// The root and the instance at hierarchical `path`.
fn instance_at(
    hierarchy: &Hierarchy,
    path: &str,
) -> Option<(crate::elab::InstanceId, crate::elab::InstanceId)> {
    fn find(
        hierarchy: &Hierarchy,
        id: crate::elab::InstanceId,
        at: &str,
        path: &str,
    ) -> Option<crate::elab::InstanceId> {
        if at == path {
            return Some(id);
        }
        let rest = path.strip_prefix(at)?.strip_prefix('.')?;
        hierarchy.instance(id).children.iter().find_map(|&child| {
            let name = &hierarchy.instance(child).name;
            rest.starts_with(name.as_str())
                .then(|| find(hierarchy, child, &format!("{at}.{name}"), path))
                .flatten()
        })
    }
    hierarchy.roots.iter().find_map(|&root| {
        find(hierarchy, root, &hierarchy.root_path(root), path).map(|id| (root, id))
    })
}

/// Every signal a procedural CFG reads, as a hardware process is sensitive
/// to: the values its instructions and branches use, written places aside.
fn procedural_sensitivity(process: &ProcessCfg, ir: &ProcessIr) -> Vec<ProcessSensitivity> {
    let mut roots = Vec::new();
    for block in &process.blocks {
        for instruction in &block.instructions {
            match instruction {
                ProcessInstruction::Declare {
                    initializer: Some(value),
                    ..
                }
                | ProcessInstruction::Assign { value, .. } => roots.push(*value),
                ProcessInstruction::Schedule { value, delay, .. } => roots.extend([*value, *delay]),
                ProcessInstruction::Runtime { arguments, .. } => roots.extend(arguments),
                ProcessInstruction::Declare { .. } => {}
            }
        }
        match &block.terminator {
            ProcessTerminator::Branch { condition, .. } => roots.push(*condition),
            ProcessTerminator::Match { scrutinee, .. } => roots.push(*scrutinee),
            ProcessTerminator::For { iterable, .. } => roots.push(*iterable),
            ProcessTerminator::Suspend { arguments, .. } => roots.extend(arguments),
            ProcessTerminator::Return {
                value: Some(value), ..
            } => roots.push(*value),
            _ => {}
        }
    }
    let mut sensitivity = Vec::new();
    for root in roots {
        for signal in ir.signal_reads(root).unwrap_or_default() {
            let signal = ProcessSensitivity::Signal(signal);
            if !sensitivity.contains(&signal) {
                sensitivity.push(signal);
            }
        }
    }
    sensitivity
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::{DiagnosticSink, FileId};

    #[test]
    fn checked_scalar_aliases_fill_only_incomplete_layouts() {
        let span = crate::diag::Span::new(FileId(0), 0..1);
        let opaque = SourceLayout {
            span,
            kind: LayoutKind::Opaque {
                name: "m::Scalar".into(),
                width: None,
            },
        };
        for (ty, domain) in [
            (crate::types::Ty::Integer, crate::ir::ScalarDomain::Integer),
            (crate::types::Ty::Real, crate::ir::ScalarDomain::Real),
            (crate::types::Ty::Char, crate::ir::ScalarDomain::Character),
        ] {
            assert_eq!(
                process_layout_with_type(&opaque, Some(&ty)),
                SourceLayout {
                    span,
                    kind: LayoutKind::Scalar {
                        width: ty.bit_width().unwrap(),
                        domain,
                        nominal: None,
                        value_range: None,
                    },
                }
            );
        }
        let constrained = SourceLayout {
            span,
            kind: LayoutKind::Scalar {
                width: 4,
                domain: crate::ir::ScalarDomain::Integer,
                nominal: None,
                value_range: Some((0, 10)),
            },
        };
        assert_eq!(
            process_layout_with_type(&constrained, Some(&crate::types::Ty::Integer)),
            constrained
        );
        assert_eq!(process_layout_with_type(&opaque, None), opaque);
    }

    #[test]
    fn receiver_format_normalizes_the_result_without_retyping_its_intermediate() {
        let span = crate::diag::Span::new(FileId(0), 0..1);
        let ty = crate::types::Ty::Array {
            elem: Box::new(crate::types::Ty::Error),
            len: 8,
            family: Some("Format".into()),
            low: 0,
        };
        let layout = SourceLayout {
            span,
            kind: LayoutKind::Packed {
                width: 8,
                family: "Format".into(),
                range: Some(crate::ir::LayoutRange { left: 3, right: -4 }),
                element_enum: None,
            },
        };
        let mut ir = ProcessIr {
            values: vec![
                ProcessValue {
                    span,
                    ty: Some(ty.clone()),
                    bit_width: Some(8),
                    kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![1])),
                },
                ProcessValue {
                    span,
                    ty: Some(ty),
                    bit_width: Some(64),
                    kind: ProcessValueKind::Number(ProcessNumber::Integer(vec![0x1ff])),
                },
            ],
            value_layouts: vec![Some(layout.clone()), None],
            ..ProcessIr::default()
        };
        let resolved = Resolved::default();
        let mut context = LoweringContext {
            modules: &[],
            resolved: &resolved,
            typed: &Typed::default(),
            design: &Design::default(),
            hierarchy: &Hierarchy::default(),
            root_path: "",
            process_ir: &mut ir,
            suffixes: &Default::default(),
            constants: &Default::default(),
            type_aliases: &Default::default(),
            constant_stack: Default::default(),
            functions: &crate::ir::FunctionIndex::new(&resolved),
            constant_integers: &Default::default(),
            value_bindings: vec![],
            self_formats: vec![],

            widening: std::collections::HashSet::new(),
            inline_self_values: vec![],
            inline_return_types: vec![],
            inline_return_blocks: vec![],
            inline_functions: Default::default(),
            inline_scope: None,
            inline_value_locals: Vec::new(),
            inline_value_branches: Vec::new(),
            cfg_call_cache: vec![],
            shared_functions: &std::cell::RefCell::new(SharedFunctions::new()),
            recursive_calls: &std::cell::RefCell::new(Vec::new()),
            rejected: &std::cell::RefCell::new(Vec::new()),
            self_calls: &std::cell::RefCell::new(std::collections::HashSet::new()),
            frames: Vec::new(),
            frame_functions: Default::default(),
        };

        let result = inherit_receiver_layout(ProcessValueId(1), ProcessValueId(0), &mut context);
        assert_eq!(result, ProcessValueId(2));
        assert_eq!(ir.values[1].bit_width, Some(64));
        assert!(ir.value_layouts[1].is_none());
        assert_eq!(ir.values[2].bit_width, Some(8));
        assert_eq!(ir.value_layouts[2], Some(layout));
        assert!(matches!(
            ir.values[2].kind,
            ProcessValueKind::RawResize {
                operand: ProcessValueId(1)
            }
        ));
        assert!(ir.validate(0).is_empty(), "{:?}", ir.validate(0));
    }

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
            &mut sink,
        )
        .expect("hardware scheduler view derives from Process IR");
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
                    if elements.len() == 2 && elements.iter().all(|element| {
                        let ProcessValueKind::InlineCall { operand, .. } =
                            &design.process_ir.values[element.0 as usize].kind else { return false };
                        matches!(design.process_ir.values[operand.0 as usize].kind,
                            ProcessValueKind::Select { .. })
                    })
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
        assert_eq!(design.process_ir.processes.len(), 4);
        assert!(!design.process_ir.values.is_empty());
        let descriptor = &design.process_ir.tests[0];
        assert_eq!(descriptor.qualified_name, "tests::Smoke");
        assert_eq!(
            descriptor.processes,
            [ProcessId(0), ProcessId(1), ProcessId(2), ProcessId(3)]
        );
        let flag_storage = design
            .process_ir
            .storages
            .iter()
            .find(|storage| storage.name == "flag")
            .expect("flag storage")
            .id;
        let initialization = &design.process_ir.processes[0];
        assert_eq!(initialization.activation, ProcessActivation::Initialization);
        assert!(initialization
            .blocks
            .iter()
            .any(|block| !block.instructions.is_empty()));
        let clock = &design.process_ir.processes[1];
        assert_eq!(clock.label.as_deref(), Some("Smoke::clock"));
        assert_eq!(clock.region, ProcessRegion::Procedural);
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
        let process = &design.process_ir.processes[2];
        assert_eq!(process.label.as_deref(), Some("Smoke::stimulus"));
        assert_eq!(process.region, ProcessRegion::Procedural);
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
        let hardware = &design.process_ir.processes[3];
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
        assert_eq!(hardware.region, ProcessRegion::Combinational);
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
        assert!(clock
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
            "test @tests::Smoke root {} processes [%p0, %p1, %p2, %p3]",
            descriptor.root.0
        )));
        // The storage arena is part of the product, so `--emit ir` style dumps
        // have to show it or a migration bug there is invisible.
        assert!(
            dump.contains(&format!("storage %g1 root {} i", descriptor.root.0)),
            "the dump should name the entity-level `i` storage:\n{dump}"
        );
        assert!(dump.contains(&format!(
            "process %p{} root {} owner {} [Smoke::stimulus]",
            process.id.0, descriptor.root.0, descriptor.root.0
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
            &mut sink,
        )
        .expect("hardware scheduler view derives from Process IR");

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
                ProcessFormatPart::Value { value, kind, .. } => Some((*value, kind)),
                _ => None,
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
            &mut sink,
        )
        .expect("hardware scheduler view derives from Process IR");
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

        // The source hardware walk already produced canonical CFGs. Clear
        // the public compatibility view to prove attaching a test plan (or
        // no plan) preserves those CFGs instead of importing scheduler units.
        let hardware = design.process_ir.clone();
        assert_eq!(hardware.processes.len(), 1);
        design.drivers.clear();
        design.event_blocks.clear();
        lower(
            &modules,
            &resolved,
            &typed,
            &hierarchy,
            None,
            &mut design,
            &mut DiagnosticSink::new(),
        )
        .expect("hardware scheduler view derives from Process IR");
        assert_eq!(design.process_ir, hardware);
        assert_eq!(design.drivers.len(), 1);
        assert!(design.process_ir.tests.is_empty());
        assert_eq!(design.process_ir.processes.len(), 1);
        let process = &design.process_ir.processes[0];
        assert_eq!(process.root, process.owner);
        assert_eq!(process.region, ProcessRegion::Combinational);
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
            &mut sink,
        )
        .expect("hardware scheduler view derives from Process IR");

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
             type Row = integer[7..4][-2..-1]; type Alias = Row; \
             fn first(values: integer[2]) -> integer { return values[0]; } \
             fn first_pair(value: Pair) -> integer { return value.a; } \
             #[test] entity Smoke {} \
             impl Smoke { \
               const VALUES: integer[2] = [7, 8]; \
               let seed: Pair = { .a = 0, .b = 0 }; \
               let result: integer = 0; \
               run: process { \
                 let local: integer[2] = [1, 2]; \
                 let labelled: Alias[5..4] = [[[1, 2, 3, 4], [5, 6, 7, 8]], [[9, 10, 11, 12], [13, 14, 15, 16]]]; \
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
            &mut sink,
        )
        .expect("hardware scheduler view derives from Process IR");

        assert_eq!(
            design.process_ir.value_layouts.len(),
            design.process_ir.values.len()
        );
        let local = design
            .process_ir
            .processes
            .iter()
            .flat_map(|process| &process.locals)
            .find(|local| local.name == "labelled")
            .unwrap();
        let mut layout = local.layout.as_ref().unwrap();
        for expected in [(5, 4), (-2, -1), (7, 4)] {
            let LayoutKind::Array { range, element } = &layout.kind else {
                panic!("expected a declaration-owned array dimension");
            };
            let range = range.unwrap();
            assert_eq!((range.left, range.right), expected);
            layout = element;
        }
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
            .zip(&design.process_ir.value_layouts)
            .all(|(value, layout)| layout
                .as_ref()
                .and_then(SourceLayout::packed_width)
                .is_none_or(|width| value.bit_width == Some(width))));
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
                functions: Vec::new(),
                calls: Vec::new(),
                storages: Vec::new(),
                processes: vec![ProcessCfg {
                    id: ProcessId(0),
                    root: crate::elab::InstanceId(1),
                    owner: crate::elab::InstanceId(1),
                    label: None,
                    span,
                    activation: ProcessActivation::TimeZero,
                    region: ProcessRegion::Procedural,
                    entry: ProcessBlockId(1),
                    inline_calls: Vec::new(),
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
