//! Typed defaults reuse ordinary source calls and aggregate value contracts.

use super::*;

impl<'a> Interpreter<'a> {
    pub(super) fn default_value(
        &mut self,
        ty: &Type,
        receiver: Option<DefId>,
        depth: u32,
        span: Span,
    ) -> Result<Value, NotConstant> {
        let fail = |reason| NotConstant { span, reason };
        if depth > 64 {
            return Err(fail(Reason::RecursionLimit));
        }
        let source = self
            .environment
            .source()
            .ok_or_else(|| fail(Reason::Unsupported))?;
        let ty = self
            .instantiated_type(ty)
            .ok_or_else(|| fail(Reason::Unsupported))?;
        let normalized = source
            .normalized_type(&ty)
            .ok_or_else(|| fail(Reason::Unsupported))?;
        match normalized.as_ref() {
            Type::Indexed { base, index, .. } => {
                let (len, left, descending) =
                    self.array_bounds(index.as_deref(), Some(0), depth + 1, span)?;
                let element = Arc::new(self.contract(base, receiver, depth + 1)?);
                let mut values = Vec::new();
                if len != 0 {
                    let len = usize::try_from(len).map_err(|_| fail(Reason::Unsupported))?;
                    let value = self.default_value(base, receiver, depth + 1, span)?;
                    values
                        .try_reserve_exact(len)
                        .map_err(|_| fail(Reason::Unsupported))?;
                    // shortcut: constant arrays are dense; add shared repeat storage for large defaults.
                    values.resize(len, value);
                }
                return Ok(Value::Array {
                    element,
                    values: values.into(),
                    left,
                    descending,
                });
            }
            Type::Tuple { elems, .. } => {
                return Ok(Value::Tuple(
                    elems
                        .iter()
                        .map(|ty| self.default_value(ty, receiver, depth + 1, span))
                        .collect::<Result<Vec<_>, _>>()?
                        .into(),
                ));
            }
            _ => {}
        }
        let owner = source
            .type_id(&normalized, receiver)
            .ok_or_else(|| fail(Reason::Unsupported))?;
        if let Some(selected) = source.default_function(owner).map_err(fail)? {
            return self.invoke_selected(selected, vec![], depth + 1, span);
        }
        if Some(owner) == source.primitive_id("integer") {
            return Ok(Value::Integer(0));
        }
        if Some(owner) == source.primitive_id("natural") {
            return Ok(Value::Natural(0));
        }
        if Some(owner) == source.primitive_id("real") {
            return Ok(Value::Real(0.0));
        }
        if Some(owner) == source.primitive_id("Char") {
            return Ok(Value::Character('\0'));
        }
        if let Some(value) = source.first_variant(owner) {
            return Ok(value);
        }
        if let Some(base) = source.derived_base(owner) {
            let value = self.default_value(base, Some(owner), depth + 1, span)?;
            return Ok(Value::Record {
                owner,
                fields: Arc::from([value]),
            });
        }
        let structure = source
            .structure(owner)
            .filter(|structure| structure.params.params.is_empty() && structure.base.is_none())
            .ok_or_else(|| fail(Reason::Unsupported))?;
        let fields = structure
            .fields
            .iter()
            .map(|field| self.default_value(&field.ty, Some(owner), depth + 1, span))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Value::Record {
            owner,
            fields: fields.into(),
        })
    }
}
