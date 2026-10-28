//! Type attributes: `pub attr integers: integer = self'high + 1;` inside a
//! type's implementation makes `x'integers` answer for every value `x` of
//! that type, and `Self'integers` inside its implementation.
//!
//! A type attribute is a shape fact: its value reads only `self`'s
//! attributes (system ones such as `'high`, or other type attributes),
//! constants and arithmetic over them, so a read is a constant, as `x'high`
//! is. The checker validates each declaration ([`codes::INVALID_TYPE_ATTR`])
//! and records each read; after checking, [`expand_type_attrs`] replaces
//! every read with the declared value, `self` standing for the read's base,
//! so later stages only ever see system attributes (`x'high + 1`).

use super::*;

impl Checker<'_> {
    /// The type attribute `name` that `base`'s type declares, with its owner
    /// key. `Self` is the implementation being checked.
    pub(super) fn type_attr(
        &self,
        base: &Expr,
        name: &str,
        sym: &HashMap<String, Ty>,
    ) -> Option<(String, &AttrDecl)> {
        let owner = match base {
            Expr::Path(path) if matches!(path.segments.as_slice(), [s] if s.text == "Self") => {
                self.current_impl_owner.borrow().clone()?
            }
            _ => self.ty_head(&self.type_of(base, sym))?,
        };
        let declaration = self.type_attrs.get(&(owner.clone(), name.to_string()))?;
        Some((owner, declaration))
    }

    /// Record a read of a type attribute for expansion, after checking that
    /// a private one is read inside its own type's implementation.
    pub(super) fn check_type_attr_read(&mut self, owner: &str, declaration: &AttrDecl, span: Span) {
        if !declaration.is_pub && self.current_impl_owner.borrow().as_deref() != Some(owner) {
            self.error_with_help(
                codes::PRIVATE_MEMBER,
                span,
                format!(
                    "type attribute `'{}` is private to `{}`",
                    declaration.name.text,
                    self.key_leaf(owner)
                ),
                "declare it `pub attr` to read it outside the type's implementation".to_string(),
            );
        }
        if let Some(value) = &declaration.default {
            self.type_attr_reads.insert(span, value.clone());
        }
    }

    /// Check one type attribute declaration: a free name, a value that is a
    /// shape fact, and a value of the declared type.
    pub(super) fn check_type_attr_decl(&mut self, declaration: &AttrDecl, in_trait_impl: bool) {
        let name = declaration.name.text.as_str();
        let taken = if SYS_ATTRS.contains(&name) || PHASE2_ATTRS.contains(&name) {
            Some("a system attribute")
        } else if self.attr_value_kinds.contains_key(name) {
            Some("a declared metadata attribute")
        } else {
            None
        };
        if let Some(taken) = taken {
            self.error(
                codes::INVALID_TYPE_ATTR,
                declaration.name.span,
                format!("`'{name}` is already {taken}, so a type attribute cannot use it"),
            );
        }
        if in_trait_impl {
            self.error(
                codes::INVALID_TYPE_ATTR,
                declaration.span,
                "a type attribute belongs to the type's own implementation, not a trait's"
                    .to_string(),
            );
        }
        let Some(value) = &declaration.default else {
            self.error(
                codes::INVALID_TYPE_ATTR,
                declaration.span,
                format!("type attribute `'{name}` needs a value: `= self'high + 1`"),
            );
            return;
        };
        if let Some(span) = self.not_a_shape_fact(value) {
            self.error_with_help(
                codes::INVALID_TYPE_ATTR,
                span,
                format!("type attribute `'{name}` must be a shape fact"),
                "it may read `self`'s attributes (`self'high`), constants and arithmetic over \
                 them, never the value itself; a computation on the value is a method"
                    .to_string(),
            );
            return;
        }
        let mut sym = HashMap::new();
        if let Some(ty) = self.current_self_ty.borrow().clone() {
            sym.insert("self".to_string(), ty);
        }
        self.check_expr(value, &sym);
        let expected = self.ast_ty(&declaration.ty);
        if !self.assignable(&expected, value, &sym) {
            self.error(
                codes::TYPE_MISMATCH,
                crate::syntax::ast::expr_span(value),
                format!(
                    "type attribute `'{name}` is declared {} but its value is {}",
                    ty_name(&expected),
                    ty_name(&self.type_of(value, &sym))
                ),
            );
        }
    }

    /// The first part of a type attribute's value that is not a shape fact.
    fn not_a_shape_fact(&self, e: &Expr) -> Option<Span> {
        match e {
            Expr::Int { .. }
            | Expr::SuffixLit { .. }
            | Expr::CharLit { .. }
            | Expr::StrLit { .. }
            | Expr::BitStrLit { .. } => None,
            Expr::Path(path) => (self
                .resolved
                .resolved(path.span)
                .and_then(|id| self.resolved.kind_of(id))
                != Some(DefKind::Const))
            .then_some(path.span),
            Expr::SysAttr { base, attr, span } => {
                let on_self = matches!(base.as_ref(), Expr::Path(path)
                    if matches!(path.segments.as_slice(), [s] if s.text == "self" || s.text == "Self"));
                (!on_self || matches!(attr.text.as_str(), "event" | "old")).then_some(*span)
            }
            Expr::Unary { rhs, .. } => self.not_a_shape_fact(rhs),
            Expr::Binary { lhs, rhs, .. } => self
                .not_a_shape_fact(lhs)
                .or_else(|| self.not_a_shape_fact(rhs)),
            Expr::IfExpr {
                cond, then, els, ..
            } => self
                .not_a_shape_fact(cond)
                .or_else(|| self.not_a_shape_fact(then))
                .or_else(|| self.not_a_shape_fact(els)),
            // A free function of shape facts (`clog2(self'length)`).
            Expr::Call {
                callee,
                qualifier: None,
                args,
                bang: false,
                ..
            } if matches!(callee.as_ref(), Expr::Path(_)) => args
                .iter()
                .find_map(|argument| self.not_a_shape_fact(argument)),
            other => Some(crate::syntax::ast::expr_span(other)),
        }
    }
}

/// Replace every type attribute read the checker recorded with the declared
/// value, `self` (and `Self`) standing for the read's base: `x'integers`
/// becomes `x'high + 1`. A value that reads another type attribute expands
/// in turn.
pub fn expand_type_attrs(modules: &mut [Module], typed: &Typed) {
    if typed.type_attr_reads.is_empty() {
        return;
    }
    let mut expand = |e: &mut Expr| expand_read(e, &typed.type_attr_reads, 0);
    for module in modules {
        for item in &mut module.items {
            walk_item(item, &mut expand);
        }
    }
}

/// `[10, 20, 30]'length` and `arr[0..2]'length`: the length of a value that
/// is not a name, from its checked type. Lowering read the attribute of a
/// declaration only, and these did not lower or read the element's width
/// (#203). A runtime array's length stays a run-time read.
pub fn fold_value_lengths(modules: &mut [Module], typed: &Typed) {
    let mut fold = |e: &mut Expr| {
        let Expr::SysAttr { base, attr, span } = e else {
            return true;
        };
        let attribute = attr.text.as_str();
        if !matches!(
            attribute,
            "length" | "left" | "right" | "high" | "low" | "ascending"
        ) || matches!(base.as_ref(), Expr::Path(_))
        {
            return true;
        }
        let literal = |e: &Expr| match e {
            Expr::Int { text, .. } => text.replace('_', "").parse::<i64>().ok(),
            _ => None,
        };
        // The value's labels, left to right: a literal is labelled from 0 and
        // a slice keeps the labels it selects (`arr[2..1]` is 2 down to 1),
        // as VHDL's are. `'high` and the rest did not lower (#224).
        let labels = match base.as_ref() {
            Expr::Array { elems, .. } if !elems.is_empty() => Some((0, elems.len() as i64 - 1)),
            Expr::StrLit { text, .. } if !text.is_empty() => {
                Some((0, text.chars().count() as i64 - 1))
            }
            Expr::Index { index, .. } => match index.as_ref() {
                Expr::Range { lo, hi, .. } => literal(lo).zip(literal(hi)),
                _ => None,
            },
            _ => None,
        };
        let len = match (attribute, typed.expr_type(expr_span(base)), labels) {
            ("length", Some(Ty::Array { len, .. }), _) if *len > 0 => Some(i64::from(*len)),
            (_, _, Some((left, right))) => Some(match attribute {
                "left" => left,
                "right" => right,
                "high" => left.max(right),
                "low" => left.min(right),
                _ => (right - left).abs() + 1,
            }),
            _ => None,
        };
        // `'ascending` is the comparison of the labels, a `Bool` that lowers
        // without a path to resolve.
        if let ("ascending", Some((left, right))) = (attribute, labels) {
            let int = |value: i64| {
                Box::new(Expr::Int {
                    text: value.to_string(),
                    span: *span,
                })
            };
            *e = Expr::Binary {
                op: BinOp::Le,
                lhs: int(left),
                rhs: int(right),
                span: *span,
            };
            return false;
        }
        match len {
            Some(len) => {
                *e = Expr::Int {
                    text: len.to_string(),
                    span: *span,
                };
                false
            }
            None => true,
        }
    };
    for module in modules {
        for item in &mut module.items {
            walk_item(item, &mut fold);
        }
    }
}

/// `arr[1..3][0]` is `arr[1]` and `arr[1..3][0..1]` is `arr[1..2]`: an
/// index into a slice counts positions from the slice's left end, as the
/// slice's checked type does. Chained, neither had a hardware form, and a
/// write through one did not lower (#262). Constant bounds only.
pub fn flatten_slice_chains(modules: &mut [Module]) {
    fn literal(e: &Expr) -> Option<i64> {
        match e {
            Expr::Int { text, .. } => text.replace('_', "").parse().ok(),
            _ => None,
        }
    }
    let mut flatten = |e: &mut Expr| {
        let Expr::Index { base, index, span } = e else {
            return true;
        };
        let Expr::Index {
            base: inner,
            index: range,
            ..
        } = base.as_ref()
        else {
            return true;
        };
        let Expr::Range { lo, hi, .. } = range.as_ref() else {
            return true;
        };
        let (Some(left), Some(right)) = (literal(lo), literal(hi)) else {
            return true;
        };
        let step = if left <= right { 1 } else { -1 };
        let at = |position: i64| Expr::Int {
            text: (left + step * position).to_string(),
            span: *span,
        };
        let mapped = match index.as_ref() {
            Expr::Range {
                lo,
                hi,
                span: range_span,
            } => match (literal(lo), literal(hi)) {
                (Some(a), Some(b)) => Expr::Range {
                    lo: Box::new(at(a)),
                    hi: Box::new(at(b)),
                    span: *range_span,
                },
                _ => return true,
            },
            single => match literal(single) {
                Some(position) => at(position),
                None => return true,
            },
        };
        *e = Expr::Index {
            base: inner.clone(),
            index: Box::new(mapped),
            span: *span,
        };
        true
    };
    for module in modules {
        for item in &mut module.items {
            walk_item(item, &mut flatten);
        }
    }
}

/// Expand `e` when it is a recorded read; true when the walk should still
/// descend into it.
fn expand_read(e: &mut Expr, reads: &HashMap<Span, Expr>, depth: usize) -> bool {
    let Expr::SysAttr { base, span, .. } = e else {
        return true;
    };
    // A value defined through itself was reported; stop rather than loop.
    let Some(value) = reads.get(span).filter(|_| depth < 32) else {
        return true;
    };
    let mut replacement = value.clone();
    let base = base.as_ref().clone();
    walk_expr(&mut replacement, &mut |inner: &mut Expr| match inner {
        Expr::Path(path) if matches!(path.segments.as_slice(), [s] if s.text == "self" || s.text == "Self") =>
        {
            *inner = base.clone();
            false
        }
        _ => true,
    });
    walk_expr(&mut replacement, &mut |inner: &mut Expr| {
        expand_read(inner, reads, depth + 1)
    });
    *e = replacement;
    false
}

/// Visit every expression of an item, types' width expressions included.
fn walk_item(item: &mut Item, f: &mut impl FnMut(&mut Expr) -> bool) {
    match item {
        Item::Const(constant) => {
            walk_type(&mut constant.ty, f);
            walk_expr(&mut constant.value, f);
        }
        Item::Fn(function) => walk_fn(function, f),
        Item::Entity(entity) => {
            for port in &mut entity.ports {
                walk_type(&mut port.ty, f);
            }
        }
        Item::Impl(implementation) => {
            for member in &mut implementation.items {
                match member {
                    ImplItem::Const(constant) => {
                        walk_type(&mut constant.ty, f);
                        walk_expr(&mut constant.value, f);
                    }
                    ImplItem::Let(declaration) => walk_let(declaration, f),
                    ImplItem::Fn(function) => walk_fn(function, f),
                    ImplItem::Process(process) => walk_block(&mut process.body, f),
                    ImplItem::Stmt(statement) => walk_stmt(statement, f),
                    ImplItem::AttrBinding(binding) => walk_expr(&mut binding.value, f),
                    ImplItem::ModeField { .. } | ImplItem::Attr(_) => {}
                }
            }
        }
        Item::Trait(declaration) => {
            for function in &mut declaration.items {
                walk_fn(function, f);
            }
        }
        _ => {}
    }
}

fn walk_fn(function: &mut FnDecl, f: &mut impl FnMut(&mut Expr) -> bool) {
    for parameter in &mut function.params {
        if let Some(ty) = &mut parameter.ty {
            walk_type(ty, f);
        }
    }
    if let Some(ty) = &mut function.ret {
        walk_type(ty, f);
    }
    if let Some(body) = &mut function.body {
        walk_block(body, f);
    }
}

fn walk_let(declaration: &mut LetDecl, f: &mut impl FnMut(&mut Expr) -> bool) {
    if let Some(ty) = &mut declaration.ty {
        walk_type(ty, f);
    }
    if let Some(value) = &mut declaration.value {
        walk_expr(value, f);
    }
}

fn walk_block(block: &mut Block, f: &mut impl FnMut(&mut Expr) -> bool) {
    for statement in &mut block.stmts {
        walk_stmt(statement, f);
    }
}

fn walk_if(statement: &mut IfStmt, f: &mut impl FnMut(&mut Expr) -> bool) {
    walk_expr(&mut statement.cond, f);
    walk_block(&mut statement.then, f);
    match statement.else_.as_deref_mut() {
        Some(ElseBranch::Block(block)) => walk_block(block, f),
        Some(ElseBranch::If(nested)) => walk_if(nested, f),
        None => {}
    }
}

fn walk_stmt(statement: &mut Stmt, f: &mut impl FnMut(&mut Expr) -> bool) {
    match statement {
        Stmt::Let(declaration) => walk_let(declaration, f),
        Stmt::Use(_) => {}
        Stmt::Assign {
            target,
            value,
            after,
            ..
        } => {
            walk_expr(target, f);
            walk_expr(value, f);
            if let Some(after) = after {
                walk_expr(after, f);
            }
        }
        Stmt::If(nested) => walk_if(nested, f),
        Stmt::Match(nested) => {
            walk_expr(&mut nested.scrutinee, f);
            for arm in &mut nested.arms {
                walk_block(&mut arm.body, f);
            }
        }
        Stmt::For { range, body, .. } => {
            walk_expr(range, f);
            walk_block(body, f);
        }
        Stmt::Expr(e) => walk_expr(e, f),
        Stmt::Return { value, .. } => {
            if let Some(value) = value {
                walk_expr(value, f);
            }
        }
    }
}

fn walk_type(ty: &mut Type, f: &mut impl FnMut(&mut Expr) -> bool) {
    match ty {
        Type::Path(_) => {}
        Type::Indexed { base, index, .. } => {
            walk_type(base, f);
            if let Some(index) = index {
                walk_expr(index, f);
            }
        }
        Type::Generic { base, args, .. } => {
            walk_type(base, f);
            for argument in args {
                walk_generic_arg(argument, f);
            }
        }
        Type::View { target, .. } => walk_type(target, f),
        Type::Tuple { elems, .. } => {
            for elem in elems {
                walk_type(elem, f);
            }
        }
    }
}

fn walk_generic_arg(argument: &mut GenericArg, f: &mut impl FnMut(&mut Expr) -> bool) {
    match argument {
        GenericArg::Positional(e) | GenericArg::Named { value: e, .. } => walk_expr(e, f),
        GenericArg::PositionalType(ty) | GenericArg::NamedType { ty, .. } => walk_type(ty, f),
    }
}

/// Visit `e` and, while `f` says so, its subexpressions, before-order.
fn walk_expr(e: &mut Expr, f: &mut impl FnMut(&mut Expr) -> bool) {
    if !f(e) {
        return;
    }
    match e {
        Expr::Int { .. }
        | Expr::SuffixLit { .. }
        | Expr::BitStrLit { .. }
        | Expr::CharLit { .. }
        | Expr::StrLit { .. }
        | Expr::Path(_) => {}
        Expr::Field { base, .. } | Expr::SysAttr { base, .. } => walk_expr(base, f),
        Expr::Index { base, index, .. } => {
            walk_expr(base, f);
            walk_expr(index, f);
        }
        Expr::Range { lo, hi, .. } => {
            walk_expr(lo, f);
            walk_expr(hi, f);
        }
        Expr::PartialRange { lo, hi, .. } => {
            for bound in [lo, hi].into_iter().flatten() {
                walk_expr(bound, f);
            }
        }
        Expr::Unary { rhs, .. } => walk_expr(rhs, f),
        Expr::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, f);
            walk_expr(rhs, f);
        }
        Expr::IfExpr {
            cond, then, els, ..
        } => {
            walk_expr(cond, f);
            walk_expr(then, f);
            walk_expr(els, f);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            walk_expr(scrutinee, f);
            for arm in arms {
                walk_block(&mut arm.body, f);
            }
        }
        Expr::Call {
            callee,
            type_args,
            qualifier,
            args,
            ..
        } => {
            walk_expr(callee, f);
            for argument in type_args {
                walk_generic_arg(argument, f);
            }
            if let Some(qualifier) = qualifier {
                walk_type(qualifier, f);
            }
            for argument in args {
                walk_expr(argument, f);
            }
        }
        Expr::Construct {
            ty, args, spread, ..
        } => {
            if let Some(ty) = ty {
                walk_type(ty, f);
            }
            for argument in args {
                if let Some(value) = &mut argument.value {
                    walk_expr(value, f);
                }
            }
            if let Some(spread) = spread {
                walk_expr(spread, f);
            }
        }
        Expr::Concat { parts: items, .. }
        | Expr::Array { elems: items, .. }
        | Expr::Tuple { elems: items, .. } => {
            for item in items {
                walk_expr(item, f);
            }
        }
    }
}

/// `for i in R` where `R` is a constant range: the range itself. A generate
/// loop unrolled only a written `lo..hi`, so a named range dropped the body's
/// drivers and instances, and a testbench loop over it did not lower (#207).
/// Runs after resolution, so the inlined bounds keep their resolved spans.
pub fn inline_loop_ranges(modules: &mut [Module], resolved: &Resolved) {
    let mut ranges: HashMap<DefId, Expr> = HashMap::new();
    for module in modules.iter() {
        for item in &module.items {
            let constants: Vec<&ConstDecl> = match item {
                Item::Const(constant) => vec![constant],
                Item::Impl(implementation) => implementation
                    .items
                    .iter()
                    .filter_map(|member| match member {
                        ImplItem::Const(constant) => Some(constant),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            for constant in constants {
                if let (Expr::Range { .. }, Some(id)) =
                    (&constant.value, resolved.declared(constant.name.span))
                {
                    ranges.insert(id, constant.value.clone());
                }
            }
        }
    }
    if ranges.is_empty() {
        return;
    }
    fn block(block: &mut Block, ranges: &HashMap<DefId, Expr>, resolved: &Resolved) {
        for statement in &mut block.stmts {
            stmt(statement, ranges, resolved);
        }
    }
    fn stmt(statement: &mut Stmt, ranges: &HashMap<DefId, Expr>, resolved: &Resolved) {
        match statement {
            Stmt::For {
                range, body: inner, ..
            } => {
                if let Expr::Path(path) = range {
                    if let Some(value) = resolved.resolved(path.span).and_then(|id| ranges.get(&id))
                    {
                        *range = value.clone();
                    }
                }
                block(inner, ranges, resolved);
            }
            Stmt::If(nested) => if_stmt(nested, ranges, resolved),
            Stmt::Match(nested) => {
                for arm in &mut nested.arms {
                    block(&mut arm.body, ranges, resolved);
                }
            }
            _ => {}
        }
    }
    fn if_stmt(statement: &mut IfStmt, ranges: &HashMap<DefId, Expr>, resolved: &Resolved) {
        block(&mut statement.then, ranges, resolved);
        match statement.else_.as_deref_mut() {
            Some(ElseBranch::Block(inner)) => block(inner, ranges, resolved),
            Some(ElseBranch::If(nested)) => if_stmt(nested, ranges, resolved),
            None => {}
        }
    }
    for module in modules.iter_mut() {
        for item in &mut module.items {
            match item {
                Item::Fn(function) => {
                    if let Some(body) = &mut function.body {
                        block(body, &ranges, resolved);
                    }
                }
                Item::Impl(implementation) => {
                    for member in &mut implementation.items {
                        match member {
                            ImplItem::Fn(function) => {
                                if let Some(body) = &mut function.body {
                                    block(body, &ranges, resolved);
                                }
                            }
                            ImplItem::Process(process) => {
                                block(&mut process.body, &ranges, resolved)
                            }
                            ImplItem::Stmt(statement) => stmt(statement, &ranges, resolved),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }
    // `v[R]`: a slice by the named range is the written one (#280). The
    // checker and lowering read a slice from its `lo..hi`, so `v[R]` typed
    // as one element.
    let mut slice = |e: &mut Expr| {
        if let Expr::Index { index, .. } = e {
            if let Expr::Path(path) = index.as_ref() {
                if let Some(value) = resolved.resolved(path.span).and_then(|id| ranges.get(&id)) {
                    **index = value.clone();
                }
            }
        }
        true
    };
    for module in modules {
        for item in &mut module.items {
            walk_item(item, &mut slice);
        }
    }
}

/// `stage[i] = { .din = x }` (or `= {}`) on an instance array: the element's
/// entity, `stage[i] = Cell { .din = x }`, as the declaration names it. A
/// slot already exists from its declaration; this only rebuilds it with the
/// connections written (#290).
pub fn type_instance_slots(modules: &mut [Module], resolved: &Resolved) {
    fn stmt(statement: &mut Stmt, arrays: &HashMap<String, Type>) {
        match statement {
            Stmt::Assign { target, value, .. } => {
                let mut root = &*target;
                while let Expr::Index { base, .. } | Expr::Field { base, .. } = root {
                    root = base;
                }
                let Expr::Path(path) = root else { return };
                let [name] = path.segments.as_slice() else {
                    return;
                };
                let (Some(entity), Expr::Index { .. }) = (arrays.get(&name.text), &*target) else {
                    return;
                };
                match value {
                    Expr::Construct { ty: ty @ None, .. } => *ty = Some(entity.clone()),
                    Expr::Concat { parts, span } if parts.is_empty() => {
                        *value = Expr::Construct {
                            ty: Some(entity.clone()),
                            args: Vec::new(),
                            spread: None,
                            span: *span,
                        };
                    }
                    _ => {}
                }
            }
            Stmt::For { body, .. } => {
                for inner in &mut body.stmts {
                    stmt(inner, arrays);
                }
            }
            Stmt::If(nested) => if_stmt(nested, arrays),
            _ => {}
        }
    }
    fn if_stmt(statement: &mut IfStmt, arrays: &HashMap<String, Type>) {
        for inner in &mut statement.then.stmts {
            stmt(inner, arrays);
        }
        match statement.else_.as_deref_mut() {
            Some(ElseBranch::Block(block)) => {
                for inner in &mut block.stmts {
                    stmt(inner, arrays);
                }
            }
            Some(ElseBranch::If(nested)) => if_stmt(nested, arrays),
            None => {}
        }
    }
    for module in modules {
        for item in &mut module.items {
            let Item::Impl(implementation) = item else {
                continue;
            };
            let arrays: HashMap<String, Type> = implementation
                .items
                .iter()
                .filter_map(|member| match member {
                    ImplItem::Let(LetDecl {
                        name,
                        ty: Some(Type::Indexed { base, .. }),
                        value: None,
                        ..
                    }) => {
                        let head = match base.as_ref() {
                            Type::Generic { base, .. } => base.as_ref(),
                            head => head,
                        };
                        let Type::Path(path) = head else {
                            return None;
                        };
                        let id = resolved.resolved(path.span)?;
                        (resolved.kind_of(id) == Some(DefKind::Entity))
                            .then(|| (name.text.clone(), (**base).clone()))
                    }
                    _ => None,
                })
                .collect();
            if arrays.is_empty() {
                continue;
            }
            for member in &mut implementation.items {
                if let ImplItem::Stmt(statement) = member {
                    stmt(statement, &arrays);
                }
            }
        }
    }
}
