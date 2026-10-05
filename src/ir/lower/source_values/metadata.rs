//! Canonical companion-plane construction from source-owned logic encodings.

use super::*;

impl SourceValues {
    /// Project storage bits without turning a physical 0/1 into an enum value.
    pub(in crate::ir::lower) fn raw_slice(
        &mut self,
        base: &Expr,
        high: u32,
        low: u32,
        span: crate::diag::Span,
    ) -> Expr {
        let result = self.slice(base, high, low, span);
        if high == low {
            let Expr::Canonical { value, .. } = result else {
                unreachable!()
            };
            self.raw_bits.insert(value);
        }
        result
    }

    pub(in crate::ir::lower) fn disc_in(
        &mut self,
        discriminant: &Expr,
        members: &HashSet<u64>,
        span: crate::diag::Span,
    ) -> Expr {
        let value = self.append(discriminant, span, None);
        let discriminant = self.reference(value);
        let mut members = members.iter().copied().collect::<Vec<_>>();
        members.sort_unstable();
        let mut result = None;
        for member in members {
            let equal = self.binary(BinOp::Eq, &discriminant, &Expr::Const(member), span);
            result = Some(match result {
                Some(previous) => self.binary(BinOp::Or, &previous, &equal, span),
                None => equal,
            });
        }
        result.unwrap_or(Expr::Const(0))
    }

    pub(in crate::ir::lower) fn value_bit(
        &mut self,
        discriminant: &Expr,
        encoding: &LogicEncoding,
        span: crate::diag::Span,
    ) -> Expr {
        let value = self.append(discriminant, span, None);
        let discriminant = self.reference(value);
        let mut result = Expr::Const(0);
        let mut entries = encoding.value_bits.iter().collect::<Vec<_>>();
        entries.sort_by_key(|(disc, _)| **disc);
        for (&disc, &value) in entries.into_iter().rev() {
            let equal = self.binary(BinOp::Eq, &discriminant, &Expr::Const(disc), span);
            result = self.select(&equal, &Expr::Const(u64::from(value)), &result, span);
        }
        result
    }

    pub(in crate::ir::lower) fn binary_table(
        &mut self,
        left: &Expr,
        right: &Expr,
        table: &HashMap<(u64, u64), u64>,
        span: crate::diag::Span,
    ) -> Expr {
        let (words, side) = packed_logic_binary_table(table);
        let row = self.binary(BinOp::Mul, left, &Expr::Const(side), span);
        let cell = self.binary(BinOp::Add, &row, right, span);
        let offset = self.binary(BinOp::Mul, &cell, &Expr::Const(4), span);
        let shifted = self.binary(BinOp::Shr, &words_const(words), &offset, span);
        self.raw_slice(&shifted, 3, 0, span)
    }

    pub(in crate::ir::lower) fn unary_table(
        &mut self,
        operand: &Expr,
        table: &HashMap<u64, u64>,
        span: crate::diag::Span,
    ) -> Expr {
        let words = packed_logic_unary_table(table);
        let offset = self.binary(BinOp::Mul, operand, &Expr::Const(4), span);
        let shifted = self.binary(BinOp::Shr, &words_const(words), &offset, span);
        self.raw_slice(&shifted, 3, 0, span)
    }

    pub(in crate::ir::lower) fn element_disc(
        &mut self,
        value: &Expr,
        meta: &Expr,
        index: u32,
        encoding: &LogicEncoding,
        span: crate::diag::Span,
    ) -> Expr {
        let nibble = self.raw_slice(meta, 4 * index + 3, 4 * index, span);
        let binary = self.disc_in(&nibble, &encoding.binary, span);
        let is_meta = self.binary(BinOp::Eq, &binary, &Expr::Const(0), span);
        let low = encoding.binary_value(false).unwrap_or(0);
        let high = encoding.binary_value(true).unwrap_or(low);
        let bit = self.raw_slice(value, index, index, span);
        let binary_value = self.select(&bit, &Expr::Const(high), &Expr::Const(low), span);
        self.select(&is_meta, &nibble, &binary_value, span)
    }

    pub(in crate::ir::lower) fn repeat_plane(
        &mut self,
        element: &Expr,
        count: u32,
        stride: u32,
        span: crate::diag::Span,
    ) -> Expr {
        let value = self.append(element, span, None);
        let element = self.reference(value);
        let mut result = Expr::Const(0);
        for index in 0..count {
            let shifted = self.binary(
                BinOp::Shl,
                &element,
                &Expr::Const(u64::from(index * stride)),
                span,
            );
            result = self.binary(BinOp::Or, &result, &shifted, span);
        }
        result
    }

    pub(in crate::ir::lower) fn unknown_elements(
        &mut self,
        meta: &Expr,
        width: u32,
        encoding: &LogicEncoding,
        span: crate::diag::Span,
    ) -> Expr {
        let value = self.append(meta, span, None);
        let meta = self.reference(value);
        let mut result = Expr::Const(0);
        for index in 0..width {
            let nibble = self.raw_slice(&meta, 4 * index + 3, 4 * index, span);
            let unknown = self.disc_in(&nibble, &encoding.unknown, span);
            result = self.binary(BinOp::Or, &result, &unknown, span);
        }
        result
    }

    pub(in crate::ir::lower) fn meta_nibble(
        &mut self,
        condition: &Expr,
        index: u32,
        discriminant: &Expr,
        span: crate::diag::Span,
    ) -> Expr {
        let value = self.binary(BinOp::Mul, condition, discriminant, span);
        self.binary(BinOp::Shl, &value, &Expr::Const(4 * u64::from(index)), span)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_logic_tables_keep_shared_operands_and_compact_without_projection() {
        let span = crate::diag::Span::new(crate::diag::FileId(0), 10..20);
        let mut arena = SourceValues::default();
        let input = arena.append(
            &Expr::CCall {
                name: "read_disc".into(),
                args: vec![],
                f64_args: vec![],
                integer_args: vec![],
                f64_ret: false,
                integer_ret: true,
            },
            span,
            Some(crate::types::Ty::Integer),
        );
        let operand = arena.reference(input);
        let binary = arena.binary_table(&operand, &operand, &[((4, 9), 6)].into(), span);
        let unary = arena.unary_table(&operand, &[(9, 4)].into(), span);
        let Expr::Canonical { value: binary, .. } = binary else {
            unreachable!()
        };
        let Expr::Canonical { value: unary, .. } = unary else {
            unreachable!()
        };
        let before = arena.ir.values[input.0 as usize].clone();
        let mut tables = Vec::new();
        arena.compact_lookups(&mut tables, &mut HashMap::new());
        let ProcessValueKind::TableLookup { table, index } =
            arena.ir.values[binary.0 as usize].kind
        else {
            panic!("binary table must become a canonical lookup");
        };
        assert_eq!(tables[table.0].values[49], 6);
        let ProcessValueKind::Binary {
            operation: ProcessBinaryOp::Add,
            left,
            right,
        } = arena.ir.values[index.0 as usize].kind
        else {
            unreachable!()
        };
        assert_eq!(right, input);
        assert!(matches!(arena.ir.values[left.0 as usize].kind,
            ProcessValueKind::Binary { operation: ProcessBinaryOp::Mul, left, .. } if left == input));
        let ProcessValueKind::TableLookup { table, index } = arena.ir.values[unary.0 as usize].kind
        else {
            panic!("unary table must become a canonical lookup");
        };
        assert_eq!(index, input);
        assert_eq!(tables[table.0].values[9], 4);
        assert_eq!(arena.ir.values[input.0 as usize], before);
        assert_eq!(
            arena
                .ir
                .values
                .iter()
                .filter(|value| matches!(value.kind, ProcessValueKind::ForeignCall { .. }))
                .count(),
            1
        );
        assert!(arena.ir.validate(0).is_empty());
    }
}
