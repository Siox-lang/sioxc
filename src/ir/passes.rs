//! Representation-neutral IR normalization passes.
//!
//! Metavalue reconstruction and lookup compaction operate only on finished IR
//! data. Keeping them outside frontend lowering makes that boundary explicit.

use super::*;

/// Rewrite `Expr::Logic(c)` in place to `Const(position of c in `lut`)` — the
/// std-supplied variant map of the default logic type. Recurses into children.
pub(super) fn resolve_logic_expr(e: &mut Expr, lut: &HashMap<String, u64>) {
    match e {
        Expr::MetaCmp {
            operands, inner, ..
        } => {
            for operand in operands {
                resolve_logic_expr(operand, lut);
            }
            resolve_logic_expr(inner, lut);
        }
        Expr::Logic(c) => {
            *e = Expr::Const(lut.get(&format!("'{c}'")).copied().unwrap_or(0));
        }
        Expr::Unary { rhs, .. } => resolve_logic_expr(rhs, lut),
        Expr::Binary { lhs, rhs, .. } => {
            resolve_logic_expr(lhs, lut);
            resolve_logic_expr(rhs, lut);
        }
        Expr::Slice { base, .. } => resolve_logic_expr(base, lut),
        Expr::TableLookup { index, .. } => resolve_logic_expr(index, lut),
        Expr::CheckedIndex { index, valid, .. } => {
            resolve_logic_expr(index, lut);
            resolve_logic_expr(valid, lut);
        }
        Expr::Select { cond, then, els } => {
            resolve_logic_expr(cond, lut);
            resolve_logic_expr(then, lut);
            resolve_logic_expr(els, lut);
        }
        Expr::CCall { args, .. } => {
            for a in args {
                resolve_logic_expr(a, lut);
            }
        }
        Expr::Const(_)
        | Expr::WideConst(_)
        | Expr::Real(_)
        | Expr::Current(_)
        | Expr::Old(_)
        | Expr::Event(_)
        | Expr::Unknown => {}
    }
}

/// Rewrite `Slice(Current(v), i, i)` — one element of a metavalue vector — into
/// its 9-value reconstruction: the companion nibble when it is a metavalue
/// according to the elaborated encoding, else the source-defined low/high
/// discriminant selected by the value bit. Recurses; does not descend into the
/// node it creates (the companion has no companion).
pub(super) fn reconstruct_expr(
    e: &mut Expr,
    meta_of: &HashMap<u32, u32>,
    elems: &HashMap<u32, u32>,
    encodings: &HashMap<u32, LogicEncoding>,
) {
    // A marked comparison: apply the `numeric_std` rule now that every
    // companion exists. Done before the generic walk so the operands, which are
    // also present inside `inner`, are not rewritten twice.
    if let Expr::MetaCmp {
        ne,
        operands,
        inner,
    } = e
    {
        let ne = *ne;
        let mut resolved = inner.as_ref().clone();
        reconstruct_expr(&mut resolved, meta_of, elems, encodings);
        let unknown = operands
            .iter()
            .filter_map(|operand| companion_read(operand, meta_of))
            .map(|(companion, read)| {
                let n = elems.get(&companion).copied().unwrap_or(0);
                encodings
                    .get(&companion)
                    .map(|encoding| any_unknown(&read, n, encoding))
                    .unwrap_or(Expr::Const(0))
            })
            .reduce(or_expr);
        *e = match unknown {
            // `/=` is the one that goes the other way: unknown operands are
            // definitely not equal.
            Some(unknown) if ne => or_expr(resolved, unknown),
            Some(unknown) => and_expr(resolved, not1(unknown)),
            None => resolved,
        };
        return;
    }
    // A whole-vector comparison with a metavalue operand is false (numeric_std).
    if let Expr::Binary { op, lhs, rhs } = e {
        if matches!(
            op,
            BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
                | BinOp::SLt
                | BinOp::SLe
                | BinOp::SGt
                | BinOp::SGe
        ) {
            // Per element, not `companion != 0`: a nibble is non-zero for
            // `'L'` and `'H'` too, and those are a weak 0 and a weak 1 that
            // `numeric_std` compares like any other value. The coarse test made
            // a vector holding a pull-up's `'H'` compare false against
            // everything.
            let cond = [companion_read(lhs, meta_of), companion_read(rhs, meta_of)]
                .into_iter()
                .flatten()
                .map(|(companion, read)| {
                    let elems = elems.get(&companion).copied().unwrap_or(0);
                    encodings
                        .get(&companion)
                        .map(|encoding| any_unknown(&read, elems, encoding))
                        .unwrap_or(Expr::Const(0))
                })
                .reduce(|a, b| Expr::Binary {
                    op: BinOp::Or,
                    lhs: Box::new(a),
                    rhs: Box::new(b),
                });
            if let Some(cond) = cond {
                let orig = e.clone();
                *e = Expr::Select {
                    cond: Box::new(cond),
                    then: Box::new(Expr::Const(0)),
                    els: Box::new(orig),
                };
                return;
            }
        }
    }
    if let Expr::Slice { base, hi, lo } = e {
        if hi == lo {
            if let Expr::Current(vid) | Expr::Old(vid) = base.as_ref() {
                if let Some(&cid) = meta_of.get(&vid.0) {
                    let Some(encoding) = encodings.get(&cid) else {
                        return;
                    };
                    let companion = match base.as_ref() {
                        Expr::Current(_) => Expr::Current(SignalId(cid)),
                        Expr::Old(_) => Expr::Old(SignalId(cid)),
                        _ => unreachable!(),
                    };
                    let elem = *lo;
                    let nibble = Expr::Slice {
                        base: Box::new(companion),
                        hi: 4 * elem + 3,
                        lo: 4 * elem,
                    };
                    let valbit = (**base).clone();
                    let valbit = Expr::Slice {
                        base: Box::new(valbit),
                        hi: *hi,
                        lo: *lo,
                    };
                    *e = Expr::Select {
                        cond: Box::new(not1(logic_disc_in(nibble.clone(), &encoding.binary))),
                        then: Box::new(nibble),
                        els: Box::new(Expr::Select {
                            cond: Box::new(valbit),
                            then: Box::new(Expr::Const(encoding.binary_value(true).unwrap_or(0))),
                            els: Box::new(Expr::Const(encoding.binary_value(false).unwrap_or(0))),
                        }),
                    };
                    return;
                }
            }
        }
    }
    match e {
        Expr::Unary { rhs, .. } => reconstruct_expr(rhs, meta_of, elems, encodings),
        Expr::Binary { lhs, rhs, .. } => {
            reconstruct_expr(lhs, meta_of, elems, encodings);
            reconstruct_expr(rhs, meta_of, elems, encodings);
        }
        Expr::Slice { base, .. } => reconstruct_expr(base, meta_of, elems, encodings),
        Expr::TableLookup { index, .. } => reconstruct_expr(index, meta_of, elems, encodings),
        Expr::Select { cond, then, els } => {
            reconstruct_expr(cond, meta_of, elems, encodings);
            reconstruct_expr(then, meta_of, elems, encodings);
            reconstruct_expr(els, meta_of, elems, encodings);
        }
        Expr::CCall { args, .. } => {
            for a in args {
                reconstruct_expr(a, meta_of, elems, encodings);
            }
        }
        _ => {}
    }
}

/// Preserve the temporal plane of a value read when looking up its metavalue
/// companion. `old(v)` must inspect `old(v$meta)`, not the current companion.
pub(super) fn companion_read(expr: &Expr, meta_of: &HashMap<u32, u32>) -> Option<(u32, Expr)> {
    match expr {
        Expr::Current(id) => meta_of
            .get(&id.0)
            .copied()
            .map(|companion| (companion, Expr::Current(SignalId(companion)))),
        Expr::Old(id) => meta_of
            .get(&id.0)
            .copied()
            .map(|companion| (companion, Expr::Old(SignalId(companion)))),
        _ => None,
    }
}

/// The source spelling of a free or associated function path: a bare name
/// (`clog2`), a fully qualified module function (`math::bits::clog2`), or
/// `Type::name` for a static associated function (`Unicode::code`). Semantic
/// lookup of free functions uses [`FunctionIndex`] and never this string.
/// `None` only for an empty or non-path callee.
pub fn call_fn_key(callee: &ast::Expr) -> Option<String> {
    let ast::Expr::Path(p) = callee else {
        return None;
    };
    (!p.segments.is_empty()).then(|| {
        p.segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join("::")
    })
}

/// The enum-variant names a pattern tree names directly (`State::Idle`, `'0'`),
/// flattening or-patterns. A wildcard names none — the caller treats it
/// separately.
pub(super) fn collect_named_variants(
    p: &ast::Pattern,
    out: &mut std::collections::HashSet<String>,
) {
    match p {
        ast::Pattern::Path(path) if path.segments.len() >= 2 => {
            out.insert(path.segments.last().expect("variant path").text.clone());
        }
        ast::Pattern::CharLit { ch, .. } => {
            out.insert(format!("'{ch}'"));
        }
        ast::Pattern::Or { alts, .. } => {
            for a in alts {
                collect_named_variants(a, out);
            }
        }
        _ => {}
    }
}

// --- per-element logical metavalue builders (0/1-valued Exprs) --------------

/// `a & b` (bitwise; on 0/1 operands this is logical and).
pub(super) fn and_expr(a: Expr, b: Expr) -> Expr {
    Expr::Binary {
        op: BinOp::And,
        lhs: Box::new(a),
        rhs: Box::new(b),
    }
}
/// `a | b`.
pub(super) fn or_expr(a: Expr, b: Expr) -> Expr {
    Expr::Binary {
        op: BinOp::Or,
        lhs: Box::new(a),
        rhs: Box::new(b),
    }
}
/// Logical `not` of a 0/1 value: `x == 0`.
pub(super) fn not1(x: Expr) -> Expr {
    Expr::Binary {
        op: BinOp::Eq,
        lhs: Box::new(x),
        rhs: Box::new(Expr::Const(0)),
    }
}
/// Bit `i` of a value expression.
pub(super) fn bit(e: &Expr, i: u32) -> Expr {
    Expr::Slice {
        base: Box::new(e.clone()),
        hi: i,
        lo: i,
    }
}

/// A test for membership in a discriminant set, emitted as a comparison
/// chain rather than a table read.
pub(super) fn logic_disc_in(discriminant: Expr, members: &std::collections::HashSet<u64>) -> Expr {
    members
        .iter()
        .copied()
        .map(|member| Expr::Binary {
            op: BinOp::Eq,
            lhs: Box::new(discriminant.clone()),
            rhs: Box::new(Expr::Const(member)),
        })
        .reduce(or_expr)
        .unwrap_or(Expr::Const(0))
}

/// The value-plane bit for a discriminant, from std's encoding.
pub(super) fn logic_value_bit(discriminant: Expr, encoding: &LogicEncoding) -> Expr {
    let mut result = Expr::Const(0);
    let mut entries = encoding.value_bits.iter().collect::<Vec<_>>();
    entries.sort_by_key(|(disc, _)| **disc);
    for (&disc, &value) in entries.into_iter().rev() {
        result = Expr::Select {
            cond: Box::new(Expr::Binary {
                op: BinOp::Eq,
                lhs: Box::new(discriminant.clone()),
                rhs: Box::new(Expr::Const(disc)),
            }),
            then: Box::new(Expr::Const(u64::from(value))),
            els: Box::new(result),
        };
    }
    result
}

/// The result of a binary logic table lookup, unrolled from std's table.
pub(super) fn logic_binary_table_result(
    left: Expr,
    right: Expr,
    table: &HashMap<(u64, u64), u64>,
) -> Expr {
    let side = table
        .keys()
        .map(|(left, right)| left.max(right))
        .copied()
        .max()
        .unwrap_or(0)
        + 1;
    let cells = side.saturating_mul(side);
    let mut words = vec![0u64; usize::try_from(cells.saturating_mul(4).div_ceil(64)).unwrap_or(0)];
    for (&(left, right), &result) in table {
        let bit = (left.saturating_mul(side).saturating_add(right)).saturating_mul(4);
        if let Some(word) = words.get_mut(usize::try_from(bit / 64).unwrap_or(usize::MAX)) {
            *word |= (result & 0xF) << (bit % 64);
        }
    }
    let cell = Expr::Binary {
        op: BinOp::Add,
        lhs: Box::new(Expr::Binary {
            op: BinOp::Mul,
            lhs: Box::new(left),
            rhs: Box::new(Expr::Const(side)),
        }),
        rhs: Box::new(right),
    };
    Expr::Slice {
        base: Box::new(Expr::Binary {
            op: BinOp::Shr,
            lhs: Box::new(words_const(words)),
            rhs: Box::new(Expr::Binary {
                op: BinOp::Mul,
                lhs: Box::new(cell),
                rhs: Box::new(Expr::Const(4)),
            }),
        }),
        hi: 3,
        lo: 0,
    }
}

/// The result of a unary logic table lookup, unrolled from std's table.
pub(super) fn logic_unary_table_result(operand: Expr, table: &HashMap<u64, u64>) -> Expr {
    let cells = table.keys().copied().max().unwrap_or(0) + 1;
    let mut words = vec![0u64; usize::try_from(cells.saturating_mul(4).div_ceil(64)).unwrap_or(0)];
    for (&disc, &result) in table {
        let bit = disc.saturating_mul(4);
        if let Some(word) = words.get_mut(usize::try_from(bit / 64).unwrap_or(usize::MAX)) {
            *word |= (result & 0xF) << (bit % 64);
        }
    }
    Expr::Slice {
        base: Box::new(Expr::Binary {
            op: BinOp::Shr,
            lhs: Box::new(words_const(words)),
            rhs: Box::new(Expr::Binary {
                op: BinOp::Mul,
                lhs: Box::new(operand),
                rhs: Box::new(Expr::Const(4)),
            }),
        }),
        hi: 3,
        lo: 0,
    }
}

/// Replace packed-constant dynamic shifts with shared constant lookup tables.
///
/// Std operator bodies deliberately lower through ordinary expressions. That
/// keeps their semantics visible to the frontend, but a logic truth table used
/// to become a 300+-bit integer shifted at runtime at every call site. LLVM can
/// eventually rediscover that this is a lookup, but only after constructing
/// and optimizing a very large amount of wide-integer IR. This final lowering
/// pass recognizes the representation-independent expression shape and gives
/// every backend the compact operation directly.
pub(super) fn compact_lookup_tables(design: &mut Design) {
    let mut tables = std::mem::take(&mut design.lookup_tables);
    let mut intern: HashMap<LookupTable, LookupTableId> = tables
        .iter()
        .cloned()
        .enumerate()
        .map(|(id, table)| (table, LookupTableId(id)))
        .collect();

    {
        let mut compact = |expr: &mut Expr| {
            let owned = std::mem::replace(expr, Expr::Unknown);
            *expr = compact_lookup_expr(owned, &mut tables, &mut intern);
        };
        for driver in &mut design.drivers {
            if let Some(cond) = &mut driver.cond {
                compact(cond);
            }
            compact(&mut driver.expr);
            if let Some(meta) = &mut driver.meta {
                compact(meta);
            }
        }
        for block in &mut design.event_blocks {
            compact(&mut block.condition);
            for update in &mut block.updates {
                if let Some(cond) = &mut update.cond {
                    compact(cond);
                }
                compact(&mut update.expr);
                if let Some(meta) = &mut update.meta {
                    compact(meta);
                }
            }
        }
    }
    design.lookup_tables = tables;
}

/// Replace an unrolled table expression with a shared [`LookupTable`],
/// interning identical tables so one is emitted per distinct table.
pub(super) fn compact_lookup_expr(
    expr: Expr,
    tables: &mut Vec<LookupTable>,
    intern: &mut HashMap<LookupTable, LookupTableId>,
) -> Expr {
    let recurse = |expr, tables: &mut Vec<_>, intern: &mut HashMap<_, _>| {
        Box::new(compact_lookup_expr(expr, tables, intern))
    };
    let expr = match expr {
        Expr::MetaCmp {
            ne,
            operands,
            inner,
        } => Expr::MetaCmp {
            ne,
            operands: operands
                .into_iter()
                .map(|operand| compact_lookup_expr(operand, tables, intern))
                .collect(),
            inner: recurse(*inner, tables, intern),
        },
        Expr::Unary { op, rhs } => Expr::Unary {
            op,
            rhs: recurse(*rhs, tables, intern),
        },
        Expr::Binary { op, lhs, rhs } => Expr::Binary {
            op,
            lhs: recurse(*lhs, tables, intern),
            rhs: recurse(*rhs, tables, intern),
        },
        Expr::Slice { base, hi, lo } => Expr::Slice {
            base: recurse(*base, tables, intern),
            hi,
            lo,
        },
        Expr::TableLookup { table, index } => Expr::TableLookup {
            table,
            index: recurse(*index, tables, intern),
        },
        Expr::CheckedIndex {
            index,
            valid,
            left,
            right,
            span,
        } => Expr::CheckedIndex {
            index: recurse(*index, tables, intern),
            valid: recurse(*valid, tables, intern),
            left,
            right,
            span,
        },
        Expr::Select { cond, then, els } => Expr::Select {
            cond: recurse(*cond, tables, intern),
            then: recurse(*then, tables, intern),
            els: recurse(*els, tables, intern),
        },
        Expr::CCall {
            name,
            args,
            f64_args,
            integer_args,
            f64_ret,
            integer_ret,
        } => Expr::CCall {
            name,
            args: args
                .into_iter()
                .map(|argument| compact_lookup_expr(argument, tables, intern))
                .collect(),
            f64_args,
            integer_args,
            f64_ret,
            integer_ret,
        },
        leaf => leaf,
    };

    let Some((table, index)) = packed_lookup(&expr) else {
        return expr;
    };
    let id = match intern.get(&table).copied() {
        Some(id) => id,
        None => {
            let id = LookupTableId(tables.len());
            tables.push(table.clone());
            intern.insert(table, id);
            id
        }
    };
    Expr::TableLookup {
        table: id,
        index: Box::new(index.clone()),
    }
}

/// Recognize `(packed >> (index * element_width))[element_width-1..0]`.
pub(super) fn packed_lookup(expr: &Expr) -> Option<(LookupTable, &Expr)> {
    let Expr::Slice { base, hi, lo: 0 } = expr else {
        return None;
    };
    let element_width = hi.checked_add(1)?;
    if element_width > 64 {
        return None;
    }
    let Expr::Binary {
        op: BinOp::Shr,
        lhs: packed,
        rhs,
    } = base.as_ref()
    else {
        return None;
    };
    let Expr::Binary {
        op: BinOp::Mul,
        lhs: index,
        rhs: stride,
    } = rhs.as_ref()
    else {
        return None;
    };
    if !matches!(stride.as_ref(), Expr::Const(width) if *width == u64::from(element_width)) {
        return None;
    }
    let words: &[u64] = match packed.as_ref() {
        Expr::Const(word) => std::slice::from_ref(word),
        Expr::WideConst(words) => words,
        _ => return None,
    };
    let cells = words
        .len()
        .saturating_mul(64)
        .div_ceil(element_width as usize);
    let mask = if element_width == 64 {
        u64::MAX
    } else {
        (1u64 << element_width) - 1
    };
    let mut values = (0..cells)
        .map(|cell| {
            let bit = cell * element_width as usize;
            let word = bit / 64;
            let offset = bit % 64;
            let mut value = words[word] >> offset;
            if offset + element_width as usize > 64 {
                value |= words.get(word + 1).copied().unwrap_or(0) << (64 - offset);
            }
            value & mask
        })
        .collect::<Vec<_>>();
    while values.len() > 1 && values.last() == Some(&0) {
        values.pop();
    }
    Some((
        LookupTable {
            element_width,
            values,
        },
        index,
    ))
}

/// One operand metavalue lowering wants to hoist into its own signal.
pub(super) struct MetaTemp {
    pub(super) id: u32,
    pub(super) width: u32,
    pub(super) expr: Expr,
    pub(super) ctx: u32,
    pub(super) anchor: crate::diag::Span,
}

/// Signals metavalue lowering wants to create, collected while it holds only
/// `&self`. Ids are handed out from `next_id`, which starts at the current
/// signal count, so a caller that appends `made` in order gets exactly the ids
/// the returned expressions already reference. `ctx`/`anchor` are the driver
/// being lowered, so a hoisted operand keeps its originating context and
/// declaration anchor.
pub(super) struct MetaTemps {
    /// Whether this sink may hoist at all. `false` for a caller that cannot
    /// append signals to the design; lowering then keeps the fully inlined form
    /// those paths have always produced.
    pub(super) hoist: bool,
    pub(super) next_id: u32,
    pub(super) ctx: u32,
    pub(super) anchor: crate::diag::Span,
    pub(super) made: Vec<MetaTemp>,
}

impl MetaTemps {
    /// A hoisting sink whose first temporary takes `next_id`, carrying the
    /// context and anchor of the write being lowered.
    pub(super) fn new(next_id: u32, ctx: u32, anchor: crate::diag::Span) -> Self {
        Self {
            hoist: true,
            next_id,
            ctx,
            anchor,
            made: Vec::new(),
        }
    }

    /// A sink for a caller that cannot append signals -- resolution folding and
    /// the partial-write helpers, which build a companion expression while
    /// holding only `&self`. Nothing is ever recorded, so the placeholder
    /// anchor is never read.
    pub(super) fn inline_only() -> Self {
        Self {
            hoist: false,
            next_id: 0,
            ctx: 0,
            anchor: crate::diag::Span::new(crate::diag::FileId(0), 0..0),
            made: Vec::new(),
        }
    }
}

/// Bind `expr` to a fresh signal and return a read of it, so an enclosing
/// per-element unroll references a leaf instead of deep-copying the whole
/// subtree once per element. A leaf comes back unchanged: binding it would add
/// a signal without removing a copy.
pub(super) fn materialize(expr: Expr, width: u32, temps: &mut MetaTemps) -> Expr {
    if !temps.hoist
        || width == 0
        || matches!(
            expr,
            Expr::Const(_) | Expr::WideConst(_) | Expr::Current(_) | Expr::Old(_)
        )
    {
        return expr;
    }
    let id = temps.next_id;
    temps.next_id += 1;
    temps.made.push(MetaTemp {
        id,
        width,
        expr,
        ctx: temps.ctx,
        anchor: temps.anchor,
    });
    Expr::Current(SignalId(id))
}

/// The discriminant of element `index`: its companion nibble when the
/// element is a metavalue, otherwise the value plane's bit widened to a
/// discriminant.
pub(super) fn logic_element_disc(
    value: &Expr,
    meta: &Expr,
    index: u32,
    encoding: &LogicEncoding,
) -> Expr {
    let nibble = Expr::Slice {
        base: Box::new(meta.clone()),
        hi: 4 * index + 3,
        lo: 4 * index,
    };
    let low = encoding.binary_value(false).unwrap_or(0);
    let high = encoding.binary_value(true).unwrap_or(low);
    Expr::Select {
        cond: Box::new(not1(logic_disc_in(nibble.clone(), &encoding.binary))),
        then: Box::new(nibble),
        els: Box::new(Expr::Select {
            cond: Box::new(bit(value, index)),
            then: Box::new(Expr::Const(high)),
            els: Box::new(Expr::Const(low)),
        }),
    }
}

/// Replicate one element across `count` positions at `stride` bits each.
pub(super) fn repeat_element_plane(element: Expr, count: u32, stride: u32) -> Expr {
    let mut result = Expr::Const(0);
    for index in 0..count {
        result = or_expr(
            result,
            Expr::Binary {
                op: BinOp::Shl,
                lhs: Box::new(element.clone()),
                rhs: Box::new(Expr::Const((index * stride) as u64)),
            },
        );
    }
    result
}

/// Is element `i` of the companion an *unknown*?
///
/// `'L'` and `'H'` are not: `std_logic_1164` defines them as a weak 0 and a
/// weak 1, and both its tables and `numeric_std` treat them as such -- `H and 0`
/// is `'0'`, and `"0000H100" + b` is ordinary arithmetic. Testing the whole
/// discriminant range at or above `'Z'` swept them in, so a vector holding a
/// pull-up's `'H'` poisoned every operation applied to it.
///
/// The exact set comes from `LogicEncoding::to_x01`; no discriminant interval
/// or symbol spelling is known here.
pub(super) fn meta_bit(m: &Option<Expr>, i: u32, encoding: &LogicEncoding) -> Expr {
    let Some(m) = m else { return Expr::Const(0) };
    let nibble = Expr::Slice {
        base: Box::new(m.clone()),
        hi: 4 * i + 3,
        lo: 4 * i,
    };
    logic_disc_in(nibble, &encoding.unknown)
}

/// Does any element of `m` hold an unknown? The arithmetic rule is all-or-
/// nothing per vector, but it must ask the same question per element as the
/// logical rule -- a bare `companion != 0` also fires on `'L'`/`'H'`, whose
/// discriminants are non-zero but whose values are perfectly definite.
pub(super) fn any_unknown(m: &Expr, width: u32, encoding: &LogicEncoding) -> Expr {
    let mut acc = Expr::Const(0);
    for i in 0..width {
        acc = or_expr(acc, meta_bit(&Some(m.clone()), i, encoding));
    }
    acc
}
/// Place `'X'` (disc `x_disc`, from std's logic enum) in nibble `i` when
/// `meta_i` (0/1) is set, else 0.
/// Place `disc` in nibble `i` when `meta_i` holds, and nothing otherwise.
/// `disc` is an expression rather than a constant because the discriminant a
/// logical operator produces depends on its operands: `'U'` dominates `'X'`.
pub(super) fn meta_nibble(meta_i: Expr, i: u32, disc: Expr) -> Expr {
    Expr::Binary {
        op: BinOp::Shl,
        lhs: Box::new(Expr::Binary {
            op: BinOp::Mul,
            lhs: Box::new(meta_i),
            rhs: Box::new(disc),
        }),
        rhs: Box::new(Expr::Const(4 * i as u64)),
    }
}
