//! Calls: conversion and format arity, return types, argument contracts,
//! callee resolution, and instance placement.

use super::*;

impl<'a> Checker<'a> {
    /// Compile-time fit check for conversion expressions with constant
    /// arguments: the value must be representable in the target container.
    pub(super) fn check_conversion_fit(&mut self, callee: &Expr, args: &[Expr], site: &Expr) {
        // Target family + width from the conversion callee shape.
        let width = match callee {
            Expr::Index { base, index, .. } => {
                let head = match base.as_ref() {
                    Expr::Path(path) => self.path_key(path),
                    _ => return,
                };
                if !head.is_some_and(|key| self.array_families.contains(&key)) {
                    return;
                }
                match signed_lit(index) {
                    Some(w) => w,
                    None => return,
                }
            }
            Expr::Path(p) if p.segments.last().is_some_and(|name| name.text == "resize") => {
                match args.get(1).and_then(signed_lit) {
                    Some(w) => w,
                    None => return,
                }
            }
            _ => return,
        };
        if !(1..=64).contains(&width) {
            return;
        }
        /// Fold a constant integer expression, or `None` when it is not constant.
        fn const_fold(e: &Expr) -> Option<i64> {
            match e {
                Expr::Binary { op, lhs, rhs, .. } => {
                    let (a, b) = (const_fold(lhs)?, const_fold(rhs)?);
                    match op {
                        BinOp::Add => a.checked_add(b),
                        BinOp::Sub => a.checked_sub(b),
                        BinOp::Mul => a.checked_mul(b),
                        BinOp::Div => a.checked_div(b),
                        _ => None,
                    }
                }
                _ => signed_lit(e),
            }
        }
        let Some(v) = args.first().and_then(const_fold) else {
            return;
        };
        self.check_fits_width(v, width as u32, expr_span(site));
    }

    /// `T()` is explicit default construction and `T(value)` is conversion;
    /// no type constructor accepts more than one value. Several lowerers read
    /// only `args.first()`, so extra arguments otherwise vanished silently.
    pub(super) fn check_conversion_arity(&mut self, callee: &Expr, args: &[Expr]) {
        if args.len() <= 1 {
            return;
        }
        let is_conversion = match callee {
            Expr::Path(path) => {
                let Some(name) = path.segments.last().map(|segment| &segment.text) else {
                    return;
                };
                let associated = self
                    .associated_owner_key(path)
                    .is_some_and(|owner| self.methods.contains_key(&(owner, name.clone())));
                let target = self.path_key(path).unwrap_or_else(|| name.clone());
                !associated && self.function_id(path).is_none() && self.is_conversion_name(&target)
            }
            Expr::Index { base, .. } => {
                matches!(base.as_ref(), Expr::Path(path) if self.path_key(path)
                    .is_some_and(|name| self.is_conversion_name(&name)))
            }
            _ => false,
        };
        if is_conversion {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(callee),
                format!(
                    "a type constructor takes zero or one argument, but {} were given",
                    args.len()
                ),
            );
        }
    }

    /// `print!("{} {}", x)` silently rendered an empty slot, and a spare
    /// argument was silently dropped — in a testbench that is exactly where a
    /// wrong value costs you debugging time. Both engines share the arity, so
    /// checking it here covers them at once.
    pub(super) fn check_format_arity(&mut self, callee: &Expr, args: &[Expr]) {
        let name = match callee {
            Expr::Path(p) if p.segments.len() == 1 => p.segments[0].text.as_str(),
            _ => return,
        };
        // `assert!`/`warn!(cond, "msg", args..)` put the format string second.
        let fmt_at = match name {
            "print" => 0,
            "assert" | "warn" => 1,
            _ => return,
        };
        let Some(Expr::StrLit { text, span }) = args.get(fmt_at) else {
            return;
        };
        let want = crate::syntax::format::arity(text);
        let have = args.len().saturating_sub(fmt_at + 1);
        if want != have {
            self.error(
                codes::TYPE_MISMATCH,
                *span,
                format!("format string takes {want} argument(s) but {have} were given"),
            );
        }
    }

    /// Render a type for a diagnostic, resolving a named struct/enum/entity to
    /// its declared name. The free `ty_name` has no definition table, so it can
    /// only say "a named type" — never show that to a user.
    pub(super) fn ty_display(&self, t: &Ty) -> String {
        match t {
            Ty::Named(id) => self
                .resolved
                .def(*id)
                .map(|d| d.name.clone())
                .unwrap_or_else(|| ty_name(t)),
            Ty::Array {
                elem: _,
                len,
                family: Some(name),
            } => format!("{}[{len}]", self.key_leaf(name)),
            Ty::Array {
                elem,
                len,
                family: None,
            } => format!("{}[{len}]", self.ty_display(elem)),
            _ => ty_name(t),
        }
    }

    /// Resolve a declared free-function return at its call site. Concrete
    /// returns are direct; a bare generic return (`-> T`) takes the type
    /// inferred from the corresponding value parameter.
    pub(super) fn free_call_return_type(
        &self,
        path: &Path,
        args: &[Expr],
        sym: &HashMap<String, Ty>,
    ) -> Ty {
        let name = path
            .segments
            .last()
            .map(|segment| segment.text.as_str())
            .unwrap_or("");
        let Some(function) = self.function_id(path) else {
            return self.runtime_call_return_type(name);
        };
        let Some(declared) = self.fn_return_types.get(&function) else {
            return self.runtime_call_return_type(name);
        };
        let Some(declared) = declared else {
            return Ty::Void;
        };
        if let Type::Path(path) = declared {
            if path.segments.len() == 1 {
                let parameter_name = &path.segments[0].text;
                if let Some((generics, params)) = self.generic_fns.get(&function) {
                    let is_generic = generics
                        .iter()
                        .any(|parameter| parameter.name.text == *parameter_name);
                    if is_generic {
                        return params
                            .iter()
                            .zip(args)
                            .find_map(|(parameter, argument)| {
                                parameter
                                    .as_ref()
                                    .filter(|ty| Self::is_direct_type_param(ty, parameter_name))
                                    .map(|_| self.type_of(argument, sym))
                            })
                            .unwrap_or(Ty::Error);
                    }
                }
            }
        }
        self.ast_ty(declared)
    }

    /// Return contracts for compiler/runtime-provided functions which have no
    /// source `FnDecl`. Context-sized file reads deliberately remain unknown;
    /// their initializer target determines the result layout.
    pub(super) fn runtime_call_return_type(&self, name: &str) -> Ty {
        match name {
            "rand" | "randint" => Ty::Integer,
            "uniform" => Ty::Real,
            "exists" => self.ty_from_head("Bool"),
            "seed" | "print" | "assert" | "warn" | "await" | "wait" | "tick" | "clock" | "stop"
            | "finish" => Ty::Void,
            "read" | "resize" => Ty::Error,
            _ => Ty::Error,
        }
    }

    /// A call to a *declared* function must pass one argument per parameter.
    /// Nothing checked this: a short call left a parameter unbound, and a short
    /// `extern "C"` call passed a garbage argument to real native code.
    /// Conversions, method calls and runtime-provided std functions have no
    /// declaration here and are skipped.
    pub(super) fn check_call_arity(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        sym: &HashMap<String, Ty>,
    ) {
        let Expr::Path(p) = callee else { return };
        let Some(name) = p.segments.last() else {
            return;
        };
        if let Some(owner) = self.associated_owner_key(p) {
            if self
                .methods
                .contains_key(&(owner.clone(), name.text.clone()))
                || (name.text == "new" && self.is_conversion_name(&owner))
            {
                return;
            }
        }
        let Some(function) = self.function_id(p) else {
            // No declaration here is normal for a conversion (`unsigned[8](x)`,
            // `Logic(b)`) and for the runtime-provided std functions, and a
            // mistake for anything else — but the two were indistinguishable,
            // so a misspelled or unimported call passed every stage and failed
            // in the backend as "unsupported call `abs` in testbench
            // expression", blaming the emitter for a missing `using`.
            let callee_key = self.path_key(p).unwrap_or_else(|| name.text.clone());
            if !self.callee_is_declared(&callee_key) {
                self.error_with_help(
                    codes::UNKNOWN_NAME,
                    expr_span(callee),
                    format!("unknown function `{}`", name.text),
                    "declare it, or import it with `using` — a std function needs \
                     its module (`using std::math::{abs};`)"
                        .to_string(),
                );
            }
            return;
        };
        let Some(&want) = self.fn_arity.get(&function) else {
            return;
        };
        if args.len() != want {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(callee),
                format!(
                    "`{}` takes {want} argument(s) but {} were given",
                    name.text,
                    args.len()
                ),
            );
            return;
        }
        self.check_call_arg_types(function, &name.text, args, sym);
    }

    /// Each argument must be assignable to its parameter, by the same rule an
    /// assignment uses. Only the count was checked, so a `real` handed to an
    /// `integer` parameter passed its f64 bits through as an integer, and a
    /// `signed[8]` passed its raw bit pattern — `abs(-5)` returned 251.
    pub(super) fn check_call_arg_types(
        &mut self,
        function: DefId,
        name: &str,
        args: &[Expr],
        sym: &HashMap<String, Ty>,
    ) {
        let Some(params) = self.fn_param_types.get(&function).cloned() else {
            return;
        };
        for (arg, pty) in args.iter().zip(params.iter()) {
            let Some(pty) = pty else { continue };
            let want = self.ast_ty(pty);
            if self.check_struct_literal_for_ty(&want, arg, sym) {
                continue;
            }
            if want == Ty::Error || self.assignable(&want, arg, sym) {
                continue;
            }
            let got = self.type_of(arg, sym);
            if got == Ty::Error {
                continue;
            }
            self.error_with_help(
                codes::TYPE_MISMATCH,
                expr_span(arg),
                format!(
                    "cannot pass {} to a {} parameter of `{name}` without an explicit conversion",
                    self.ty_display(&got),
                    self.ty_display(&want)
                ),
                format!(
                    "wrap it in a conversion, e.g. `{}(...)`",
                    self.ty_display(&want)
                ),
            );
        }
    }

    /// Compiler/runtime-provided functions have no source `FnDecl`, so retain
    /// their complete public contract here: arity, argument domains, macro
    /// spelling, and removed migration forms. This keeps malformed calls from
    /// surviving until C harness generation.
    pub(super) fn check_runtime_call_contract(
        &mut self,
        callee: &Expr,
        type_args: &[Type],
        args: &[Expr],
        bang: bool,
        sym: &HashMap<String, Ty>,
    ) {
        let Expr::Path(path) = callee else { return };
        let Some(name) = path.segments.last() else {
            return;
        };
        if let Some(owner) = self.associated_owner_key(path) {
            if self
                .methods
                .contains_key(&(owner.clone(), name.text.clone()))
                || (name.text == "new" && self.is_conversion_name(&owner))
            {
                return;
            }
        }
        let function = name.text.as_str();
        // A source declaration shadows a runtime primitive with the same
        // leaf name (`fn read<Bus>(bus)` is common in protocol traits).
        if self.function_id(path).is_some() {
            if !type_args.is_empty() {
                self.error(
                    codes::TYPE_MISMATCH,
                    expr_span(callee),
                    "explicit call type arguments are currently reserved for `read<T>`".to_string(),
                );
            }
            return;
        }
        if function == "read" {
            if type_args.len() != 1 {
                self.error_with_help(
                    codes::TYPE_MISMATCH,
                    expr_span(callee),
                    "`read` needs exactly one constructed type".to_string(),
                    "write `read<string>(\"text.txt\")` for UTF-8 or `read<integer>(\"data.bin\")` for binary"
                        .to_string(),
                );
            } else {
                let requested = self.ast_ty(&type_args[0]);
                let supported = matches!(requested, Ty::Integer)
                    || matches!(
                        requested,
                        Ty::Array {
                            ref elem,
                            family: None,
                            ..
                        } if matches!(elem.as_ref(), Ty::Char)
                    )
                    || matches!(
                        requested,
                        Ty::Array {
                            len,
                            family: Some(_),
                            ..
                        } if len > 0
                    );
                if !supported {
                    self.error(
                        codes::TYPE_MISMATCH,
                        type_head_span(&type_args[0]).unwrap_or(expr_span(callee)),
                        "`read<T>` needs `string`, `integer`, or a sized packed numeric type constructible from `integer`"
                            .to_string(),
                    );
                }
            }
        } else if !type_args.is_empty() {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(callee),
                "explicit call type arguments are currently reserved for `read<T>`".to_string(),
            );
        }
        if matches!(function, "tick" | "clock") {
            let replacement = if function == "tick" {
                "drive the clock and `await` each half-period explicitly"
            } else {
                "write `clk = not clk after <half-period>;`"
            };
            self.error_with_help(
                codes::UNKNOWN_NAME,
                expr_span(callee),
                format!("`{function}()` was removed"),
                replacement.to_string(),
            );
            return;
        }

        let exact = match function {
            "rand" | "uniform" | "stop" | "finish" => Some(0),
            "seed" | "exists" | "read" | "await" => Some(1),
            "randint" | "resize" => Some(2),
            _ => None,
        };
        if let Some(expected) = exact {
            if args.len() != expected {
                self.error(
                    codes::TYPE_MISMATCH,
                    expr_span(callee),
                    format!(
                        "`{function}` takes {expected} argument(s) but {} were given",
                        args.len()
                    ),
                );
                return;
            }
        } else if matches!(function, "print" | "assert" | "warn") && args.is_empty() {
            self.error(
                codes::TYPE_MISMATCH,
                expr_span(callee),
                format!("`{function}!` needs at least one argument"),
            );
            return;
        } else if !matches!(
            function,
            "print"
                | "assert"
                | "warn"
                | "rand"
                | "uniform"
                | "stop"
                | "finish"
                | "seed"
                | "exists"
                | "read"
                | "await"
                | "randint"
                | "resize"
        ) {
            return;
        }

        if matches!(function, "print" | "assert" | "warn") && !bang {
            self.error_with_help(
                codes::TYPE_MISMATCH,
                expr_span(callee),
                format!("`{function}` is a macro-like compiler primitive"),
                format!("write `{function}!(...)`"),
            );
            return;
        }

        match function {
            "seed" | "randint" => {
                for argument in args {
                    if !self.assignable(&Ty::Integer, argument, sym) {
                        self.error(
                            codes::TYPE_MISMATCH,
                            expr_span(argument),
                            format!("`{function}` expects integer argument(s)"),
                        );
                    }
                }
            }
            "exists" | "read" => {
                let Some(argument) = args.first() else { return };
                if !matches!(argument, Expr::StrLit { .. }) {
                    self.error_with_help(
                        codes::TYPE_MISMATCH,
                        expr_span(argument),
                        format!("`{function}` needs a literal file path"),
                        format!("write `{function}(\"path/to/file\")`"),
                    );
                }
            }
            "print" => {
                let Some(format) = args.first() else { return };
                if !matches!(format, Expr::StrLit { .. }) {
                    self.error(
                        codes::TYPE_MISMATCH,
                        expr_span(format),
                        "`print!` needs a string-literal format".to_string(),
                    );
                }
            }
            "assert" | "warn" => {
                let Some(condition) = args.first() else {
                    return;
                };
                self.check_condition(condition, sym);
                if let Some(message) = args.get(1) {
                    if !matches!(message, Expr::StrLit { .. }) {
                        self.error(
                            codes::TYPE_MISMATCH,
                            expr_span(message),
                            format!("`{function}!` message must be a string literal"),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    /// Whether a bare call name is something the language declares somewhere:
    /// a `fn`, a type used as a conversion, or one of the runtime-provided std
    /// functions that never reach `fn_arity`. Anything else does not exist.
    pub(super) fn callee_is_declared(&self, name: &str) -> bool {
        // Runtime-provided (std::rand, std::fs) — declared by the runtime, not
        // by a `fn`. Kept beside `check_runtime_call_arity`'s list.
        if matches!(
            name,
            "rand" | "uniform" | "randint" | "seed" | "exists" | "read"
        ) {
            return true;
        }
        // Primitives the compiler implements rather than std declaring: the
        // stimulus and reporting forms, simulation control (`stop`/`finish`,
        // spec 3.24), and the width builtin. Kept in step with the emitter's
        // own match — anything it handles by name belongs here.
        if matches!(
            name,
            "print"
                | "assert"
                | "warn"
                | "await"
                | "wait"
                | "tick"
                | "clock"
                | "stop"
                | "finish"
                | "resize"
        ) {
            return true;
        }
        // A conversion names a value type. Entities are instantiated with a
        // struct literal and are never callable values.
        if self.is_conversion_name(name) {
            return true;
        }
        // A method reached through UFCS-ish sugar, or a trait method the
        // receiver supplies: if any type implements a method of this name it
        // is not an unknown *function*.
        self.methods.keys().any(|(_, m)| m == name)
    }

    /// The definition a call's path names, when it resolves to a function.
    pub(super) fn function_id(&self, path: &Path) -> Option<DefId> {
        self.resolved
            .resolved(path.span)
            .filter(|id| self.resolved.kind_of(*id) == Some(DefKind::Fn))
    }

    /// Whether `name` is one of the built-in conversions rather than an ordinary
    /// call.
    pub(super) fn is_conversion_name(&self, name: &str) -> bool {
        if matches!(name, "integer" | "real" | "Char" | "string")
            || self.is_array_family(name)
            || self.structs.contains_key(name)
            || self.enum_variants.contains_key(name)
        {
            return true;
        }
        let Some(alias) = self.aliases.get(name) else {
            return false;
        };
        self.resolve_alias_type(alias)
            .as_ref()
            .and_then(|ty| self.type_key(ty))
            .is_some_and(|head| !self.entities.contains_key(&head))
    }

    /// The write restrictions `self` carries inside an impl on a view: each
    /// leaf the role declares `in` is an input there, so `self.<leaf>` cannot
    /// be driven. Empty for any other impl target, where `self` is plain data.
    pub(super) fn self_view_dirs(&self, target: &Type) -> PortDirs {
        let mut dirs = PortDirs::default();
        let Some(key) = self.type_key(target) else {
            return dirs;
        };
        let Some(leaves) = self.view_dirs.get(&key) else {
            return dirs;
        };
        for (field, dir) in leaves {
            if *dir == Direction::In {
                dirs.illegal.insert(format!("self.{field}"));
            }
        }
        dirs
    }

    /// Bring `names` into scope as generic parameters, returning the previous
    /// set for the caller to restore.
    pub(super) fn push_type_params<'n>(
        &self,
        names: impl Iterator<Item = &'n String>,
    ) -> HashSet<String> {
        let mut scope = self.type_params.borrow_mut();
        let saved = scope.clone();
        scope.extend(names.cloned());
        saved
    }

    /// An entity may be instantiated only at the root layer of another
    /// entity's body, or inside a generate `for`/`if`. A `match` arm and a
    /// function body are neither.
    ///
    /// Both used to be accepted and then quietly dropped: elaboration gathers
    /// instances from the root, from a generate-`for` and from a generate-`if`
    /// and from nothing else, so an instance in a `match` arm simply never
    /// existed — the design compiled and ran without it. A function was worse,
    /// failing much later with "the driver for `y` contains an Unknown", which
    /// names neither the function nor the instantiation.
    ///
    /// Instantiating from a function would also mean a function could bring a
    /// process into being, which only an entity may do.
    pub(super) fn check_instance_placement(&mut self, l: &LetDecl) {
        let Some(head) = l.ty.as_ref().and_then(|ty| self.type_key(ty)) else {
            return;
        };
        if !self.entities.contains_key(&head)
            || self.type_params.borrow().contains(self.key_leaf(&head))
        {
            return;
        }
        let context = if self.in_fn_body.get() {
            "a function"
        } else if self.in_match_arm.get() {
            "a `match` arm"
        } else {
            return;
        };
        self.error_with_help(
            codes::INSTANCE_PLACEMENT,
            l.span,
            format!("an entity cannot be instantiated in {context}"),
            "hardware is structural: instantiate at the top of an entity body, \
             or inside a generate `for`/`if` whose condition folds to a \
             constant. A `match` on a signal selects a value at run time, and \
             cannot bring an instance into being"
                .to_string(),
        );
    }

    /// Whether `t` names an entity — i.e. an array of it is an *instance*
    /// array, which is always declared with a plain element count.
    pub(super) fn is_entity_ty(&self, t: &Ty) -> bool {
        matches!(t, Ty::Named(id)
            if self.resolved.def(*id).map(|d| d.kind) == Some(DefKind::Entity))
    }
}
