//! Source-layout construction and flattened signal allocation.

use super::*;

impl<'a> Lowering<'a> {
    /// Add a signal for `name: ty`, flattening composites into scalar leaves: a
    /// struct into one signal per field (`s.valid`), an array into one per
    /// element (`a[0]`). Nested composites recurse. An integer vector
    /// (`unsigned[8]`) stays a single scalar signal.
    pub(super) fn add_typed_signal(
        &mut self,
        entity: &str,
        name: &str,
        ty: &ast::Type,
        env: &HashMap<String, i64>,
        declaration_span: crate::diag::Span,
    ) {
        // A generic entity's type parameters (`T -> unsigned[8]`) substitute first,
        // so a port/signal typed `T` becomes its concrete type here.
        let subst_ty;
        let ty = if self.cur_type_env.is_empty() {
            ty
        } else {
            subst_ty = subst_type_params(ty, &self.cur_type_env);
            &subst_ty
        };
        // Substitute `using X = T;` aliases transitively; an index applied to an alias of
        // an unconstrained array fills its hole (`string[5]` = `Char[5]`).
        let resolved;
        let ty = match ty {
            ast::Type::Path(_) => {
                let terminal = self.resolve_alias(ty);
                if std::ptr::eq(terminal, ty) {
                    ty
                } else {
                    resolved = terminal.clone();
                    &resolved
                }
            }
            ast::Type::Indexed {
                base,
                index: Some(i),
                span,
            } => {
                let inner = self.resolve_alias(base);
                match inner {
                    ast::Type::Indexed {
                        base: elem,
                        index: None,
                        ..
                    } => {
                        resolved = ast::Type::Indexed {
                            base: elem.clone(),
                            index: Some(i.clone()),
                            span: *span,
                        };
                        &resolved
                    }
                    _ => ty,
                }
            }
            _ => ty,
        };
        // An unconstrained array (`Char[]`) has no length to flatten with.
        if let ast::Type::Indexed { index: None, .. } = ty {
            self.sink.emit(
                crate::diag::Diagnostic::error(format!(
                    "unconstrained array type for `{name}`: the range must be set here                      (e.g. an explicit length)"
                ))
                .with_code(crate::diag::codes::TYPE_MISMATCH)
                .at(declaration_span),
            );
            return;
        }
        let layout = self.source_layout(ty, env);
        self.add_layout_signal(entity, name, &layout, declaration_span);
    }

    /// Persist and flatten one already-resolved layout. This is now the sole
    /// recursive storage traversal; AST declarations are consulted only while
    /// constructing the root `SourceLayout` above.
    pub(super) fn add_layout_signal(
        &mut self,
        entity: &str,
        name: &str,
        layout: &SourceLayout,
        declaration_span: crate::diag::Span,
    ) {
        self.out
            .source_layouts
            .insert(format!("{entity}.{name}"), layout.clone());
        match &layout.kind {
            LayoutKind::Struct {
                name: representation,
                view,
                fields,
            } => {
                self.local_struct.insert(
                    name.to_string(),
                    view.clone().unwrap_or_else(|| representation.clone()),
                );
                self.local_struct_repr
                    .insert(name.to_string(), representation.clone());
                for field in fields {
                    self.add_layout_signal(
                        entity,
                        &format!("{name}.{}", field.name),
                        &field.layout,
                        declaration_span,
                    );
                }
            }
            LayoutKind::Array { range, element } => {
                let Some(range) = range else {
                    return;
                };
                let indices = loop_range(range.left, range.right);
                self.local_array.insert(name.to_string(), indices.clone());
                for index in indices {
                    self.add_layout_signal(
                        entity,
                        &format!("{name}[{index}]"),
                        element,
                        declaration_span,
                    );
                }
            }
            LayoutKind::Packed {
                width,
                family,
                element_enum,
                ..
            } => {
                self.add_signal(entity, name, *width, declaration_span);
                self.local_numeric.insert(name.to_string(), family.clone());
                if let Some(&id) = self.locals.get(name) {
                    self.sig_type.insert(id.0, family.clone());
                    if let Some(element) = element_enum {
                        self.out.array_element_enums.insert(id.0, element.clone());
                    }
                }
            }
            LayoutKind::Scalar {
                width,
                domain,
                nominal,
                value_range,
            } => {
                self.add_signal(entity, name, *width, declaration_span);
                let Some(&id) = self.locals.get(name) else {
                    return;
                };
                if let Some(nominal) = nominal {
                    self.local_numeric.insert(name.to_string(), nominal.clone());
                    self.sig_type.insert(id.0, nominal.clone());
                }
                let signal = &mut self.out.signals[id.0 as usize];
                signal.range = *value_range;
                match domain {
                    ScalarDomain::Bits => {}
                    ScalarDomain::Integer => signal.integer = true,
                    ScalarDomain::Real => signal.real = true,
                    ScalarDomain::Character => {
                        signal.char = true;
                        self.local_char.insert(name.to_string());
                    }
                    ScalarDomain::Enum(enum_name) => {
                        self.local_enum.insert(name.to_string(), enum_name.clone());
                        self.sig_type.insert(id.0, enum_name.clone());
                        signal.enum_type = Some(enum_name.clone());
                        if let Some(&default) = self
                            .new_defaults
                            .get(enum_name)
                            .or_else(|| self.enum_first_disc.get(enum_name))
                        {
                            signal.init = vec![default];
                        }
                    }
                }
            }
            LayoutKind::Opaque { width, .. } => {
                self.add_signal(entity, name, width.unwrap_or(0), declaration_span);
            }
        }
    }

    /// Build the language-neutral recursive layout persisted on `Design`.
    /// Alias expansion, generic substitution, inherited fields, and concrete
    /// ranges happen here once; consumers must not need the source AST to
    /// recover them again.
    pub(super) fn source_layout(&self, ty: &ast::Type, env: &HashMap<String, i64>) -> SourceLayout {
        self.source_layout_at(ty, env, &mut HashSet::new())
    }

    /// The source layout for a type in `env`, recursing through fields and
    /// elements.
    pub(super) fn source_layout_at(
        &self,
        ty: &ast::Type,
        env: &HashMap<String, i64>,
        expanding: &mut HashSet<String>,
    ) -> SourceLayout {
        let ty = self.normalize_layout_type(ty);
        let span = source_type_span(&ty);
        let rendered = crate::syntax::pretty::type_str(&ty);

        if let Some(fields) = self.struct_fields(&ty).filter(|fields| !fields.is_empty()) {
            let recursion_key = rendered.clone();
            if !expanding.insert(recursion_key.clone()) {
                return SourceLayout {
                    span,
                    kind: LayoutKind::Opaque {
                        name: rendered,
                        width: None,
                    },
                };
            }
            let (name, view) = match &ty {
                ast::Type::View { view, target, .. } => (
                    self.free_fns
                        .type_head_key(target)
                        .unwrap_or_else(|| "<anonymous>".to_string()),
                    self.view_of(&ty).or_else(|| {
                        view.segments.last().map(|name| {
                            format!(
                                "{}@{}",
                                name.text,
                                self.free_fns
                                    .type_head_key(target)
                                    .unwrap_or_else(|| "<anonymous>".to_string())
                            )
                        })
                    }),
                ),
                ast::Type::Generic { base, .. } => (
                    self.free_fns
                        .type_head_key(base)
                        .unwrap_or_else(|| "<anonymous>".to_string()),
                    None,
                ),
                _ => (
                    self.free_fns
                        .type_head_key(&ty)
                        .unwrap_or_else(|| "<anonymous>".to_string()),
                    None,
                ),
            };
            let directions = self
                .view_of(&ty)
                .and_then(|view_key| self.view_dirs.get(&view_key));
            let fields = fields
                .into_iter()
                .map(|(name, ty)| {
                    let direction =
                        directions
                            .and_then(|directions| directions.get(&name))
                            .map(|direction| match direction {
                                ast::Direction::In => LayoutDirection::In,
                                ast::Direction::Out => LayoutDirection::Out,
                                ast::Direction::Inout => LayoutDirection::InOut,
                            });
                    LayoutField {
                        name,
                        direction,
                        layout: self.source_layout_at(&ty, env, expanding),
                    }
                })
                .collect();
            expanding.remove(&recursion_key);
            return SourceLayout {
                span,
                kind: LayoutKind::Struct { name, view, fields },
            };
        }

        if let Some((element, indices)) = array_of(
            &ty,
            env,
            &self.const_ranges,
            &self.array_families,
            &self.free_fns,
        ) {
            let range = indices
                .first()
                .copied()
                .zip(indices.last().copied())
                .map(|(left, right)| LayoutRange { left, right });
            return SourceLayout {
                span,
                kind: LayoutKind::Array {
                    range,
                    element: Box::new(self.source_layout_at(element, env, expanding)),
                },
            };
        }

        if let Some(family) = self.packed_family(&ty, env) {
            let width = type_width(&ty, env, &self.free_fns, &self.structs, &self.const_ranges);
            return SourceLayout {
                span,
                kind: LayoutKind::Packed {
                    width,
                    range: self.layout_range(&ty, env, &mut HashSet::new()),
                    element_enum: self.array_element_enum(&family),
                    family,
                },
            };
        }

        if let Some((width, enum_name)) = self.enum_representation(&ty) {
            return SourceLayout {
                span,
                kind: LayoutKind::Scalar {
                    width,
                    domain: ScalarDomain::Enum(enum_name.clone()),
                    nominal: Some(enum_name),
                    value_range: None,
                },
            };
        }

        if let Some((width, real, value_range)) = self.ranged_numeric(&ty) {
            return SourceLayout {
                span,
                kind: LayoutKind::Scalar {
                    width,
                    domain: if real {
                        ScalarDomain::Real
                    } else {
                        ScalarDomain::Integer
                    },
                    nominal: None,
                    value_range,
                },
            };
        }

        let width = type_width(&ty, env, &self.free_fns, &self.structs, &self.const_ranges);
        let head = self.free_fns.type_head_key(&ty);
        let domain = match head.as_deref() {
            Some("real") => Some(ScalarDomain::Real),
            Some("integer") => Some(ScalarDomain::Integer),
            Some("Char") => Some(ScalarDomain::Character),
            Some(name) if struct_derives_kernel(name, "real", &self.structs, &self.free_fns) => {
                Some(ScalarDomain::Real)
            }
            Some(name) if struct_derives_kernel(name, "integer", &self.structs, &self.free_fns) => {
                Some(ScalarDomain::Integer)
            }
            Some(name) if struct_derives_kernel(name, "Char", &self.structs, &self.free_fns) => {
                Some(ScalarDomain::Character)
            }
            Some(_) if width != 0 => Some(ScalarDomain::Bits),
            _ => None,
        };
        match domain {
            Some(domain) => SourceLayout {
                span,
                kind: LayoutKind::Scalar {
                    width,
                    domain,
                    nominal: head
                        .filter(|name| !matches!(name.as_str(), "integer" | "real" | "Char")),
                    value_range: None,
                },
            },
            None => SourceLayout {
                span,
                kind: LayoutKind::Opaque {
                    name: rendered,
                    width: (width != 0).then_some(width),
                },
            },
        }
    }

    /// Apply the same concrete alias/type-parameter rules signal lowering uses
    /// before building layout metadata for nested fields.
    pub(super) fn normalize_layout_type(&self, ty: &ast::Type) -> ast::Type {
        let substituted = if self.cur_type_env.is_empty() {
            ty.clone()
        } else {
            subst_type_params(ty, &self.cur_type_env)
        };
        match &substituted {
            ast::Type::Path(path) if path.segments.len() == 1 => {
                self.resolve_alias(&substituted).clone()
            }
            ast::Type::Indexed {
                base,
                index: Some(index),
                span,
            } => match self.resolve_alias(base) {
                ast::Type::Indexed {
                    base: element,
                    index: None,
                    ..
                } => ast::Type::Indexed {
                    base: element.clone(),
                    index: Some(index.clone()),
                    span: *span,
                },
                _ => substituted,
            },
            _ => substituted,
        }
    }

    /// The vector family a type packs into, or `None` when it is not packed.
    pub(super) fn packed_family(
        &self,
        ty: &ast::Type,
        env: &HashMap<String, i64>,
    ) -> Option<String> {
        match ty {
            ast::Type::Indexed { base, .. } => {
                let name = self.free_fns.type_head_key(base)?;
                self.array_families.contains(&name).then_some(name)
            }
            ast::Type::Path(_) => {
                let name = self.free_fns.type_head_key(ty)?;
                (self.array_families.contains(&name)
                    && type_width(ty, env, &self.free_fns, &self.structs, &self.const_ranges) != 0)
                    .then_some(name)
            }
            _ => None,
        }
    }

    /// The declared index range of a type, guarding against an alias cycle.
    pub(super) fn layout_range(
        &self,
        ty: &ast::Type,
        env: &HashMap<String, i64>,
        seen: &mut HashSet<String>,
    ) -> Option<LayoutRange> {
        if let Some((left, right)) = self.declared_range(ty, env) {
            return Some(LayoutRange { left, right });
        }
        let name = self.free_fns.type_head_key(ty)?;
        if !seen.insert(name.clone()) {
            return None;
        }
        let base = self.structs.get(&name)?.base.as_ref()?;
        self.layout_range(base, env, seen)
    }

    /// The element enum of an array family, so companions can render variants.
    pub(super) fn array_element_enum(&self, family: &str) -> Option<String> {
        let mut current = family.to_string();
        let mut seen = HashSet::new();
        while seen.insert(current.clone()) {
            let base = self.structs.get(&current)?.base.as_ref()?;
            let element = match base {
                ast::Type::Indexed { base, .. } => base.as_ref(),
                ast::Type::Path(_) => base,
                _ => return None,
            };
            if let ast::Type::Path(path) = element {
                if let Some(key) = self.free_fns.enum_path_key(path) {
                    if self.enum_reprs.contains_key(&key) {
                        return Some(key);
                    }
                }
            }
            let head = self.free_fns.type_head_key(element)?;
            if self.enum_reprs.contains_key(&head) {
                return Some(head);
            }
            current = head;
        }
        None
    }

    /// The storage width of a value-range-constrained numeric type
    /// (`integer<left..right>` / `real<left..right>`), if `ty` is one. Returns
    /// `(width, is_real)`.
    pub(super) fn ranged_numeric(&self, ty: &ast::Type) -> Option<NumericRangeInfo> {
        let ast::Type::Generic { base, args, .. } = ty else {
            return None;
        };
        let ast::Type::Path(p) = base.as_ref() else {
            return None;
        };
        let kind = p.segments.last().map(|s| s.text.as_str())?;
        if kind != "integer" && kind != "real" {
            return None;
        }
        let [ast::GenericArg::Positional(arg)] = args.as_slice() else {
            return None;
        };
        if kind == "real" {
            return Some((64, true, None)); // range is a constraint, storage is f64
        }
        let (a, b) = match arg {
            ast::Expr::Range { lo, hi, .. } => (
                self.eval_const(lo, &self.cur_env)?,
                self.eval_const(hi, &self.cur_env)?,
            ),
            ast::Expr::Path(path) => self
                .free_fns
                .constant_path_key(path)
                .and_then(|key| self.const_ranges.get(&key).copied())?,
            _ => return None,
        };
        let (lo, hi) = (a.min(b), a.max(b));
        // Smallest width whose (signed, when lo < 0) domain covers [lo, hi].
        for w in 1..=64u32 {
            let fits = if lo < 0 {
                let half = 1i128 << (w - 1);
                (lo as i128) >= -half && (hi as i128) < half
            } else {
                (hi as i128) < (1i128 << w.min(63)) || w >= 64
            };
            if fits {
                return Some((w, false, Some((lo, hi))));
            }
        }
        Some((64, false, None))
    }

    /// The bit width of `ty` if it names a known enum or a fieldless nominal
    /// type whose representation ultimately derives from one.
    pub(super) fn enum_representation(&self, ty: &ast::Type) -> Option<(u32, String)> {
        let ast::Type::Path(_) = ty else {
            return None;
        };
        let mut name = self.free_fns.type_head_key(ty)?;
        let mut seen = HashSet::new();
        while seen.insert(name.clone()) {
            if let Some(width) = self.enum_reprs.get(&name) {
                return Some((*width, name));
            }
            let declaration = self.structs.get(&name)?;
            if !declaration.fields.is_empty() {
                return None;
            }
            let base = declaration.base.as_ref()?;
            // A newtype over an *array* is a derived vector, not an enum
            // wearing a new name: `struct Byte(unsigned[8])` is eight bits.
            // Walking on reached `unsigned` -> `Logic[]` -> `Logic` and took
            // the element's four, so every `Byte` signal silently truncated.
            if matches!(base, ast::Type::Indexed { .. }) {
                return None;
            }
            name = self.free_fns.type_head_key(base)?;
        }
        None
    }

    /// The `(field name, field type)` list if `ty` names a known struct —
    /// resolving generic applications (`Pair<unsigned[8]>`) and bus-mode views
    /// (`Stream::Source`, `Stream<unsigned[8]>::Source`, spec 3.19).
    /// Normalize an instance's connection args into `(port, value)` pairs:
    /// positional args (`Inv { a, b }`) bind by the sub-entity's port order,
    /// explicit args (`.clk = clk`) bind by name. Every arg carries a value, so
    /// downstream sites don't special-case the connection shape.
    pub(super) fn norm_conns(
        &self,
        conns: &[ast::ConnectArg],
        entity_id: DefId,
    ) -> Vec<(String, ast::Expr)> {
        let order: Vec<String> = self
            .entities
            .get(&entity_id)
            .map(|d| d.ports.iter().map(|p| p.name.text.clone()).collect())
            .unwrap_or_default();
        conns
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let port = match &c.field {
                    Some(f) => f.text.clone(),
                    None => order.get(i).cloned()?,
                };
                // The connected value is read against the *port's* declared
                // type, so a positional literal on a struct port
                // (`.p = { 7, 8 }`) is a struct literal, not the concat it
                // lexes as. It connected nothing and the child read zeros.
                let port_ty = self
                    .entities
                    .get(&entity_id)
                    .and_then(|d| d.ports.iter().find(|p| p.name.text == port))
                    .map(|p| p.ty.clone());
                let value = self.as_struct_literal(port_ty.as_ref(), &c.value.clone()?);
                Some((port, value))
            })
            .collect()
    }

    /// The declared fields of a struct type, or `None` when it is not one.
    pub(super) fn struct_fields(&self, ty: &ast::Type) -> Option<Vec<(String, ast::Type)>> {
        match ty {
            // A generic application: substitute the type parameters into the
            // base struct's field types.
            ast::Type::Generic { base, args, .. } => {
                let sname = self.free_fns.type_head_key(base)?;
                let s = self.structs.get(&sname)?;
                let mut subst: HashMap<String, ast::Type> = HashMap::new();
                for (index, arg) in args.iter().enumerate() {
                    let Some(ty) = (match arg {
                        ast::GenericArg::Positional(e) => expr_to_type(e),
                        ast::GenericArg::PositionalType(ty) => Some(ty.clone()),
                        ast::GenericArg::Named { value, .. } => expr_to_type(value),
                        ast::GenericArg::NamedType { ty, .. } => Some(ty.clone()),
                    }) else {
                        continue;
                    };
                    let parameter = match arg {
                        ast::GenericArg::Named { name, .. }
                        | ast::GenericArg::NamedType { name, .. } => name.text.clone(),
                        _ => s
                            .params
                            .params
                            .get(index)
                            .map(|parameter| parameter.name.text.clone())
                            .unwrap_or_default(),
                    };
                    if !parameter.is_empty() {
                        subst.insert(parameter, ty);
                    }
                }
                let fields = self.raw_struct_fields(&sname)?;
                Some(
                    fields
                        .into_iter()
                        .map(|(n, ft)| (n, subst_type_params(&ft, &subst)))
                        .collect(),
                )
            }
            // An applied view reuses the backing struct's representation.
            ast::Type::View { target, .. } => self.struct_fields(target),
            ast::Type::Path(p) => self
                .free_fns
                .struct_path_key(p)
                .and_then(|name| self.raw_struct_fields(&name)),
            _ => None,
        }
    }

    /// The base-first field list of a struct named directly (no generics/mode).
    /// Every leaf of a struct as a dotted path (`a.x`, `a.y`, `z`), descending
    /// through fields that are themselves structs. Composition makes a struct
    /// value a tree while the signal table holds only its leaves, so anything
    /// copying a whole struct value has to work in these terms.
    pub(super) fn struct_leaf_names(&self, name: &str) -> Vec<String> {
        self.struct_leaf_names_at(name, &mut HashSet::new())
    }

    /// The flattened leaf names of a struct, guarding against a recursive type.
    pub(super) fn struct_leaf_names_at(
        &self,
        name: &str,
        seen: &mut HashSet<String>,
    ) -> Vec<String> {
        if !seen.insert(name.to_string()) {
            return Vec::new();
        }
        let Some(fields) = self.raw_struct_fields(name) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (f, ty) in fields {
            let nested = self
                .free_fns
                .type_head_key(&ty)
                .map(|h| self.struct_leaf_names_at(&h, seen))
                .unwrap_or_default();
            if nested.is_empty() {
                out.push(f);
            } else {
                out.extend(nested.into_iter().map(|n| format!("{f}.{n}")));
            }
        }
        seen.remove(name);
        out
    }

    /// The struct's own declared fields, before derivation bases are merged in.
    pub(super) fn raw_struct_fields(&self, name: &str) -> Option<Vec<(String, ast::Type)>> {
        let s = self.structs.get(name)?;
        // Derived struct: inherited base fields come first (spec: derivation).
        // A cyclic derivation is reported by resolve, but lowering still runs
        // (best-effort) — and `struct_fields` calls straight back here, so
        // without this guard the pair recursed until the stack overflowed and
        // the process aborted.
        let mut fields = match &s.base {
            Some(_) if !self.expanding_structs.borrow_mut().insert(name.to_string()) => Vec::new(),
            Some(b) => {
                let inherited = self.struct_fields(b).unwrap_or_default();
                self.expanding_structs.borrow_mut().remove(name);
                inherited
            }
            None => Vec::new(),
        };
        fields.extend(s.fields.iter().map(|f| (f.name.text.clone(), f.ty.clone())));
        Some(fields)
    }

    /// The view applied to a type, if any.
    pub(super) fn view_of(&self, ty: &ast::Type) -> Option<String> {
        if let ast::Type::View { view, target, .. } = ty {
            return Some(format!(
                "{}@{}",
                self.free_fns.view_path_key(view)?,
                self.free_fns.type_head_key(target)?
            ));
        }
        let head = self.free_fns.type_head_key(ty)?;
        self.views.contains_key(&head).then_some(head)
    }
}
