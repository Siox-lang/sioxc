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
use crate::syntax::walk::{walk_expr, walk_item, walk_type};

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

/// Write each partial range index as the explicit range the checker
/// recorded for it (#496), so every lowering path reads one meaning.
pub fn expand_partial_ranges(modules: &mut [Module], typed: &Typed) {
    if typed.partial_ranges.is_empty() {
        return;
    }
    let int = |value: i64, span: Span| -> Expr {
        let magnitude = Expr::Int {
            text: value.unsigned_abs().to_string(),
            span,
        };
        if value < 0 {
            Expr::Unary {
                op: UnOp::Neg,
                rhs: Box::new(magnitude),
                span,
            }
        } else {
            magnitude
        }
    };
    let mut expand = |e: &mut Expr| {
        if let Expr::Index { index, .. } = e {
            if let Expr::PartialRange { span, .. } = index.as_ref() {
                if let Some(&(lo, hi)) = typed.partial_ranges.get(span) {
                    let span = *span;
                    **index = Expr::Range {
                        lo: Box::new(int(lo, span)),
                        hi: Box::new(int(hi, span)),
                        span,
                    };
                }
            }
        }
        true
    };
    for module in modules {
        for item in &mut module.items {
            walk_item(item, &mut expand);
        }
    }
}

/// Give each unsized family constructor its declared target's format:
/// `let f: ufixed<8, 4> = ufixed(x);` lowers as `ufixed<8, 4>(x)` (#532).
pub fn expand_constructor_formats(modules: &mut [Module], typed: &Typed) {
    if typed.constructor_formats.is_empty() {
        return;
    }
    let mut expand = |e: &mut Expr| {
        if let Expr::Call { callee, span, .. } = e {
            if let Some(index) = typed.constructor_formats.get(span) {
                if matches!(callee.as_ref(), Expr::Path(_)) {
                    let base = std::mem::replace(
                        callee.as_mut(),
                        Expr::Int {
                            text: String::new(),
                            span: *span,
                        },
                    );
                    let at = expr_span(&base);
                    **callee = Expr::Index {
                        base: Box::new(base),
                        index: Box::new(index.clone()),
                        span: at,
                    };
                }
            }
        }
        true
    };
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
            // A constructor builds its written labels: `unsigned[6..4](5)'left`
            // is 6, where the testbench read the checked width as 0..2 (#144).
            Expr::Call { callee, .. } => match callee.as_ref() {
                Expr::Index { index, .. } => match index.as_ref() {
                    Expr::Range { lo, hi, .. } => literal(lo).zip(literal(hi)),
                    _ => None,
                },
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
            // A runtime index counts the same way: `arr[1..3][i]` is
            // `arr[1 + i]` (#315). ponytail: checked against the array's
            // bounds, not the slice's.
            single => match literal(single) {
                Some(position) => at(position),
                None => Expr::Binary {
                    op: if step > 0 { BinOp::Add } else { BinOp::Sub },
                    lhs: Box::new(at(0)),
                    rhs: Box::new(single.clone()),
                    span: *span,
                },
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
            Stmt::While { body: inner, .. } => block(inner, ranges, resolved),
            Stmt::Process(process) => block(&mut process.body, ranges, resolved),
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
    fn stmt(statement: &mut Stmt, arrays: &HashMap<String, (Type, usize)>) {
        match statement {
            Stmt::Assign { target, value, .. } => {
                // A whole slot: as many indices as the array has levels.
                let mut root = &*target;
                let mut depth = 0;
                while let Expr::Index { base, .. } = root {
                    root = base;
                    depth += 1;
                }
                let Expr::Path(path) = root else { return };
                let [name] = path.segments.as_slice() else {
                    return;
                };
                let Some((entity, levels)) = arrays.get(&name.text) else {
                    return;
                };
                if depth != *levels {
                    return;
                }
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
            Stmt::For { body, .. } | Stmt::While { body, .. } => {
                for inner in &mut body.stmts {
                    stmt(inner, arrays);
                }
            }
            Stmt::If(nested) => if_stmt(nested, arrays),
            _ => {}
        }
    }
    fn if_stmt(statement: &mut IfStmt, arrays: &HashMap<String, (Type, usize)>) {
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
            let arrays: HashMap<String, (Type, usize)> = implementation
                .items
                .iter()
                .filter_map(|member| match member {
                    ImplItem::Let(LetDecl {
                        name,
                        ty: Some(ty @ Type::Indexed { .. }),
                        value: None,
                        ..
                    }) => {
                        // `Cell[R][C]`: the element under every level (#300).
                        let mut base = ty;
                        let mut levels = 0;
                        while let Type::Indexed { base: inner, .. } = base {
                            base = inner;
                            levels += 1;
                        }
                        let head = match base {
                            Type::Generic { base, .. } => base.as_ref(),
                            head => head,
                        };
                        let Type::Path(path) = head else {
                            return None;
                        };
                        let id = resolved.resolved(path.span)?;
                        (resolved.kind_of(id) == Some(DefKind::Entity))
                            .then(|| (name.text.clone(), (base.clone(), levels)))
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

/// `y: unsigned[x'length] out`: an attribute of a port or signal in a
/// declared type is read from that declaration's own type, so `x:
/// unsigned[W]` gives `W` per instance. Widths are evaluated before any
/// signal exists, and the name was read as a value there, which had none
/// (#295). `'length`, `'low` and `'high` of a width (`[8]` is `0..7` by
/// position) or of a literal range.
pub fn fold_type_attributes(modules: &mut [Module]) {
    // Names in a type index are not resolved, so a declaration is found by
    // name among its entity's ports and its impl's `let`s.
    let mut scopes: HashMap<String, HashMap<String, Type>> = HashMap::new();
    for module in modules.iter() {
        for item in &module.items {
            match item {
                Item::Entity(entity) => {
                    let scope = scopes.entry(entity.name.text.clone()).or_default();
                    for port in &entity.ports {
                        scope.insert(port.name.text.clone(), port.ty.clone());
                    }
                }
                Item::Impl(implementation) => {
                    let Some(owner) = type_owner(&implementation.target) else {
                        continue;
                    };
                    let scope = scopes.entry(owner).or_default();
                    for member in &implementation.items {
                        if let ImplItem::Let(LetDecl {
                            name, ty: Some(ty), ..
                        }) = member
                        {
                            scope.insert(name.text.clone(), ty.clone());
                        }
                    }
                }
                _ => {}
            }
        }
    }
    fn type_owner(ty: &Type) -> Option<String> {
        match ty {
            Type::Path(path) => path.segments.last().map(|segment| segment.text.clone()),
            Type::Generic { base, .. } => type_owner(base),
            _ => None,
        }
    }
    let fold = |e: &mut Expr, declared: &HashMap<String, Type>| {
        let Expr::SysAttr { base, attr, span } = e else {
            return true;
        };
        let Expr::Path(path) = base.as_ref() else {
            return true;
        };
        let [name] = path.segments.as_slice() else {
            return true;
        };
        let Some(Type::Indexed {
            index: Some(index), ..
        }) = declared.get(&name.text)
        else {
            return true;
        };
        let int = |value: i64| Expr::Int {
            text: value.to_string(),
            span: *span,
        };
        let literal = |e: &Expr| match e {
            Expr::Int { text, .. } => text.replace('_', "").parse::<i64>().ok(),
            _ => None,
        };
        let replacement = match (index.as_ref(), attr.text.as_str()) {
            (Expr::Range { lo, hi, .. }, attribute) => {
                let (Some(left), Some(right)) = (literal(lo), literal(hi)) else {
                    return true;
                };
                match attribute {
                    "length" => int((right - left).abs() + 1),
                    "low" => int(left.min(right)),
                    "high" => int(left.max(right)),
                    _ => return true,
                }
            }
            (Expr::PartialRange { .. }, _) => return true,
            (width, "length") => width.clone(),
            (_, "low") => int(0),
            (width, "high") => Expr::Binary {
                op: BinOp::Sub,
                lhs: Box::new(width.clone()),
                rhs: Box::new(int(1)),
                span: *span,
            },
            _ => return true,
        };
        *e = replacement;
        false
    };
    let empty = HashMap::new();
    for module in modules {
        for item in &mut module.items {
            match item {
                Item::Entity(entity) => {
                    let declared = scopes.get(&entity.name.text).unwrap_or(&empty);
                    for port in &mut entity.ports {
                        walk_type(&mut port.ty, &mut |e| fold(e, declared));
                    }
                }
                Item::Impl(implementation) => {
                    let declared = type_owner(&implementation.target)
                        .and_then(|owner| scopes.get(&owner))
                        .unwrap_or(&empty);
                    for member in &mut implementation.items {
                        if let ImplItem::Let(LetDecl { ty: Some(ty), .. }) = member {
                            walk_type(ty, &mut |e| fold(e, declared));
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
