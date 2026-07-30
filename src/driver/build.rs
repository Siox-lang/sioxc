//! `siox build` — compile a design + its `#[test]` stimulus into a standalone
//! native simulator binary (stage B5.1).
//!
//! The DUT lowers to a native object (`sx_*` C ABI) via the LLVM backend; the
//! testbench statements are translated to a C `main` that drives it. clang
//! links them into an executable that runs *every* `#[test]` (one C function
//! per test, a libtest-style `main`) and reports results + exit code. All
//! tests share the one lowered Design (one `sx_*` namespace); `sx_reset`
//! zeroes state between them. Testbench locals use the same flattened,
//! arbitrary-width scalar representation as elaborated signals.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::process::Command;

use siox::elab::{Hierarchy, InstanceId};
use siox::ir::{Design, SignalId};
use siox::syntax::ast;
use siox::syntax::Module;

type NativeOperatorImpls<'a> = HashMap<(String, String), Vec<(&'a ast::FnDecl, Option<String>)>>;
type StructFields = Vec<(String, ast::Type)>;
type RawStructLayouts = HashMap<String, (Option<String>, StructFields)>;

/// Build a native simulator binary that runs *all* `#[test]` entities, like
/// rustc's test harness. Every test's DUT is in the one lowered `Design` (one
/// `sx_*` namespace); `sx_reset` zeroes all state, so tests run sequentially
/// in the same object.
pub fn build(
    modules: &[Module],
    hier: &Hierarchy,
    design: &Design,
    out: &Path,
) -> Result<(), String> {
    let issues = design.validate();
    if !issues.is_empty() {
        return Err(issues.join("; "));
    }

    let tests: Vec<InstanceId> = hier
        .roots
        .iter()
        .copied()
        .filter(|&r| is_test_entity(modules, &hier.instance(r).entity))
        .collect();
    if tests.is_empty() {
        return Err("no #[test] entity to build a test binary from".into());
    }
    let enums = siox::ir::enum_discriminants(modules);
    let families = siox::ir::vector_families(modules);
    let mut op_impls: NativeOperatorImpls<'_> = HashMap::new();
    for m in modules {
        for item in &m.items {
            if let ast::Item::Impl(im) = item {
                let tr = im.trait_.as_ref().and_then(|t| t.segments.last());
                if let (Some(tr), Some(ty)) = (tr, type_head_name(&im.target)) {
                    let operator = if tr.text == "Operator" {
                        im.trait_args.first().and_then(|a| match a {
                            ast::GenericArg::Positional(ast::Expr::StrLit { text, .. }) => {
                                Some(text.clone())
                            }
                            _ => None,
                        })
                    } else {
                        Some(tr.text.clone())
                    };
                    let Some(operator) = operator else { continue };
                    let input_index = usize::from(tr.text == "Operator");
                    let input = im.trait_args.get(input_index).and_then(|a| match a {
                        ast::GenericArg::Positional(ast::Expr::Path(p)) => {
                            p.segments.last().map(|s| s.text.clone())
                        }
                        _ => None,
                    });
                    for it in &im.items {
                        if let ast::ImplItem::Fn(f) = it {
                            let input = input.clone().or_else(|| {
                                f.params
                                    .iter()
                                    .find(|p| !p.is_self)
                                    .and_then(|p| p.ty.as_ref())
                                    .and_then(type_head_name)
                                    .map(str::to_string)
                            });
                            op_impls
                                .entry((operator.clone(), ty.to_string()))
                                .or_default()
                                .push((f, input));
                        }
                    }
                }
            }
        }
    }
    let type_aliases: HashMap<String, ast::Type> = modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            ast::Item::Using(ast::Using {
                kind: ast::UsingKind::Alias { name, ty },
                ..
            }) => Some((name.text.clone(), ty.clone())),
            _ => None,
        })
        .collect();
    let mut fns: HashMap<String, &ast::FnDecl> = HashMap::new();
    let mut extern_fns = std::collections::HashSet::new();
    for m in modules {
        for item in &m.items {
            match item {
                ast::Item::Fn(f) => {
                    fns.insert(f.name.text.clone(), f);
                }
                ast::Item::ExternBlock {
                    abi, fns: block, ..
                } if abi == "C" => {
                    for f in block {
                        extern_fns.insert(f.name.text.clone());
                        fns.insert(f.name.text.clone(), f);
                    }
                }
                // A *static* associated fn (no `self`) is callable as
                // `Type::name(..)`, keyed like a module-level fn.
                ast::Item::Impl(im) => {
                    let Some(ty) = type_head_name(&im.target) else {
                        continue;
                    };
                    for it in &im.items {
                        if let ast::ImplItem::Fn(f) = it {
                            if !f.params.iter().any(|p| p.is_self) {
                                fns.insert(format!("{ty}::{}", f.name.text), f);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    // Module consts (LOW/HIGH, user consts), to a fixpoint so order-independent.
    let const_decls: Vec<&ast::ConstDecl> = modules
        .iter()
        .flat_map(|m| &m.items)
        .filter_map(|it| match it {
            ast::Item::Const(c) => Some(c),
            _ => None,
        })
        .collect();
    let real_consts: std::collections::HashSet<String> = const_decls
        .iter()
        .filter(|declaration| resolved_type_name(&declaration.ty, &type_aliases) == Some("real"))
        .map(|declaration| declaration.name.text.clone())
        .collect();
    let integer_consts: std::collections::HashSet<String> = const_decls
        .iter()
        .filter(|declaration| resolved_type_name(&declaration.ty, &type_aliases) == Some("integer"))
        .map(|declaration| declaration.name.text.clone())
        .collect();
    let const_ranges: HashMap<String, (i64, i64)> = const_decls
        .iter()
        .filter(|declaration| type_head_name(&declaration.ty) == Some("range"))
        .filter_map(|declaration| {
            let ast::Expr::Range { lo, hi, .. } = &declaration.value else {
                return None;
            };
            Some((
                declaration.name.text.clone(),
                (signed_index_bound(lo)?, signed_index_bound(hi)?),
            ))
        })
        .collect();
    let mut consts: HashMap<String, u128> = HashMap::new();
    for _ in 0..=const_decls.len() {
        let mut progressed = false;
        for c in &const_decls {
            if consts.contains_key(&c.name.text) {
                continue;
            }
            if let Some(v) = eval_c_const(&c.value, &consts, &enums, &fns) {
                consts.insert(c.name.text.clone(), v);
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    // Preserve the source expression separately from the narrow evaluator.
    // `_BitInt` accepts the resulting arbitrary-width C expression directly,
    // so testbench references never inherit the u128 helper's ceiling.
    let mut const_exprs: HashMap<String, String> = HashMap::new();
    for _ in 0..=const_decls.len() {
        let mut progressed = false;
        for declaration in &const_decls {
            if const_exprs.contains_key(&declaration.name.text) {
                continue;
            }
            let expression = if real_consts.contains(&declaration.name.text) {
                emit_c_real_const(&declaration.value, &const_exprs, &real_consts, &enums)
                    .map(|value| format!("sx_b64((double)({value}))"))
            } else {
                emit_c_const(&declaration.value, &const_exprs, &enums)
            };
            if let Some(expression) = expression {
                const_exprs.insert(declaration.name.text.clone(), expression);
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    // Constant lookup tables, one emitted expression per element. The scalar
    // table above holds a single entry per name, so an indexed read of a
    // `const` array found nothing there and was called untranslatable.
    let mut const_array_exprs: HashMap<String, Vec<String>> = HashMap::new();
    for declaration in &const_decls {
        let ast::Expr::Array { elems, .. } = &declaration.value else {
            continue;
        };
        let values: Option<Vec<String>> = elems
            .iter()
            .map(|e| emit_c_const(e, &const_exprs, &enums))
            .collect();
        if let Some(values) = values {
            const_array_exprs.insert(declaration.name.text.clone(), values);
        }
    }

    // Struct field layouts (base-first, inheritance flattened) so a struct-typed
    // testbench local can be materialized as one C local per leaf field. Every
    // impl method by (type head, name), for `recv.method(args)` in stimulus.
    let structs = collect_structs(modules);
    let methods = collect_methods(modules);
    let derived_widths = siox::ir::derived_widths(modules);

    // Header, one `signed test_<name>(void)` per test, then a libtest-style main.
    let mut prog = String::new();
    prog.push_str("#include <stdint.h>\n#include <stdio.h>\n#include <string.h>\n");
    for name in &extern_fns {
        let f = fns[name];
        let ret = f
            .ret
            .as_ref()
            .map_or("void", |ty| extern_c_type(Some(ty), &type_aliases));
        let params = f
            .params
            .iter()
            .filter(|param| !param.is_self)
            .map(|param| extern_c_type(param.ty.as_ref(), &type_aliases))
            .collect::<Vec<_>>()
            .join(", ");
        prog.push_str(&format!(
            "extern {ret} {name}({});\n",
            if params.is_empty() { "void" } else { &params }
        ));
    }
    prog.push_str("extern void sx_reset(void);\n");
    prog.push_str("extern void sx_set_word(uint32_t, uint32_t, uint64_t);\n");
    prog.push_str("extern uint64_t sx_read_word(uint32_t, uint32_t);\n");
    prog.push_str("extern uint32_t sx_range_error(void);\n");
    prog.push_str("static signed sx_check_ranges(void);\n");
    let abi_words = design
        .signals
        .iter()
        .map(|signal| siox::llvm::words_for(signal.width).to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let value_bits = design
        .signals
        .iter()
        .map(|signal| signal.width)
        .max()
        .unwrap_or(1)
        .max(max_literal_type_width(modules, &derived_widths))
        .max(siox::llvm::ABI_WORD_BITS);
    // ceil(bits * log10(2)) + sign/NUL slack, using a conservative integer
    // approximation so formatting storage scales with the actual value type.
    let decimal_capacity = u64::from(value_bits)
        .saturating_mul(30_103)
        .div_ceil(100_000)
        .saturating_add(2);
    prog.push_str(&format!(
        "typedef unsigned _BitInt({value_bits}) sx_value;\n\
         #define SX_DECIMAL_CAP {decimal_capacity}\n\
         static const uint32_t sx_nwords[] = {{{abi_words}}};\n\
         static void sx_set(uint32_t s, sx_value v) {{\n\
         \x20   for (uint32_t i = 0; i < sx_nwords[s]; ++i)\n\
         \x20       sx_set_word(s, i, (uint64_t)(v >> (i * 64)));\n\
         }}\n\
         static sx_value sx_read(uint32_t s) {{\n\
         \x20   sx_value v = 0;\n\
         \x20   for (uint32_t i = 0; i < sx_nwords[s]; ++i)\n\
         \x20       v |= (sx_value)sx_read_word(s, i) << (i * 64);\n\
         \x20   return v;\n\
         }}\n\
         static sx_value sx_udiv(sx_value lhs, sx_value rhs) {{\n\
         \x20   return rhs == 0 ? 0 : lhs / rhs;\n\
         }}\n\
         static sx_value sx_idiv(int64_t lhs, int64_t rhs) {{\n\
         \x20   if (rhs == 0) return 0;\n\
         \x20   if (lhs == INT64_MIN && rhs == -1) return (sx_value)(uint64_t)INT64_MIN;\n\
         \x20   return (sx_value)(uint64_t)(lhs / rhs);\n\
         }}\n\
         static sx_value sx_shl(sx_value lhs, sx_value rhs) {{\n\
         \x20   return rhs >= {value_bits} ? 0 : lhs << rhs;\n\
         }}\n\
         static sx_value sx_shr(sx_value lhs, sx_value rhs) {{\n\
         \x20   return rhs >= {value_bits} ? 0 : lhs >> rhs;\n\
         }}\n\
         static sx_value sx_ishr(int64_t lhs, sx_value rhs) {{\n\
         \x20   return rhs >= 64 ? (lhs < 0 ? (sx_value)-1 : 0) \
         : (sx_value)(uint64_t)(lhs >> rhs);\n\
         }}\n\
         static int64_t sx_i64(sx_value value, uint32_t width) {{\n\
         \x20   uint64_t low = (uint64_t)value;\n\
         \x20   if (width == 0 || width >= 64) return (int64_t)low;\n\
         \x20   uint64_t mask = (UINT64_C(1) << width) - 1, sign = UINT64_C(1) << (width - 1);\n\
         \x20   low &= mask;\n\
         \x20   return (int64_t)((low ^ sign) - sign);\n\
         }}\n\
         static sx_value sx_mask(sx_value value, uint32_t width) {{\n\
         \x20   return width >= {value_bits} ? value : value & (((sx_value)1 << width) - 1);\n\
         }}\n\
         static char *sx_decimal(sx_value value, char *buffer) {{\n\
         \x20   char *end = buffer + SX_DECIMAL_CAP, *cursor = end;\n\
         \x20   *--cursor = '\\0';\n\
         \x20   do {{ *--cursor = (char)('0' + value % 10); value /= 10; }} while (value);\n\
         \x20   memmove(buffer, cursor, (size_t)(end - cursor));\n\
         \x20   return buffer;\n\
         }}\n\
         static char *sx_utf8(sx_value raw, char buffer[5]) {{\n\
         \x20   uint32_t value = (uint32_t)raw;\n\
         \x20   if (value <= 0x7f) {{ buffer[0] = (char)value; buffer[1] = 0; }}\n\
         \x20   else if (value <= 0x7ff) {{ buffer[0] = (char)(0xc0 | value >> 6); \
         buffer[1] = (char)(0x80 | (value & 0x3f)); buffer[2] = 0; }}\n\
         \x20   else if (value <= 0xffff && !(value >= 0xd800 && value <= 0xdfff)) {{ \
         buffer[0] = (char)(0xe0 | value >> 12); buffer[1] = (char)(0x80 | ((value >> 6) & 0x3f)); \
         buffer[2] = (char)(0x80 | (value & 0x3f)); buffer[3] = 0; }}\n\
         \x20   else if (value <= 0x10ffff) {{ buffer[0] = (char)(0xf0 | value >> 18); \
         buffer[1] = (char)(0x80 | ((value >> 12) & 0x3f)); \
         buffer[2] = (char)(0x80 | ((value >> 6) & 0x3f)); \
         buffer[3] = (char)(0x80 | (value & 0x3f)); buffer[4] = 0; }}\n\
         \x20   else {{ buffer[0] = '?'; buffer[1] = 0; }}\n\
         \x20   return buffer;\n\
         }}\n\
         static char *sx_chars(const sx_value *values, size_t length, char *buffer) {{\n\
         \x20   char *cursor = buffer;\n\
         \x20   for (size_t i = 0; i < length; ++i) {{\n\
         \x20       char encoded[5];\n\
         \x20       sx_utf8(values[i], encoded);\n\
         \x20       size_t bytes = strlen(encoded);\n\
         \x20       memcpy(cursor, encoded, bytes);\n\
         \x20       cursor += bytes;\n\
         \x20   }}\n\
         \x20   *cursor = '\\0';\n\
         \x20   return buffer;\n\
         }}\n"
    ));
    prog.push_str("extern void sx_settle(void);\n");
    prog.push_str("static const char *g_msg;\nstatic signed g_range_failed;\n");
    prog.push_str("static signed g_warnings;\n");
    prog.push_str("static double sx_f64(uint64_t b) { double d; memcpy(&d, &b, 8); return d; }\n");
    prog.push_str(
        "static uint64_t sx_b64(double d) { uint64_t b; memcpy(&b, &d, 8); return b; }\n",
    );
    // xorshift64* with the runner's constants: identical random sequences.
    prog.push_str(
        "static uint64_t g_rand = 0x9E3779B97F4A7C15ULL;\n\
         static uint64_t sx_rand(void) {\n\
         \x20   g_rand ^= g_rand >> 12; g_rand ^= g_rand << 25; g_rand ^= g_rand >> 27;\n\
         \x20   return g_rand * 0x2545F4914F6CDD1DULL;\n}\n",
    );
    prog.push_str(&format!(
        "static sx_value sx_random_value(void) {{\n\
         \x20   sx_value value = 0;\n\
         \x20   for (unsigned bit = 0; bit < {value_bits}; bit += 64)\n\
         \x20       value |= (sx_value)sx_rand() << bit;\n\
         \x20   return value;\n\
         }}\n\
         static sx_value sx_randint(sx_value left, sx_value right) {{\n\
         \x20   if (right < left) {{ sx_value swap = left; left = right; right = swap; }}\n\
         \x20   sx_value span = right - left;\n\
         \x20   if (right <= (sx_value)UINT64_MAX) {{\n\
         \x20       if (span == (sx_value)UINT64_MAX) return left + (sx_value)sx_rand();\n\
         \x20       uint64_t range = (uint64_t)span + 1, threshold = (0 - range) % range, draw;\n\
         \x20       do {{ draw = sx_rand(); }} while (draw < threshold);\n\
         \x20       return left + (sx_value)(draw % range);\n\
         \x20   }}\n\
         \x20   if (span == ~(sx_value)0) return sx_random_value();\n\
         \x20   sx_value range = span + 1, threshold = (0 - range) % range, draw;\n\
         \x20   do {{ draw = sx_random_value(); }} while (draw < threshold);\n\
         \x20   return left + draw % range;\n\
         }}\n"
    ));
    // `uniform()` — the runner's exact expression, so both engines agree.
    prog.push_str(
        "static uint64_t sx_uniform(void) {\n\
         \x20   double d = (double)(sx_rand() >> 11) / (double)(1ULL << 53);\n\
         \x20   return sx_b64(d);\n}\n",
    );
    prog.push_str(&gen_vcd_runtime(design));
    // The event wheel: earliest pending clock edge, and one step of the
    // scheduler (advance to that edge, toggle the due clocks, settle).
    prog.push_str(
        "static uint64_t sx_next_edge(const uint64_t *next, signed n) {\n\
         \x20   uint64_t t = UINT64_MAX;\n\
         \x20   for (signed i = 0; i < n; i++) if (next[i] < t) t = next[i];\n\
         \x20   return t;\n}\n\
         static signed sx_step_clock(uint64_t *now, uint64_t *next, const uint32_t *cid,\n\
         \x20                        const uint64_t *half, signed n) {\n\
         \x20   uint64_t t = sx_next_edge(next, n);\n\
         \x20   if (t == UINT64_MAX) return 0;\n\
         \x20   if (t > *now) *now = t;\n\
         \x20   for (signed i = 0; i < n; i++)\n\
         \x20       if (next[i] == t) { sx_set(cid[i], !sx_read(cid[i])); \
         next[i] = UINT64_MAX - next[i] < half[i] ? UINT64_MAX : next[i] + half[i]; }\n\
         \x20   sx_run_settle(*now);\n\
         \x20   return 1;\n}\n\n",
    );

    // The dynamic range assert (spec 3.26): after settles, ranged numerics
    // must lie in their domain.
    let ranged: Vec<(u32, &siox::ir::Signal)> = design
        .signals
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.range.map(|_| (i as u32, s)))
        .collect();
    if !ranged.is_empty() {
        prog.push_str(
            "static signed sx_check_ranges(void) {\n    int64_t v;\n    uint32_t e;\n\
             \x20   if (g_range_failed) return 1;\n\
             \x20   e = sx_range_error();\n",
        );
        // Read a ranged signal into `v`, sign-extending when its domain goes
        // below zero. Both the engine-flagged path and the post-settle scan
        // need the value, and the message is written once for both: naming the
        // signal and its bounds while dropping the number that broke them left
        // the reader to go and find it.
        let decode = |id: u32, sig: &siox::ir::Signal, lo: i64| {
            if lo < 0 && sig.width > 0 && sig.width < 64 {
                format!(
                    "v = (int64_t)sx_read({id}); if (v & {s}LL) v -= {m}LL;",
                    s = 1u64 << (sig.width - 1),
                    m = 1u64 << sig.width
                )
            } else {
                format!("v = (int64_t)sx_read({id});")
            }
        };
        let report = |id: u32, sig: &siox::ir::Signal, lo: i64, hi: i64| {
            let buffer = format!("_sx_range_{id}");
            let capacity = sig.path.len() + 64;
            format!(
                "static char {buffer}[{capacity}]; \
                 snprintf({buffer}, sizeof {buffer}, \
                 \"`{}` left its range {lo}..{hi} (it was %lld)\", (long long)v); \
                 g_msg = {buffer};",
                sig.path
            )
        };

        prog.push_str("    if (e) { switch (e) {\n");
        for (id, sig) in &ranged {
            let (lo, hi) = sig.range.unwrap();
            prog.push_str(&format!(
                "    case {}: {{ {} {} }} break;\n",
                id + 1,
                decode(*id, sig, lo),
                report(*id, sig, lo, hi)
            ));
        }
        prog.push_str(
            "    default: g_msg = \"a ranged signal left its domain\"; break;\n\
             \x20   } g_range_failed = 1; return 1; }\n",
        );
        for (id, sig) in &ranged {
            let (lo, hi) = sig.range.unwrap();
            prog.push_str(&format!(
                "    {}\n    if (v < {lo}LL || v > {hi}LL) {{ {} g_range_failed = 1; return 1; }}\n",
                decode(*id, sig, lo),
                report(*id, sig, lo, hi)
            ));
        }
        prog.push_str("    return 0;\n}\n\n");
    } else {
        prog.push_str("static signed sx_check_ranges(void) { return 0; }\n\n");
    }

    let mut names = Vec::new();
    for &root in &tests {
        let name = hier.instance(root).entity.clone();
        let qualified = qualified_test_name(modules, &name);
        let (map, aliases) = build_map(hier, root, design);
        let items = test_items(modules, &name);
        let clocks = scan_clocks(&items, &aliases)?;
        let instance_names: std::collections::HashSet<String> = hier
            .instance(root)
            .children
            .iter()
            .map(|&c| hier.instance(c).name.clone())
            .collect();
        let ctx = Ctx {
            design,
            map: &map,
            enums: &enums,
            families: &families,
            name: &name,
            clocks,
            locals: Default::default(),
            local_widths: Default::default(),
            local_indices: Default::default(),
            local_ranges: Default::default(),
            local_families: Default::default(),
            local_types: Default::default(),
            op_impls: &op_impls,
            methods: &methods,
            structs: &structs,
            derived_widths: &derived_widths,
            const_exprs: &const_exprs,
            const_array_exprs: &const_array_exprs,
            consts: &consts,
            real_consts: &real_consts,
            integer_consts: &integer_consts,
            const_ranges: &const_ranges,
            aliases: &aliases,
            tmp: Default::default(),
            message_id: Default::default(),
            fns: &fns,
            extern_fns: &extern_fns,
            type_aliases: &type_aliases,
            fn_env: Default::default(),
            fn_type_env: Default::default(),
            instance_names,
            value_bits,
        };
        prog.push_str(&ctx.gen_test_fn(&items)?);
        names.push((name, qualified));
    }
    prog.push_str(&gen_main(&names));

    // Emit the DUT object (all tests' logic) and link with clang.
    let tmp = std::env::temp_dir().join(format!("siox_build_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let obj = tmp.join("design.o");
    let csrc = tmp.join("sim.c");
    siox::llvm::emit_object(design, &obj)?;
    std::fs::write(&csrc, &prog).map_err(|e| e.to_string())?;
    if std::env::var("SIOX_DEBUG_C").is_ok() {
        let _ = std::fs::write("/tmp/siox_debug.c", &prog);
    }
    let status = Command::new("clang")
        .args([
            csrc.to_str().unwrap(),
            obj.to_str().unwrap(),
            "-O2",
            "-lm",
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .map_err(|e| format!("failed to run clang: {e}"))?;
    let _ = std::fs::remove_dir_all(&tmp);
    if !status.success() {
        return Err("clang failed to link the simulator".into());
    }
    Ok(())
}

#[derive(Default)]
struct VcdScope {
    children: BTreeMap<String, VcdScope>,
    signals: Vec<(usize, String)>,
}

fn logic_vcd_symbols(design: &Design, signal: &siox::ir::Signal) -> Option<HashMap<u64, char>> {
    logic_vcd_symbols_for_type(design, signal.enum_type.as_deref()?)
}

fn logic_vcd_symbols_for_type(design: &Design, type_name: &str) -> Option<HashMap<u64, char>> {
    let symbols = design.enum_syms.get(type_name)?;
    let mut out = HashMap::new();
    for (&disc, symbol) in symbols {
        let ch = symbol
            .strip_prefix('\'')?
            .strip_suffix('\'')?
            .chars()
            .next()?;
        out.insert(
            disc,
            match ch {
                '0' | 'L' => '0',
                '1' | 'H' => '1',
                'Z' => 'z',
                'U' | 'X' | 'W' | '-' => 'x',
                _ => return None,
            },
        );
    }
    Some(out)
}

fn emit_vcd_scope_header(out: &mut String, name: &str, scope: &VcdScope, design: &Design) {
    out.push_str(&format!("$scope module {name} $end\n"));
    for &(id, ref signal_name) in &scope.signals {
        let signal = &design.signals[id];
        let kind = if signal.real {
            "real"
        } else if signal.enum_type.as_ref().is_some_and(|ty| {
            logic_vcd_symbols(design, signal).is_none() && design.enum_syms.contains_key(ty)
        }) {
            "string"
        } else {
            "wire"
        };
        let width = if kind == "string" || logic_vcd_symbols(design, signal).is_some() {
            1
        } else {
            signal.width.max(1)
        };
        out.push_str(&format!("$var {kind} {width} v{id} {signal_name} $end\n"));
    }
    for (child_name, child) in &scope.children {
        emit_vcd_scope_header(out, child_name, child, design);
    }
    out.push_str("$upscope $end\n");
}

/// Generate the VCD writer into the native executable. Values are sampled
/// directly from the design ABI after each settle; no trace is returned to the
/// compiler process.
fn gen_vcd_runtime(design: &Design) -> String {
    let companions: std::collections::HashSet<usize> =
        design.meta_of.values().map(|id| *id as usize).collect();
    let mut root = VcdScope::default();
    for (id, signal) in design.signals.iter().enumerate() {
        if companions.contains(&id) {
            continue;
        }
        let mut parts = signal.path.split('.').collect::<Vec<_>>();
        let signal_name = parts.pop().unwrap_or("signal").to_string();
        let mut scope = &mut root;
        if parts.is_empty() {
            parts.push("top");
        }
        for part in parts {
            scope = scope.children.entry(part.to_string()).or_default();
        }
        scope.signals.push((id, signal_name));
    }
    let mut header =
        String::from("$version siox native test executable $end\n$timescale 1fs $end\n");
    for (name, scope) in &root.children {
        emit_vcd_scope_header(&mut header, name, scope, design);
    }
    header.push_str("$enddefinitions $end\n");

    let n = design.signals.len().max(1);
    let mut c = format!(
        "static FILE *g_vcd;\n\
         static sx_value g_vcd_last[{n}];\n\
         static unsigned char g_vcd_seen[{n}];\n\
         static uint64_t g_vcd_base, g_vcd_last_time;\n\
         static signed g_vcd_started;\n\
         static signed sx_vcd_open(const char *path) {{\n\
         \x20   g_vcd = fopen(path, \"wb\");\n\
         \x20   if (!g_vcd) {{ fprintf(stderr, \"cannot open VCD output %s\\n\", path); return 0; }}\n\
         \x20   fputs(\"{}\", g_vcd);\n\
         \x20   return 1;\n\
         }}\n\
         static void sx_vcd_close(void) {{ if (g_vcd) {{ fclose(g_vcd); g_vcd = 0; }} }}\n\
         static void sx_vcd_begin_test(void) {{\n\
         \x20   if (!g_vcd) return;\n\
         \x20   g_vcd_base = g_vcd_started && g_vcd_last_time != UINT64_MAX \
         ? g_vcd_last_time + 1 : (g_vcd_started ? UINT64_MAX : 0);\n\
         \x20   memset(g_vcd_seen, 0, sizeof(g_vcd_seen));\n\
         }}\n\
         static void sx_vcd_sample(uint64_t now) {{\n\
         \x20   if (!g_vcd) return;\n\
         \x20   uint64_t _time = UINT64_MAX - g_vcd_base < now \
         ? UINT64_MAX : g_vcd_base + now;\n\
         \x20   signed _wrote = 0, _initial = !g_vcd_started;\n",
        c_escape(&header)
    );

    let timestamp = "if (!_wrote) { if (_initial || _time != g_vcd_last_time) fprintf(g_vcd, \"#%llu\\n\", (unsigned long long)_time); if (_initial) fputs(\"$dumpvars\\n\", g_vcd); _wrote = 1; }";
    for (id, signal) in design.signals.iter().enumerate() {
        if companions.contains(&id) {
            continue;
        }
        let meta = design
            .meta_of
            .get(&(id as u32))
            .copied()
            .map(|v| v as usize);
        c.push_str(&format!("    {{ sx_value _v = sx_read({id});"));
        if let Some(meta_id) = meta {
            c.push_str(&format!(
                " sx_value _m = sx_read({meta_id}); if (!g_vcd_seen[{id}] || _v != g_vcd_last[{id}] || !g_vcd_seen[{meta_id}] || _m != g_vcd_last[{meta_id}]) {{ {timestamp} "
            ));
            let table = design
                .vector_element_enums
                .get(&(id as u32))
                .map(String::as_str)
                .and_then(|name| logic_vcd_symbols_for_type(design, name))
                .unwrap_or_default();
            c.push_str("fputc('b', g_vcd); for (signed _b = ");
            c.push_str(&signal.width.saturating_sub(1).to_string());
            c.push_str("; _b >= 0; _b--) { unsigned _d = (unsigned)((_m >> (_b * 4)) & 15); signed _ch = 'x'; switch (_d) {");
            let mut entries = table.into_iter().collect::<Vec<_>>();
            entries.sort_by_key(|(disc, _)| *disc);
            for (disc, ch) in entries {
                c.push_str(&format!("case {disc}: _ch = '{ch}'; break;"));
            }
            c.push_str("} if (_ch != 'x' && _ch != 'z') _ch = ((_v >> _b) & 1) ? '1' : '0'; fputc(_ch, g_vcd); }");
            c.push_str(&format!(
                " fprintf(g_vcd, \" v{id}\\n\"); g_vcd_last[{meta_id}] = _m; g_vcd_seen[{meta_id}] = 1;"
            ));
        } else {
            c.push_str(&format!(
                " if (!g_vcd_seen[{id}] || _v != g_vcd_last[{id}]) {{ {timestamp} "
            ));
            if signal.real {
                c.push_str(&format!(
                    "fprintf(g_vcd, \"r%.17g v{id}\\n\", sx_f64((uint64_t)_v));"
                ));
            } else if let Some(table) = logic_vcd_symbols(design, signal) {
                c.push_str("signed _ch = 'x'; switch ((uint64_t)_v) {");
                let mut entries = table.into_iter().collect::<Vec<_>>();
                entries.sort_by_key(|(disc, _)| *disc);
                for (disc, ch) in entries {
                    c.push_str(&format!("case {disc}ULL: _ch = '{ch}'; break;"));
                }
                c.push_str(&format!("}} fprintf(g_vcd, \"%cv{id}\\n\", _ch);"));
            } else if let Some(symbols) = signal
                .enum_type
                .as_ref()
                .and_then(|ty| design.enum_syms.get(ty))
            {
                c.push_str("switch ((uint64_t)_v) {");
                let mut entries = symbols.iter().collect::<Vec<_>>();
                entries.sort_by_key(|(disc, _)| **disc);
                for (&disc, symbol) in entries {
                    c.push_str(&format!(
                        "case {disc}ULL: fputs(\"s{} v{id}\\n\", g_vcd); break;",
                        c_escape(symbol)
                    ));
                }
                c.push_str(&format!(
                    "default: fprintf(g_vcd, \"s%llu v{id}\\n\", (unsigned long long)_v); break; }}"
                ));
            } else if signal.width <= 1 {
                c.push_str(&format!(
                    "fprintf(g_vcd, \"%cv{id}\\n\", (_v & 1) ? '1' : '0');"
                ));
            } else {
                c.push_str("fputc('b', g_vcd); for (signed _b = ");
                c.push_str(&signal.width.saturating_sub(1).to_string());
                c.push_str("; _b >= 0; _b--) fputc(((_v >> _b) & 1) ? '1' : '0', g_vcd);");
                c.push_str(&format!(" fputs(\" v{id}\\n\", g_vcd);"));
            }
        }
        c.push_str(&format!(
            " g_vcd_last[{id}] = _v; g_vcd_seen[{id}] = 1; }} }}\n"
        ));
    }
    c.push_str(
        "    if (_wrote) { if (_initial) fputs(\"$end\\n\", g_vcd); g_vcd_started = 1; g_vcd_last_time = _time; }\n\
         }\n\
         static void sx_run_settle(uint64_t now) { sx_settle(); sx_vcd_sample(now); (void)sx_check_ranges(); }\n\n",
    );
    c
}

/// The libtest-style `main` that runs each `test_<name>` and reports results.
/// Accepts an optional name-substring filter and `--vcd <path>`.
fn gen_main(names: &[(String, String)]) -> String {
    let mut m = String::new();
    m.push_str("signed main(signed argc, char **argv) {\n");
    m.push_str(
        "    const char *filter = 0, *vcd_path = 0;\n\
         \x20   for (signed i = 1; i < argc; i++) {\n\
         \x20       if (!strcmp(argv[i], \"--vcd\")) {\n\
         \x20           if (++i == argc) { fprintf(stderr, \"--vcd requires a path\\n\"); return 2; }\n\
         \x20           vcd_path = argv[i];\n\
         \x20       } else if (!strncmp(argv[i], \"--vcd=\", 6)) vcd_path = argv[i] + 6;\n\
         \x20       else if (!filter) filter = argv[i];\n\
         \x20       else { fprintf(stderr, \"unexpected argument: %s\\n\", argv[i]); return 2; }\n\
         \x20   }\n\
         \x20   if (vcd_path && !sx_vcd_open(vcd_path)) return 2;\n",
    );
    m.push_str("    signed failed = 0, ran = 0, filtered = 0;\n");
    // Count how many tests match, so the "running N tests" line is post-filter.
    for (_, display) in names {
        m.push_str(&format!(
            "    if (!filter || strstr(\"{display}\", filter)) ran++; else filtered++;\n"
        ));
    }
    m.push_str("    printf(\"\\nrunning %d test%s\\n\", ran, ran == 1 ? \"\" : \"s\");\n");
    for (symbol, display) in names {
        m.push_str(&format!(
            "    if (!filter || strstr(\"{display}\", filter)) {{ \
             if (test_{symbol}()) {{ printf(\"test {display} ... FAILED\\n    %s\\n\", g_msg); failed++; }} \
             else printf(\"test {display} ... ok\\n\"); }}\n"
        ));
    }
    m.push_str(
        "    printf(\"\\ntest result: %s. %d passed; %d failed; %d filtered out\",\n\
         \x20          failed ? \"FAILED\" : \"ok\", ran - failed, failed, filtered);\n\
         \x20   if (g_warnings) printf(\"; %d warning%s\", g_warnings, g_warnings == 1 ? \"\" : \"s\");\n\
         \x20   printf(\"\\n\");\n",
    );
    m.push_str("    sx_vcd_close();\n    return failed ? 1 : 0;\n}\n");
    m
}

fn qualified_test_name(modules: &[Module], entity: &str) -> String {
    modules
        .iter()
        .find(|m| {
            m.items
                .iter()
                .any(|item| matches!(item, ast::Item::Entity(e) if e.name.text == entity))
        })
        .map(|m| {
            let module = m
                .path
                .segments
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join("::");
            if module.is_empty() {
                entity.to_string()
            } else {
                format!("{module}::{entity}")
            }
        })
        .unwrap_or_else(|| entity.to_string())
}

/// Translation context: the design, this test's name -> signal map, and enum
/// discriminants.
struct Ctx<'a> {
    design: &'a Design,
    map: &'a HashMap<String, SignalId>,
    enums: &'a HashMap<String, HashMap<String, u64>>,
    families: &'a std::collections::HashSet<String>,
    name: &'a str,
    /// `clock(clk, ..)`-registered background clocks: (signal id, half period fs).
    clocks: Vec<(u32, u64)>,
    /// Names currently bound as C locals (unconnected `let`s, loop variables).
    locals: std::cell::RefCell<std::collections::HashSet<String>>,
    /// Declared bit width of a C local (`let c: unsigned[8]` -> 8): writes mask to
    /// it so arithmetic wraps exactly like the equivalent hardware signal.
    local_widths: std::cell::RefCell<HashMap<String, u32>>,
    /// Array/string name -> logical indices in declaration order. Width-only
    /// arrays are `0..N-1`; explicit ranges preserve either direction.
    local_indices: std::cell::RefCell<HashMap<String, Vec<i64>>>,
    /// Declared `(left, right)` bounds for range attributes, preserving
    /// direction independently from flattened storage order.
    local_ranges: std::cell::RefCell<HashMap<String, (i64, i64)>>,
    /// Declared vector family of a testbench name (`let a: signed[8]` -> "signed"),
    /// connected or local — operators on it inline the family's impls.
    local_families: std::cell::RefCell<HashMap<String, String>>,
    /// Operator-trait impls `(trait, type) -> fn`, mirroring the runner.
    op_impls: &'a NativeOperatorImpls<'a>,
    /// Impl methods `(type head, method) -> fn`, for `recv.method(args)`.
    methods: &'a HashMap<(String, String), &'a ast::FnDecl>,
    /// Struct layouts (name -> base-first field list) so a struct-typed
    /// testbench local materializes as one C local per leaf field.
    structs: &'a HashMap<String, Vec<(String, ast::Type)>>,
    /// Derived-type inherited widths (`struct Byte : Logic[8]` -> 8), so a bare
    /// derived-vector local masks to the right width.
    derived_widths: &'a HashMap<String, u32>,
    /// Declared type head of a testbench local (`let p: Pkt` -> "Pkt"), for
    /// resolving a method call's receiver type.
    local_types: std::cell::RefCell<HashMap<String, String>>,
    /// Module-level `const` values, for bare-name references.
    const_exprs: &'a HashMap<String, String>,
    /// Module-level `const` lookup tables, one expression per element.
    const_array_exprs: &'a HashMap<String, Vec<String>>,
    /// Integer module constants, also usable as local type widths.
    consts: &'a HashMap<String, u128>,
    /// Module constants declared as `real`; their stored C expressions are f64
    /// bit patterns and must be decoded when used as operands.
    real_consts: &'a std::collections::HashSet<String>,
    /// Module constants declared as kernel `integer`.
    integer_consts: &'a std::collections::HashSet<String>,
    /// Named range constants used by local type indices.
    const_ranges: &'a HashMap<String, (i64, i64)>,
    /// Testbench name -> EVERY connected port's signal id (a write drives all).
    aliases: &'a HashMap<String, Vec<SignalId>>,
    /// Unique-suffix counter for generated C identifiers.
    tmp: std::cell::Cell<usize>,
    /// Unique local-static buffer suffix for formatted assertion/warning
    /// messages. Each call owns storage sized from its actual format arity.
    message_id: std::cell::Cell<usize>,
    /// Module-level functions (testbench-callable; translated to C ternaries).
    fns: &'a HashMap<String, &'a ast::FnDecl>,
    /// Functions declared in an `extern "C"` block. Their signatures drive
    /// native ABI conversion instead of requiring a Siox body to inline.
    extern_fns: &'a std::collections::HashSet<String>,
    /// Module type aliases, used when deciding native and foreign ABI kinds.
    type_aliases: &'a HashMap<String, ast::Type>,
    /// Parameter-substitution stack while translating a fn body.
    fn_env: std::cell::RefCell<Vec<HashMap<String, String>>>,
    /// Declared parameter types parallel to `fn_env`, retained while inlining
    /// user functions so named real parameters keep floating-point semantics.
    fn_type_env: std::cell::RefCell<Vec<HashMap<String, String>>>,
    /// Names elaboration turned into DUT instances (`let dut: Sub = {..}` /
    /// `let dut: Sub [= {..}]`) — their `let`s are wired by elaboration and
    /// emit no testbench code.
    instance_names: std::collections::HashSet<String>,
    /// Harness-wide `_BitInt` width, also the upper bound for decimal
    /// formatting capacity.
    value_bits: u32,
}

/// Struct layouts keyed by name, each a base-first flattened field list
/// (`struct B : A` prepends A's fields), so a struct-typed testbench local can
/// be materialized as one C local per field.
fn collect_structs(modules: &[Module]) -> HashMap<String, StructFields> {
    let mut raw: RawStructLayouts = HashMap::new();
    for m in modules {
        for item in &m.items {
            if let ast::Item::Struct(s) = item {
                let base = s.base.as_ref().and_then(type_head_name).map(str::to_string);
                let own = s
                    .fields
                    .iter()
                    .map(|f| (f.name.text.clone(), f.ty.clone()))
                    .collect();
                raw.insert(s.name.text.clone(), (base, own));
            }
        }
    }
    fn flat(
        name: &str,
        raw: &RawStructLayouts,
        seen: &mut std::collections::HashSet<String>,
    ) -> StructFields {
        if !seen.insert(name.to_string()) {
            return Vec::new();
        }
        let Some((base, own)) = raw.get(name) else {
            return Vec::new();
        };
        let mut out = match base {
            Some(b) => flat(b, raw, seen),
            None => Vec::new(),
        };
        out.extend(own.iter().cloned());
        seen.remove(name);
        out
    }
    raw.keys()
        .map(|k| {
            (
                k.clone(),
                flat(k, &raw, &mut std::collections::HashSet::new()),
            )
        })
        .collect()
}

fn max_literal_type_width(modules: &[Module], derived: &HashMap<String, u32>) -> u32 {
    fn literal_width(text: &str) -> u32 {
        if text.contains('.') {
            return 64;
        }
        let words = parse_word_literal(text);
        let high = words.last().copied().unwrap_or(0);
        ((words.len().saturating_sub(1) as u32) * 64 + (64 - high.leading_zeros())).max(1)
    }

    fn expr_width(expression: &ast::Expr) -> u32 {
        use ast::Expr;
        match expression {
            Expr::Int { text, .. } | Expr::SuffixLit { text, .. } => literal_width(text),
            Expr::BitStrLit { base, digits, .. } => {
                let bits = match base.to_ascii_lowercase() {
                    'b' => 1,
                    'o' => 3,
                    'x' => 4,
                    _ => 1,
                };
                u32::try_from(digits.chars().count())
                    .ok()
                    .and_then(|count| count.checked_mul(bits))
                    .unwrap_or(u32::MAX)
                    .max(1)
            }
            Expr::Field { base, .. }
            | Expr::SysAttr { base, .. }
            | Expr::Unary { rhs: base, .. } => expr_width(base),
            Expr::Index { base, index, .. } => {
                let mut width = expr_width(base).max(expr_width(index));
                // A conversion-shaped `unsigned[128](...)` needs a 128-bit C
                // value even when no design signal has that width. Treating a
                // plain constant index the same way is conservative and keeps
                // this syntax-only scan independent of type resolution.
                if matches!(base.as_ref(), Expr::Path(_)) {
                    if let Expr::Int { text, .. } = index.as_ref() {
                        width = width.max(text.replace('_', "").parse().unwrap_or(0));
                    }
                }
                width
            }
            Expr::Range { lo, hi, .. } => expr_width(lo).max(expr_width(hi)),
            Expr::PartialRange { lo, hi, .. } => lo
                .as_deref()
                .map(expr_width)
                .unwrap_or(0)
                .max(hi.as_deref().map(expr_width).unwrap_or(0)),
            Expr::Binary { lhs, rhs, .. } => expr_width(lhs).max(expr_width(rhs)),
            Expr::IfExpr {
                cond, then, els, ..
            } => expr_width(cond).max(expr_width(then)).max(expr_width(els)),
            Expr::Match {
                scrutinee, arms, ..
            } => arms.iter().fold(expr_width(scrutinee), |width, arm| {
                width.max(block_width(&arm.body))
            }),
            Expr::Call { callee, args, .. } => args
                .iter()
                .fold(expr_width(callee), |width, arg| width.max(expr_width(arg))),
            Expr::Construct { args, spread, .. } => {
                let width = args
                    .iter()
                    .filter_map(|argument| argument.value.as_ref())
                    .fold(0, |width, value| width.max(expr_width(value)));
                width.max(spread.as_deref().map(expr_width).unwrap_or(0))
            }
            Expr::Concat { parts, .. } => parts
                .iter()
                .map(expr_width)
                .try_fold(0u32, u32::checked_add)
                .unwrap_or(u32::MAX),
            Expr::Array { elems, .. } => elems.iter().map(expr_width).max().unwrap_or(0),
            Expr::CharLit { .. } | Expr::StrLit { .. } | Expr::Path(_) => 0,
        }
    }

    fn stmt_width(statement: &ast::Stmt) -> u32 {
        match statement {
            ast::Stmt::Let(declaration) => declaration.value.as_ref().map(expr_width).unwrap_or(0),
            ast::Stmt::Assign {
                target,
                value,
                after,
                ..
            } => expr_width(target)
                .max(expr_width(value))
                .max(after.as_ref().map(expr_width).unwrap_or(0)),
            ast::Stmt::If(statement) => {
                let else_width = match statement.else_.as_deref() {
                    Some(ast::ElseBranch::Block(block)) => block_width(block),
                    Some(ast::ElseBranch::If(statement)) => if_width(statement),
                    None => 0,
                };
                expr_width(&statement.cond)
                    .max(block_width(&statement.then))
                    .max(else_width)
            }
            ast::Stmt::Match(statement) => statement
                .arms
                .iter()
                .fold(expr_width(&statement.scrutinee), |width, arm| {
                    width.max(block_width(&arm.body))
                }),
            ast::Stmt::For { range, body, .. } => expr_width(range).max(block_width(body)),
            ast::Stmt::Expr(expression) => expr_width(expression),
            ast::Stmt::Return { value, .. } => value.as_ref().map(expr_width).unwrap_or(0),
        }
    }

    fn if_width(statement: &ast::IfStmt) -> u32 {
        let else_width = match statement.else_.as_deref() {
            Some(ast::ElseBranch::Block(block)) => block_width(block),
            Some(ast::ElseBranch::If(statement)) => if_width(statement),
            None => 0,
        };
        expr_width(&statement.cond)
            .max(block_width(&statement.then))
            .max(else_width)
    }

    fn block_width(block: &ast::Block) -> u32 {
        block.stmts.iter().map(stmt_width).max().unwrap_or(0)
    }

    fn width(ty: &ast::Type, derived: &HashMap<String, u32>) -> u32 {
        match ty {
            ast::Type::Path(p) => p
                .segments
                .last()
                .and_then(|s| derived.get(&s.text))
                .copied()
                .unwrap_or(0),
            ast::Type::Indexed { base, index, .. } => {
                let own = index
                    .as_deref()
                    .and_then(|e| match e {
                        ast::Expr::Int { text, .. } => text.replace('_', "").parse().ok(),
                        ast::Expr::Range { lo, hi, .. } => {
                            let ast::Expr::Int { text: lo, .. } = lo.as_ref() else {
                                return None;
                            };
                            let ast::Expr::Int { text: hi, .. } = hi.as_ref() else {
                                return None;
                            };
                            let lo = lo.replace('_', "").parse::<i64>().ok()? as i128;
                            let hi = hi.replace('_', "").parse::<i64>().ok()? as i128;
                            u32::try_from((lo - hi).unsigned_abs().saturating_add(1)).ok()
                        }
                        _ => None,
                    })
                    .unwrap_or(0);
                own.max(width(base, derived))
            }
            ast::Type::Generic { base, .. } | ast::Type::View { target: base, .. } => {
                width(base, derived)
            }
        }
    }
    let mut widths = Vec::new();
    for module in modules {
        for item in &module.items {
            match item {
                ast::Item::Entity(e) => {
                    widths.extend(e.ports.iter().map(|p| width(&p.ty, derived)))
                }
                ast::Item::Struct(s) => {
                    widths.extend(s.fields.iter().map(|f| width(&f.ty, derived)))
                }
                ast::Item::Fn(f) => {
                    widths.extend(
                        f.params
                            .iter()
                            .filter_map(|p| p.ty.as_ref())
                            .map(|t| width(t, derived)),
                    );
                    widths.extend(f.ret.as_ref().map(|t| width(t, derived)));
                    widths.extend(f.body.as_ref().map(block_width));
                }
                ast::Item::Const(constant) => {
                    widths.push(width(&constant.ty, derived));
                    widths.push(expr_width(&constant.value));
                }
                ast::Item::Enum(en) => {
                    widths.extend(
                        en.variants
                            .iter()
                            .filter_map(|variant| variant.value.as_ref())
                            .map(expr_width),
                    );
                }
                ast::Item::Impl(im) => {
                    for item in &im.items {
                        match item {
                            ast::ImplItem::Let(l) => {
                                widths.extend(l.ty.as_ref().map(|t| width(t, derived)));
                                widths.extend(l.value.as_ref().map(expr_width));
                            }
                            ast::ImplItem::Const(constant) => {
                                widths.push(width(&constant.ty, derived));
                                widths.push(expr_width(&constant.value));
                            }
                            ast::ImplItem::Fn(f) => {
                                widths.extend(
                                    f.params
                                        .iter()
                                        .filter_map(|p| p.ty.as_ref())
                                        .map(|t| width(t, derived)),
                                );
                                widths.extend(f.ret.as_ref().map(|t| width(t, derived)));
                                widths.extend(f.body.as_ref().map(block_width));
                            }
                            ast::ImplItem::Stmt(statement) => widths.push(stmt_width(statement)),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }
    widths.into_iter().max().unwrap_or(0)
}

/// Every impl method by `(type head, method name)`, inherent and trait impls,
/// mirroring the runner's `collect_methods`.
fn collect_methods(modules: &[Module]) -> HashMap<(String, String), &ast::FnDecl> {
    let mut out = HashMap::new();
    for m in modules {
        for item in &m.items {
            if let ast::Item::Impl(im) = item {
                if let Some(ty) = type_head_name(&im.target) {
                    for it in &im.items {
                        if let ast::ImplItem::Fn(f) = it {
                            out.entry((ty.to_string(), f.name.text.clone()))
                                .or_insert(f);
                        }
                    }
                }
            }
        }
    }
    out
}

/// A valid C identifier for a testbench local. Bare names (the common case)
/// pass through unchanged; a struct-field or array-element name (`p.a`, `v[2]`)
/// The literal path of a `read`/`read_to_string` call, if `e` is one.
fn fs_read_path(e: &ast::Expr, which: &str) -> Option<String> {
    let ast::Expr::Call { callee, args, .. } = e else {
        return None;
    };
    let ast::Expr::Path(p) = callee.as_ref() else {
        return None;
    };
    if p.segments.len() != 1 || p.segments[0].text != which {
        return None;
    }
    match args.first() {
        Some(ast::Expr::StrLit { text, .. }) => Some(text.clone()),
        _ => None,
    }
}

/// is mangled to a flat identifier (`sxl_p_a`, `sxl_v_2`).
fn c_local_ident(name: &str) -> String {
    if name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
        return name.to_string();
    }
    let mut s = String::from("sxl_");
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c);
        } else {
            s.push('_');
        }
    }
    s
}

/// Escape text for embedding inside a C string literal: backslash first,
/// then quote, newline, tab, CR (a raw newline would split the literal).
fn c_escape(t: &str) -> String {
    t.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
}

/// Evaluate a module `const` initializer for the native testbench: literals,
/// logic chars, enum variants, other consts, and const-fn arithmetic.
fn eval_c_const(
    e: &ast::Expr,
    consts: &HashMap<String, u128>,
    enums: &HashMap<String, HashMap<String, u64>>,
    fns: &HashMap<String, &ast::FnDecl>,
) -> Option<u128> {
    match e {
        ast::Expr::Int { text, .. } => {
            let words = parse_word_literal(text);
            (words.len() <= 2).then(|| {
                words.first().copied().unwrap_or(0) as u128
                    | ((words.get(1).copied().unwrap_or(0) as u128) << 64)
            })
        }
        ast::Expr::CharLit { ch, .. } => Some(logic_lit_value(*ch, enums) as u128),
        ast::Expr::Path(p) if p.segments.len() == 1 => consts.get(&p.segments[0].text).copied(),
        ast::Expr::Path(p) if p.segments.len() >= 2 => enums
            .get(&p.segments[0].text)
            .and_then(|m| m.get(&p.segments[1].text))
            .map(|&d| d as u128),
        ast::Expr::Unary { op, rhs, .. } => {
            let value = eval_c_const(rhs, consts, enums, fns)?;
            Some(match op {
                ast::UnOp::Neg => 0u128.wrapping_sub(value),
                ast::UnOp::Not => u128::from(value == 0),
            })
        }
        ast::Expr::Binary { op, lhs, rhs, .. } => {
            let left = eval_c_const(lhs, consts, enums, fns)?;
            let right = eval_c_const(rhs, consts, enums, fns)?;
            Some(match op {
                ast::BinOp::Add => left.wrapping_add(right),
                ast::BinOp::Sub => left.wrapping_sub(right),
                ast::BinOp::Mul => left.wrapping_mul(right),
                ast::BinOp::Div if right != 0 => left / right,
                ast::BinOp::Shl => left.checked_shl(right.try_into().ok()?).unwrap_or(0),
                ast::BinOp::Shr => left.checked_shr(right.try_into().ok()?).unwrap_or(0),
                ast::BinOp::Eq => u128::from(left == right),
                ast::BinOp::Ne => u128::from(left != right),
                ast::BinOp::Lt => u128::from(left < right),
                ast::BinOp::Le => u128::from(left <= right),
                ast::BinOp::Gt => u128::from(left > right),
                ast::BinOp::Ge => u128::from(left >= right),
                ast::BinOp::And => left & right,
                ast::BinOp::Or => left | right,
                _ => return None,
            })
        }
        ast::Expr::IfExpr {
            cond, then, els, ..
        } => {
            if eval_c_const(cond, consts, enums, fns)? != 0 {
                eval_c_const(then, consts, enums, fns)
            } else {
                eval_c_const(els, consts, enums, fns)
            }
        }
        _ => {
            let env: HashMap<String, i64> =
                consts.iter().map(|(k, &v)| (k.clone(), v as i64)).collect();
            siox::ir::eval_const_fns(e, &env, fns, 0).map(|v| v as u128)
        }
    }
}

fn emit_c_const(
    expression: &ast::Expr,
    constants: &HashMap<String, String>,
    enums: &HashMap<String, HashMap<String, u64>>,
) -> Option<String> {
    match expression {
        ast::Expr::Int { text, .. } => Some(c_word_literal(&parse_word_literal(text))),
        ast::Expr::CharLit { ch, .. } => {
            Some(format!("((sx_value){})", logic_lit_value(*ch, enums)))
        }
        ast::Expr::Path(path) if path.segments.len() == 1 => {
            constants.get(&path.segments[0].text).cloned()
        }
        ast::Expr::Path(path) if path.segments.len() >= 2 => enums
            .get(&path.segments[0].text)
            .and_then(|variants| variants.get(&path.segments[1].text))
            .map(|value| format!("((sx_value){value}ULL)")),
        ast::Expr::Unary {
            op: ast::UnOp::Neg,
            rhs,
            ..
        } => {
            let rhs = emit_c_const(rhs, constants, enums)?;
            Some(format!("(-({rhs}))"))
        }
        ast::Expr::Binary { op, lhs, rhs, .. } => {
            let lhs = emit_c_const(lhs, constants, enums)?;
            let rhs = emit_c_const(rhs, constants, enums)?;
            match op {
                ast::BinOp::Div => Some(format!("sx_udiv(({lhs}), ({rhs}))")),
                ast::BinOp::Shl => Some(format!("sx_shl(({lhs}), ({rhs}))")),
                ast::BinOp::Shr => Some(format!("sx_shr(({lhs}), ({rhs}))")),
                _ => Some(format!("(({lhs}) {} ({rhs}))", c_binop(op).ok()?)),
            }
        }
        ast::Expr::IfExpr {
            cond, then, els, ..
        } => Some(format!(
            "(({}) ? ({}) : ({}))",
            emit_c_const(cond, constants, enums)?,
            emit_c_const(then, constants, enums)?,
            emit_c_const(els, constants, enums)?
        )),
        ast::Expr::SuffixLit { text, suffix, .. } => {
            let value = c_word_literal(&parse_word_literal(text));
            let scale = ast::suffix_scale(&suffix.text).unwrap_or(1);
            Some(format!("(({value}) * {scale}ULL)"))
        }
        _ => None,
    }
}

/// Emit a module `real` constant as a C `double` expression. Named real
/// dependencies are stored as f64 bit patterns in `consts`, so decode them;
/// integer-shaped dependencies promote normally.
fn emit_c_real_const(
    e: &ast::Expr,
    consts: &HashMap<String, String>,
    real_consts: &std::collections::HashSet<String>,
    enums: &HashMap<String, HashMap<String, u64>>,
) -> Option<String> {
    match e {
        ast::Expr::Int { text, .. } => {
            let normalized = text.replace('_', "");
            normalized
                .parse::<f64>()
                .ok()
                .map(|_| format!("((double)({normalized}))"))
        }
        ast::Expr::Path(path) if path.segments.len() == 1 => {
            let name = &path.segments[0].text;
            let value = consts.get(name)?;
            Some(if real_consts.contains(name) {
                format!("sx_f64({value})")
            } else {
                format!("((double)({value}))")
            })
        }
        ast::Expr::Unary {
            op: ast::UnOp::Neg,
            rhs,
            ..
        } => Some(format!(
            "(-({}))",
            emit_c_real_const(rhs, consts, real_consts, enums)?
        )),
        ast::Expr::Binary { op, lhs, rhs, .. }
            if matches!(
                op,
                ast::BinOp::Add | ast::BinOp::Sub | ast::BinOp::Mul | ast::BinOp::Div
            ) =>
        {
            Some(format!(
                "(({}) {} ({}))",
                emit_c_real_const(lhs, consts, real_consts, enums)?,
                c_binop(op).ok()?,
                emit_c_real_const(rhs, consts, real_consts, enums)?
            ))
        }
        ast::Expr::IfExpr {
            cond, then, els, ..
        } => Some(format!(
            "(({}) ? ({}) : ({}))",
            emit_c_const(cond, consts, enums)?,
            emit_c_real_const(then, consts, real_consts, enums)?,
            emit_c_real_const(els, consts, real_consts, enums)?
        )),
        ast::Expr::SuffixLit { text, suffix, .. }
            if suffix.text == "Hz" || suffix.text.ends_with("Hz") =>
        {
            let scale = ast::suffix_scale(&suffix.text)?;
            Some(format!("((double)({}) * {scale}.0)", text.replace('_', "")))
        }
        _ => None,
    }
}

/// Wrap a C expression so it masks to `w` bits (wrap at 2^w).
fn mask_c(e: &str, w: u32) -> String {
    if w > 0 {
        format!("sx_mask(({e}), {w})")
    } else {
        e.to_string()
    }
}

fn c_word_literal(words: &[u64]) -> String {
    let parts = words
        .iter()
        .enumerate()
        .filter(|(_, word)| **word != 0)
        .map(|(i, word)| {
            if i == 0 {
                format!("((sx_value){word}ULL)")
            } else {
                format!("(((sx_value){word}ULL) << {})", i * 64)
            }
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        "((sx_value)0)".to_string()
    } else {
        format!("({})", parts.join(" | "))
    }
}

fn parse_word_literal(text: &str) -> Vec<u64> {
    let text = text.trim().replace('_', "");
    if let Some(digits) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        parse_digits_words(digits, 16)
    } else if let Some(digits) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
        parse_digits_words(digits, 2)
    } else {
        parse_digits_words(&text, 10)
    }
}

fn parse_digits_words(digits: &str, radix: u32) -> Vec<u64> {
    let mut words = Vec::<u64>::new();
    for digit in digits.chars().filter(|c| *c != '_') {
        let Some(digit) = digit.to_digit(radix) else {
            return vec![0];
        };
        let mut carry = digit as u128;
        for word in &mut words {
            let next = (*word as u128) * radix as u128 + carry;
            *word = next as u64;
            carry = next >> 64;
        }
        if carry != 0 || words.is_empty() {
            words.push(carry as u64);
        }
    }
    words
}

impl Ctx<'_> {
    fn type_is(&self, ty: &ast::Type, expected: &str) -> bool {
        resolved_type_name(ty, self.type_aliases) == Some(expected)
    }

    fn type_name_is(&self, name: &str, expected: &str) -> bool {
        name == expected
            || self
                .type_aliases
                .get(name)
                .is_some_and(|ty| self.type_is(ty, expected))
    }

    /// A testbench local initialized from a `std::fs` file read
    /// (`let s: string = read_to_string("path")`, `let m: unsigned[8][N] = read(..)`).
    /// The file is read at **build time** (matching the corpus's stable fixtures)
    /// to size and fill the local: one `Char`/byte element per index. Returns
    /// `true` when handled. `read`/`read_to_string` in *initializer* position of
    /// a DUT signal is baked by the IR; this covers the testbench-local case.
    fn try_declare_fs_read_local(&self, l: &ast::LetDecl, b: &mut String) -> Result<bool, String> {
        let Some(value) = &l.value else {
            return Ok(false);
        };
        let (path, bytes) = match (
            fs_read_path(value, "read_to_string"),
            fs_read_path(value, "read"),
        ) {
            (Some(p), _) => (p, false),
            (_, Some(p)) => (p, true),
            _ => return Ok(false),
        };
        let full = self.design.base_dir.join(&path);
        let codes: Vec<u64> = if bytes {
            std::fs::read(&full)
                .map_err(|e| format!("read(\"{path}\"): {e}"))?
                .iter()
                .map(|&x| x as u64)
                .collect()
        } else {
            std::fs::read_to_string(&full)
                .map_err(|e| format!("read_to_string(\"{path}\"): {e}"))?
                .chars()
                .map(|c| c as u32 as u64)
                .collect()
        };
        let name = &l.name.text;
        if let Some(head) = l.ty.as_ref().and_then(type_head_name) {
            self.local_types
                .borrow_mut()
                .insert(name.clone(), head.to_string());
        }
        // The file is read here, at build time, so the element count is known
        // — but it was only ever spent laying out storage and then dropped, so
        // `'length` on the local had nothing to consult and failed as "not
        // known at compile time" for a length this function had in hand.
        self.local_indices
            .borrow_mut()
            .insert(name.clone(), (0..codes.len() as i64).collect());
        for (i, &code) in codes.iter().enumerate() {
            let key = format!("{name}[{i}]");
            if !bytes {
                self.local_types
                    .borrow_mut()
                    .insert(key.clone(), "Char".into());
            }
            // A connected element writes its signal; an unconnected local gets
            // its own C variable, registered so `name[i]` reads resolve to it.
            if let Some(&id) = self.map.get(&key) {
                b.push_str(&format!("    sx_set({}, {code}ULL);\n", id.0));
            } else {
                b.push_str(&format!(
                    "    uint64_t {} = {code}ULL;\n",
                    c_local_ident(&key)
                ));
                self.locals.borrow_mut().insert(key);
            }
        }
        Ok(true)
    }

    fn try_declare_string_local(&self, l: &ast::LetDecl, b: &mut String) -> Result<bool, String> {
        let Some(ty) = &l.ty else { return Ok(false) };
        if !is_single_string_type(ty) {
            return Ok(false);
        }
        let literal = match &l.value {
            Some(ast::Expr::StrLit { text, .. }) => Some(text.as_str()),
            _ => None,
        };
        let source = l
            .value
            .as_ref()
            .and_then(|value| self.c_string_elems(value));
        let declared_indices = match ty {
            ast::Type::Indexed {
                index: Some(index), ..
            } => index_values(index, self.const_ranges, self.consts),
            _ => None,
        };
        let indices = declared_indices
            .or_else(|| literal.map(|text| (0..text.chars().count() as i64).collect()))
            .or_else(|| {
                source
                    .as_ref()
                    .map(|values| (0..values.len() as i64).collect())
            })
            .unwrap_or_default();
        self.local_types
            .borrow_mut()
            .insert(l.name.text.clone(), "string".into());
        self.local_indices
            .borrow_mut()
            .insert(l.name.text.clone(), indices.clone());
        if let (Some(left), Some(right)) = (indices.first(), indices.last()) {
            self.local_ranges
                .borrow_mut()
                .insert(l.name.text.clone(), (*left, *right));
        }
        for (position, index) in indices.into_iter().enumerate() {
            let key = format!("{}[{index}]", l.name.text);
            self.local_types
                .borrow_mut()
                .insert(key.clone(), "Char".into());
            let value = literal
                .and_then(|text| text.chars().nth(position))
                .map(|ch| format!("{}ULL", ch as u32))
                .or_else(|| {
                    source
                        .as_ref()
                        .and_then(|values| values.get(position))
                        .cloned()
                })
                .unwrap_or_else(|| "0ULL".into());
            // Elaboration materializes a connected string as one signal per
            // character. Seed those signals directly so the DUT connection
            // sees the initializer; only genuinely unconnected strings need C
            // locals.
            if let Some(&id) = self.map.get(&key) {
                b.push_str(&format!("    sx_set({}, {value});\n", id.0));
            } else {
                b.push_str(&format!(
                    "    uint64_t {} = {value};\n",
                    c_local_ident(&key)
                ));
                self.locals.borrow_mut().insert(key);
            }
        }
        Ok(true)
    }

    /// Materialize a one-dimensional array whose element is a scalar or packed
    /// vector. Packed vectors (`unsigned[128]`) are leaves; an additional
    /// index (`unsigned[128][2]`) is the array dimension represented here.
    fn try_declare_array_local(&self, l: &ast::LetDecl, b: &mut String) -> Result<bool, String> {
        let Some(ty) = &l.ty else { return Ok(false) };
        if self.array_parts(ty).is_none() {
            return Ok(false);
        }
        self.declare_typed_storage(&l.name.text, ty, b)?;
        if let Some(value) = &l.value {
            if !self.write_composite(&l.name.text, value, b, "    ")? {
                return Err(format!(
                    "unsupported array initializer for `{}`",
                    l.name.text
                ));
            }
        }
        Ok(true)
    }

    /// Recursively flatten an array/field aggregate into typed scalar leaves.
    /// Connected leaves already exist in `map`; only unconnected leaves need C
    /// storage, but both receive metadata for formatting and target coercion.
    fn declare_typed_storage(
        &self,
        prefix: &str,
        ty: &ast::Type,
        b: &mut String,
    ) -> Result<(), String> {
        if let Some((left, right)) = type_index_bounds(ty, self.const_ranges, self.consts) {
            self.local_ranges
                .borrow_mut()
                .insert(prefix.to_string(), (left, right));
        }
        if let Some(indices) = sized_string_indices(ty, self.const_ranges, self.consts) {
            self.local_types
                .borrow_mut()
                .insert(prefix.to_string(), "string".into());
            self.local_indices
                .borrow_mut()
                .insert(prefix.to_string(), indices.clone());
            for index in indices {
                let element = format!("{prefix}[{index}]");
                self.local_types
                    .borrow_mut()
                    .insert(element.clone(), "Char".into());
                if !self.map.contains_key(&element) {
                    b.push_str(&format!("    uint64_t {} = 0;\n", c_local_ident(&element)));
                    self.locals.borrow_mut().insert(element);
                }
            }
            return Ok(());
        }
        if let Some((element, indices)) = self.array_parts(ty) {
            self.local_indices
                .borrow_mut()
                .insert(prefix.to_string(), indices.clone());
            for index in indices {
                self.declare_typed_storage(&format!("{prefix}[{index}]"), element, b)?;
            }
            return Ok(());
        }
        let head = type_head_name(ty);
        if let Some(fields) = head
            .and_then(|name| self.structs.get(name))
            .filter(|fields| !fields.is_empty())
        {
            if let Some(head) = head {
                self.local_types
                    .borrow_mut()
                    .insert(prefix.to_string(), head.to_string());
            }
            for (field, field_ty) in fields {
                self.declare_typed_storage(&format!("{prefix}.{field}"), field_ty, b)?;
            }
            return Ok(());
        }
        let family = self.declared_family(ty);
        let width = family
            .as_ref()
            .map(|(_, width)| *width)
            .or_else(|| self.declared_width(ty));
        if let Some((name, _)) = family {
            self.local_families
                .borrow_mut()
                .insert(prefix.to_string(), name);
        }
        if let Some(width) = width {
            self.local_widths
                .borrow_mut()
                .insert(prefix.to_string(), width);
        }
        if let Some(head) = head {
            self.local_types
                .borrow_mut()
                .insert(prefix.to_string(), head.to_string());
        }
        if !self.map.contains_key(prefix) {
            let c_ty = if width.is_some_and(|bits| bits > 64) {
                "sx_value"
            } else {
                "uint64_t"
            };
            b.push_str(&format!("    {c_ty} {} = 0;\n", c_local_ident(prefix)));
            self.locals.borrow_mut().insert(prefix.to_string());
        }
        Ok(())
    }

    /// Emit writes for a composite value assigned to a connected name — a
    /// string literal (`s = "hi"` -> one `Char` element per index) or a struct
    /// literal (`a = { .re = 3 }` -> one field signal each). Returns `true`
    /// when it handled `value`, `false` to fall through to scalar assignment.
    /// Materialize an *unconnected* struct-typed testbench local as one C local
    /// per field (`let p: Pkt;` -> `uint64_t sxl_p_a = 0, sxl_p_b = 0;`),
    /// recording each field's width/family and the receiver type. A struct
    /// literal initializer (`let p: Pkt = { .a = 1 };`) writes the fields. A
    /// *connected* struct port (fields in the signal map) returns `false` so the
    /// existing signal path handles it. Returns `true` when handled.
    fn try_declare_struct_local(&self, l: &ast::LetDecl, b: &mut String) -> Result<bool, String> {
        let Some(head) = l.ty.as_ref().and_then(|t| type_head_name(t)) else {
            return Ok(false);
        };
        // Only a genuine field-aggregate is expanded into per-field locals. A
        // type that *inherits from an array* — `unsigned`/`signed` (`struct unsigned :
        // Logic[]`) or a user enum vector (`: SomeEnum[]`) — carries no named
        // fields, so it is a scalar/vector leaf and flows through the scalar
        // path (check the base: an array parent means "vector", not "struct").
        let Some(fields) = self.structs.get(head).filter(|f| !f.is_empty()) else {
            return Ok(false);
        };
        let connected = self.map.contains_key(&l.name.text)
            || fields
                .iter()
                .any(|(f, _)| self.map.contains_key(&format!("{}.{}", l.name.text, f)));
        if connected {
            // The values live in signals, so there is nothing to declare — but
            // the leaves' declared types are still needed to display and
            // dispatch on them. Without this a connected `signed` field read
            // back as its bit pattern (253 for -3) while the identical field
            // on an unconnected struct read correctly.
            self.local_types
                .borrow_mut()
                .insert(l.name.text.clone(), head.to_string());
            for (f, fty) in fields {
                let key = format!("{}.{}", l.name.text, f);
                if let Some((fam, w)) = self.declared_family(fty) {
                    self.local_families.borrow_mut().insert(key.clone(), fam);
                    self.local_widths.borrow_mut().insert(key, w);
                } else if let Some(w) = self.declared_width(fty) {
                    self.local_widths.borrow_mut().insert(key, w);
                }
            }
            return Ok(false);
        }
        self.local_types
            .borrow_mut()
            .insert(l.name.text.clone(), head.to_string());
        let init: HashMap<&str, &ast::Expr> = match &l.value {
            Some(ast::Expr::Construct { args, .. }) => args
                .iter()
                .enumerate()
                .filter_map(|(i, a)| {
                    let v = a.value.as_ref()?;
                    // Positional args bind to the struct's field at position i.
                    let name = match &a.field {
                        Some(f) => f.text.as_str(),
                        None => fields.get(i).map(|(n, _)| n.as_str())?,
                    };
                    Some((name, v))
                })
                .collect(),
            // A positional name-less struct literal `{ 3, 4 }` lexes as a brace
            // concat; parts bind to fields by declaration order.
            Some(ast::Expr::Concat { parts, .. }) => parts
                .iter()
                .enumerate()
                .filter_map(|(i, e)| Some((fields.get(i).map(|(n, _)| n.as_str())?, e)))
                .collect(),
            _ => HashMap::new(),
        };
        // `{ ..base, .x = v }`: fields not overridden are copied from `base`.
        let spread_base: Option<String> = match &l.value {
            Some(ast::Expr::Construct {
                spread: Some(base), ..
            }) => expr_path(base),
            Some(value) => expr_path(value),
            _ => None,
        };
        self.declare_struct_fields(&l.name.text, fields, &init, spread_base.as_deref(), b)?;
        Ok(true)
    }

    /// Emit `uint64_t` locals for each field of a struct local, recursing into
    /// nested struct fields (`p.inner.x`). `init` supplies literal field values.
    fn declare_struct_fields(
        &self,
        prefix: &str,
        fields: &[(String, ast::Type)],
        init: &HashMap<&str, &ast::Expr>,
        spread_base: Option<&str>,
        b: &mut String,
    ) -> Result<(), String> {
        for (fname, fty) in fields {
            let key = format!("{prefix}.{fname}");
            // An indexed aggregate field keeps its array dimension. Looking
            // only at `type_head_name` would mistake `Child[2]` for one nested
            // `Child` and materialize `field.x` instead of
            // `field[0].x`, `field[1].x`.
            if self.array_parts(fty).is_some()
                || sized_string_indices(fty, self.const_ranges, self.consts).is_some()
            {
                self.declare_typed_storage(&key, fty, b)?;
                if let Some(value) = init.get(fname.as_str()) {
                    if !self.write_composite(&key, value, b, "    ")? {
                        return Err(format!("cannot initialize aggregate struct field `{key}`"));
                    }
                } else if let Some(base) = spread_base {
                    let source_name = format!("{base}.{fname}");
                    let source = self.composite_reads(&source_name);
                    let target = self.composite_targets(&key);
                    if source.keys().ne(target.keys()) {
                        return Err(format!(
                            "composite assignment shape mismatch: `{key}` and `{source_name}`"
                        ));
                    }
                    for (suffix, expression) in source {
                        let (_, destination) = &target[&suffix];
                        b.push_str(&format!("    {destination} = {expression};\n"));
                    }
                }
                continue;
            }
            // A nested *field-aggregate* field expands to its own leaves; a
            // field that inherits from an array (a `unsigned`/`signed`/enum vector,
            // which has no fields) is a scalar leaf.
            let fhead = type_head_name(fty);
            if let Some(sub) = fhead
                .and_then(|h| self.structs.get(h))
                .filter(|f| !f.is_empty())
            {
                self.local_types
                    .borrow_mut()
                    .insert(key.clone(), fhead.unwrap().to_string());
                let nested = init.get(fname.as_str()).copied();
                let sub_init: HashMap<&str, &ast::Expr> = match nested {
                    Some(ast::Expr::Construct { args, .. }) => args
                        .iter()
                        .enumerate()
                        .filter_map(|(position, argument)| {
                            let value = argument.value.as_ref()?;
                            let field = argument
                                .field
                                .as_ref()
                                .map(|field| field.text.as_str())
                                .or_else(|| sub.get(position).map(|(field, _)| field.as_str()))?;
                            Some((field, value))
                        })
                        .collect(),
                    Some(ast::Expr::Concat { parts, .. }) => parts
                        .iter()
                        .enumerate()
                        .filter_map(|(position, value)| {
                            Some((sub.get(position)?.0.as_str(), value))
                        })
                        .collect(),
                    _ => HashMap::new(),
                };
                let explicit_base = match nested {
                    Some(ast::Expr::Construct {
                        spread: Some(base), ..
                    }) => expr_path(base),
                    Some(value) => expr_path(value),
                    None => None,
                };
                let inherited_base = spread_base.map(|base| format!("{base}.{fname}"));
                let sub_base = if nested.is_some() {
                    explicit_base
                } else {
                    inherited_base
                };
                self.declare_struct_fields(&key, sub, &sub_init, sub_base.as_deref(), b)?;
                continue;
            }
            let leaf_width = if let Some((fam, w)) = self.declared_family(fty) {
                self.local_families.borrow_mut().insert(key.clone(), fam);
                self.local_widths.borrow_mut().insert(key.clone(), w);
                Some(w)
            } else if let Some(w) = self.declared_width(fty) {
                self.local_widths.borrow_mut().insert(key.clone(), w);
                Some(w)
            } else {
                None
            };
            if let Some(head) = fhead {
                self.local_types
                    .borrow_mut()
                    .insert(key.clone(), head.to_string());
            }
            let init_e = match init.get(fname.as_str()) {
                Some(v) => {
                    let e = self.value_for_local(&key, v)?;
                    match self.local_widths.borrow().get(&key) {
                        Some(&w) => mask_c(&e, w),
                        None => e,
                    }
                }
                // Not overridden: copy the spread base's field if it exists
                // (a declared struct local), else default 0.
                None => match spread_base {
                    Some(bp) if self.locals.borrow().contains(&format!("{bp}.{fname}")) => {
                        c_local_ident(&format!("{bp}.{fname}"))
                    }
                    _ => "0".to_string(),
                },
            };
            let c_ty = if leaf_width.is_some_and(|width| width > 64) {
                "sx_value"
            } else {
                "uint64_t"
            };
            b.push_str(&format!("    {c_ty} {} = {init_e};\n", c_local_ident(&key)));
            self.locals.borrow_mut().insert(key);
        }
        Ok(())
    }

    fn write_composite(
        &self,
        name: &str,
        value: &ast::Expr,
        b: &mut String,
        ind: &str,
    ) -> Result<bool, String> {
        if let Some(source_name) = expr_path(value) {
            let source = self.composite_reads(&source_name);
            if !source.is_empty() {
                let target = self.composite_targets(name);
                if source.keys().ne(target.keys()) {
                    return Err(format!(
                        "composite assignment shape mismatch: `{name}` and `{source_name}`"
                    ));
                }
                for (suffix, expression) in source {
                    let (signal, destination) = &target[&suffix];
                    if *signal {
                        b.push_str(&format!("{ind}sx_set({destination}, {expression});\n"));
                    } else {
                        b.push_str(&format!("{ind}{destination} = {expression};\n"));
                    }
                }
                return Ok(true);
            }
        }
        if !matches!(value, ast::Expr::StrLit { .. }) {
            if let Some(source) = self.c_string_elems(value) {
                let mut target = Vec::new();
                while let Some(destination) = {
                    let element = format!("{name}[{}]", target.len());
                    self.map
                        .get(&element)
                        .map(|id| (true, id.0.to_string()))
                        .or_else(|| {
                            self.locals
                                .borrow()
                                .contains(&element)
                                .then(|| (false, c_local_ident(&element)))
                        })
                } {
                    target.push(destination);
                }
                let target_is_empty_string = target.is_empty()
                    && self.local_types.borrow().get(name).map(String::as_str) == Some("string");
                if !target.is_empty() || target_is_empty_string {
                    if target.len() != source.len() {
                        return Err(format!(
                            "array assignment length mismatch: `{name}` has {} element(s), source has {}",
                            target.len(),
                            source.len()
                        ));
                    }
                    for ((signal, destination), expression) in target.iter().zip(source) {
                        if *signal {
                            b.push_str(&format!("{ind}sx_set({destination}, {expression});\n"));
                        } else {
                            b.push_str(&format!("{ind}{destination} = {expression};\n"));
                        }
                    }
                    return Ok(true);
                }
            }
        }
        match value {
            ast::Expr::Array { elems, .. } => {
                let indices = self
                    .local_indices
                    .borrow()
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| {
                        let mut indices = Vec::new();
                        while self.has_storage_prefix(&format!("{name}[{}]", indices.len())) {
                            indices.push(indices.len() as i64);
                        }
                        indices
                    });
                if indices.len() != elems.len() {
                    return Err(format!(
                        "array assignment length mismatch: `{name}` has {} element(s), source has {}",
                        indices.len(),
                        elems.len()
                    ));
                }
                for (index, value) in indices.into_iter().zip(elems) {
                    let element = format!("{name}[{index}]");
                    if self.write_composite(&element, value, b, ind)? {
                        continue;
                    }
                    if let Some(&id) = self.map.get(&element) {
                        b.push_str(&format!(
                            "{ind}sx_set({}, {});\n",
                            id.0,
                            self.value_for(id, value)?
                        ));
                    } else {
                        let expression = self.value_for_local(&element, value)?;
                        let expression = match self.local_widths.borrow().get(&element) {
                            Some(&width) => mask_c(&expression, width),
                            None => expression,
                        };
                        b.push_str(&format!(
                            "{ind}{} = {expression};\n",
                            c_local_ident(&element)
                        ));
                    }
                }
                Ok(true)
            }
            ast::Expr::StrLit { text, .. } => {
                let source_len = text.chars().count();
                let indices = self
                    .local_indices
                    .borrow()
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| (0..source_len as i64).collect());
                if indices.len() != source_len {
                    return Err(format!(
                        "string assignment length mismatch: `{name}` has {} element(s), source has {source_len}",
                        indices.len()
                    ));
                }
                for (index, ch) in indices.into_iter().zip(text.chars()) {
                    let element = format!("{name}[{index}]");
                    let value = self.char_value_for_target(&element, ch);
                    if let Some(&id) = self.map.get(&element) {
                        b.push_str(&format!("{ind}sx_set({}, {value}ULL);\n", id.0));
                    } else if self.locals.borrow().contains(&element) {
                        let expression = match self.local_widths.borrow().get(&element) {
                            Some(&width) => mask_c(&format!("{value}ULL"), width),
                            None => format!("{value}ULL"),
                        };
                        b.push_str(&format!(
                            "{ind}{} = {expression};\n",
                            c_local_ident(&element)
                        ));
                    }
                }
                Ok(true)
            }
            ast::Expr::Construct {
                ty, args, spread, ..
            } => {
                let type_name = ty
                    .as_ref()
                    .and_then(type_head_name)
                    .map(str::to_string)
                    .or_else(|| self.local_types.borrow().get(name).cloned());
                let fields = type_name
                    .as_ref()
                    .and_then(|head| self.structs.get(head))
                    .cloned()
                    .unwrap_or_default();
                let mut wrote = false;

                if let Some(source_name) = spread.as_deref().and_then(expr_path) {
                    let source = self.composite_reads(&source_name);
                    let target = self.composite_targets(name);
                    if source.keys().ne(target.keys()) {
                        return Err(format!(
                            "composite assignment shape mismatch: `{name}` and `{source_name}`"
                        ));
                    }
                    for (suffix, expression) in source {
                        let (signal, destination) = &target[&suffix];
                        if *signal {
                            b.push_str(&format!("{ind}sx_set({destination}, {expression});\n"));
                        } else {
                            b.push_str(&format!("{ind}{destination} = {expression};\n"));
                        }
                    }
                    wrote = true;
                }

                for (position, arg) in args.iter().enumerate() {
                    let field_name = arg
                        .field
                        .as_ref()
                        .map(|field| field.text.as_str())
                        .or_else(|| fields.get(position).map(|(field, _)| field.as_str()));
                    let Some(field_name) = field_name else {
                        return Err(format!(
                            "cannot bind positional field {} while assigning `{name}`",
                            position + 1
                        ));
                    };
                    let Some(value) = &arg.value else { continue };
                    wrote |=
                        self.write_composite_field(&format!("{name}.{field_name}"), value, b, ind)?;
                }
                Ok(wrote)
            }
            ast::Expr::Concat { parts, .. } => {
                let type_name = self.local_types.borrow().get(name).cloned();
                let fields = type_name
                    .as_ref()
                    .and_then(|head| self.structs.get(head))
                    .cloned()
                    .unwrap_or_default();
                if fields.is_empty() {
                    return Ok(false);
                }
                if fields.len() != parts.len() {
                    return Err(format!(
                        "struct assignment field count mismatch: `{name}` has {} field(s), source has {}",
                        fields.len(),
                        parts.len()
                    ));
                }
                for ((field, _), value) in fields.iter().zip(parts) {
                    self.write_composite_field(&format!("{name}.{field}"), value, b, ind)?;
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn write_composite_field(
        &self,
        field: &str,
        value: &ast::Expr,
        b: &mut String,
        ind: &str,
    ) -> Result<bool, String> {
        if self.write_composite(field, value, b, ind)? {
            return Ok(true);
        }
        if let Some(&id) = self.map.get(field) {
            b.push_str(&format!(
                "{ind}sx_set({}, {});\n",
                id.0,
                self.value_for(id, value)?
            ));
            return Ok(true);
        }
        if self.locals.borrow().contains(field) {
            let expression = self.value_for_local(field, value)?;
            let expression = match self.local_widths.borrow().get(field) {
                Some(&width) => mask_c(&expression, width),
                None => expression,
            };
            b.push_str(&format!("{ind}{} = {expression};\n", c_local_ident(field)));
            return Ok(true);
        }
        Ok(false)
    }

    fn has_storage_prefix(&self, prefix: &str) -> bool {
        let descendant = |name: &str| {
            name == prefix
                || name
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('[') || rest.starts_with('.'))
        };
        self.map.keys().any(|name| descendant(name))
            || self.locals.borrow().iter().any(|name| descendant(name))
    }

    /// Scalar descendant reads keyed by their suffix relative to `prefix`.
    fn composite_reads(&self, prefix: &str) -> BTreeMap<String, String> {
        let mut reads = BTreeMap::new();
        for (name, id) in self.map {
            if let Some(suffix) = descendant_suffix(name, prefix) {
                reads.insert(suffix.to_string(), format!("sx_read({})", id.0));
            }
        }
        for name in self.locals.borrow().iter() {
            if let Some(suffix) = descendant_suffix(name, prefix) {
                reads.insert(suffix.to_string(), c_local_ident(name));
            }
        }
        reads
    }

    /// Scalar descendant write destinations keyed like `composite_reads`.
    fn composite_targets(&self, prefix: &str) -> BTreeMap<String, (bool, String)> {
        let mut targets = BTreeMap::new();
        for (name, id) in self.map {
            if let Some(suffix) = descendant_suffix(name, prefix) {
                targets.insert(suffix.to_string(), (true, id.0.to_string()));
            }
        }
        for name in self.locals.borrow().iter() {
            if let Some(suffix) = descendant_suffix(name, prefix) {
                targets.insert(suffix.to_string(), (false, c_local_ident(name)));
            }
        }
        targets
    }

    /// The declared `(family, width)` of a vector-family type (`signed[8]` ->
    /// ("signed", 8)). Mirrors the runner's rule.
    fn declared_family(&self, ty: &ast::Type) -> Option<(String, u32)> {
        if let ast::Type::Indexed {
            base,
            index: Some(i),
            ..
        } = ty
        {
            if matches!(base.as_ref(), ast::Type::Indexed { .. }) {
                return self.declared_family(base);
            }
            let head = match base.as_ref() {
                ast::Type::Path(p) => p.segments.last().map(|s| s.text.as_str())?,
                _ => return None,
            };
            if !self.families.contains(head) {
                return None;
            }
            return Some((
                head.to_string(),
                u32::try_from(index_values(i, self.const_ranges, self.consts)?.len()).ok()?,
            ));
        }
        None
    }

    /// The outer array element type and count, excluding a packed vector's own
    /// width index (`unsigned[128]` is one value; `unsigned[128][2]` is two).
    fn array_parts<'b>(&self, ty: &'b ast::Type) -> Option<(&'b ast::Type, Vec<i64>)> {
        let ast::Type::Indexed {
            base,
            index: Some(index),
            ..
        } = ty
        else {
            return None;
        };
        if let ast::Type::Path(path) = base.as_ref() {
            let head = path.segments.last()?.text.as_str();
            if self.families.contains(head) {
                return None;
            }
        }
        Some((base, index_values(index, self.const_ranges, self.consts)?))
    }

    /// The bit width a testbench name carries: a local's declared width or the
    /// connected signal's.
    fn name_width(&self, name: &str) -> Option<u32> {
        if let Some(&w) = self.local_widths.borrow().get(name) {
            return Some(w);
        }
        self.map
            .get(name)
            .map(|&id| self.design.signals[id.0 as usize].width)
    }

    /// Translate `lhs op rhs` through the lhs family's operator impl as an
    /// inline C expression, when one exists — the native mirror of the
    /// runner's `dispatch_binop`. Comparisons derive from `Ord::cmp`.
    /// The `(family, width)` an operand dispatches on, for the shapes that
    /// carry one indirectly. Kept separate from `c_dispatch_binop`'s own match
    /// so a branch can be asked the same question its parent was.
    fn dispatch_operand_family(&self, e: &ast::Expr) -> Option<(String, Option<u32>)> {
        if let Some((fam, w)) = self.conversion_target(e) {
            return Some((fam, Some(w)));
        }
        match e {
            ast::Expr::IfExpr { then, els, .. } => self
                .dispatch_operand_family(then)
                .or_else(|| self.dispatch_operand_family(els)),
            ast::Expr::Match { arms, .. } => arms
                .iter()
                .filter_map(|a| a.value_expr())
                .find_map(|v| self.dispatch_operand_family(v)),
            // Arithmetic keeps its operands' family, so `(a - b) < 0` has to
            // dispatch the same `Ord` that `d < 0` does after `let d = a - b`.
            // Without this the inline form fell through to an unsigned compare
            // and a negative result tested as positive.
            ast::Expr::Binary { op, lhs, rhs, .. } if op.keeps_operand_family() => self
                .dispatch_operand_family(lhs)
                .or_else(|| self.dispatch_operand_family(rhs)),
            _ => {
                // Any named value: a plain local, or a struct leaf like `p.x`
                // (an `Expr::Field`, which a shape match misses — a connected
                // `signed` field then compared unsigned).
                if let Some(name) = expr_path(e) {
                    let family = self
                        .local_families
                        .borrow()
                        .get(&name)
                        .cloned()
                        .or_else(|| self.local_types.borrow().get(&name).cloned())?;
                    return Some((family, self.name_width(&name)));
                }
                // A call has no name to look up, so its declared return type
                // supplies both family and width; without it a call never
                // dispatched an impl and `neg(x) < 0` compared unsigned.
                let ret = self.call_return_type(e)?;
                let head = type_head_name(&ret)?;
                Some((head.to_string(), self.declared_width(&ret)))
            }
        }
    }

    fn c_dispatch_binop(
        &self,
        op: &ast::BinOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
    ) -> Result<Option<String>, String> {
        // The family decides which operator impl runs; the width is only ever
        // needed to bind `self'length` inside it. Both come from the one
        // place that knows how each expression shape carries a family —
        // asking the question here a second time is how `Expr::Field` and
        // then `Expr::Binary` ended up answered in one and not the other.
        let Some((fam, lwidth)) = self.dispatch_operand_family(lhs) else {
            return Ok(None);
        };
        let op_str = siox::syntax::pretty::bin_op(op);
        // `==`/`!=`: bit equality at the type's width (mask both sides).
        if matches!(op_str, "==" | "!=") {
            let Some(w) = lwidth else {
                return Ok(None);
            };
            if w == 0 || w >= 64 {
                return Ok(None);
            }
            let a = mask_c(&self.expr(lhs)?, w);
            let b = mask_c(&self.expr(rhs)?, w);
            return Ok(Some(format!(
                "(({a}) {} ({b}))",
                if op_str == "==" { "==" } else { "!=" }
            )));
        }
        // `Ordering` discriminants from std's enum, not a baked-in 0/2.
        let ord = |v: &str, fallback: u64| {
            self.enums
                .get("Ordering")
                .and_then(|m| m.get(v))
                .copied()
                .unwrap_or(fallback)
        };
        let cmp = match op_str {
            "<" => Some((ord("Less", 0), false)),
            ">" => Some((ord("Greater", 2), false)),
            ">=" => Some((ord("Less", 0), true)),
            "<=" => Some((ord("Greater", 2), true)),
            _ => None,
        };
        let tr = match cmp {
            Some(_) => "<=>",
            None => op_str,
        };
        let Some(candidates) = self.op_impls.get(&(tr.to_string(), fam.clone())) else {
            return Ok(None);
        };
        let rhs_type = match rhs {
            ast::Expr::Path(p) if p.segments.len() == 1 => self
                .local_families
                .borrow()
                .get(&p.segments[0].text)
                .cloned()
                .or_else(|| self.local_types.borrow().get(&p.segments[0].text).cloned()),
            ast::Expr::Int { .. } => Some("integer".to_string()),
            _ => None,
        };
        let selected = rhs_type
            .as_deref()
            .and_then(|rhs| {
                candidates.iter().find(|(_, input)| {
                    input.as_deref() == Some(rhs)
                        || (input.as_deref() == Some("Self") && rhs == fam)
                })
            })
            .or_else(|| (candidates.len() == 1).then(|| &candidates[0]));
        let Some((f, _)) = selected else {
            return Ok(None);
        };
        let Some(body) = f.body.as_ref() else {
            return Ok(None);
        };

        let w = lwidth.unwrap_or(0);
        let mut env = HashMap::new();
        env.insert("self".to_string(), format!("({})", self.expr(lhs)?));
        env.insert("self::length".to_string(), format!("{w}ULL"));
        if let Some(pdecl) = f.params.iter().find(|p| !p.is_self) {
            if let Some(n) = &pdecl.name {
                let rw = match rhs {
                    ast::Expr::Path(p) if p.segments.len() == 1 => {
                        self.name_width(&p.segments[0].text).unwrap_or(w)
                    }
                    _ => w,
                };
                env.insert(n.text.clone(), format!("({})", self.expr(rhs)?));
                env.insert(format!("{}::length", n.text), format!("{rw}ULL"));
            }
        }
        self.fn_env.borrow_mut().push(env);
        self.fn_type_env.borrow_mut().push(HashMap::new());
        let out = self.c_fn_stmts(&body.stmts, false);
        self.fn_type_env.borrow_mut().pop();
        self.fn_env.borrow_mut().pop();
        let r = out?;
        Ok(Some(match cmp {
            Some((want, ne)) => {
                format!("(({r}) {} {want}ULL)", if ne { "!=" } else { "==" })
            }
            None => mask_c(&r, w),
        }))
    }

    fn c_dispatch_not(&self, rhs: &ast::Expr) -> Result<Option<String>, String> {
        let ast::Expr::Path(p) = rhs else {
            return Ok(None);
        };
        if p.segments.len() != 1 {
            return Ok(None);
        }
        let name = p.segments[0].text.clone();
        let family = self
            .local_families
            .borrow()
            .get(&name)
            .cloned()
            .or_else(|| self.local_types.borrow().get(&name).cloned());
        let Some(family) = family else {
            return Ok(None);
        };
        // A packed Vector forwards the blanket `T[]` implementation of core
        // `not`; the native harness performs that element-wise operation
        // directly at the concrete width.
        if self.families.contains(&family) {
            let width = self.name_width(&name).unwrap_or(0);
            if width > 0 {
                return Ok(Some(mask_c(&format!("~({})", self.expr(rhs)?), width)));
            }
        }
        let Some((f, _)) = self
            .op_impls
            .get(&("not".to_string(), family))
            .and_then(|candidates| candidates.first())
        else {
            return Ok(None);
        };
        let Some(body) = f.body.as_ref() else {
            return Ok(None);
        };
        let width = self.name_width(&name).unwrap_or(0);
        let mut env = HashMap::new();
        env.insert("self".to_string(), format!("({})", self.expr(rhs)?));
        env.insert("self::length".to_string(), format!("{width}ULL"));
        self.fn_env.borrow_mut().push(env);
        self.fn_type_env.borrow_mut().push(HashMap::new());
        let out = self.c_fn_stmts(&body.stmts, false);
        self.fn_type_env.borrow_mut().pop();
        self.fn_env.borrow_mut().pop();
        let value = out?;
        Ok(Some(if width > 0 {
            mask_c(&value, width)
        } else {
            value
        }))
    }

    /// The width of a call argument, for binding `param'length` at an inline.
    /// A named value carries its declared width; anything else has none to
    /// give and the body falls back to its own rules.
    fn arg_width(&self, a: &ast::Expr) -> Option<u32> {
        if let Some(w) = expr_path(a).and_then(|p| self.name_width(&p)) {
            return Some(w);
        }
        // Only a *named* argument had a width, so `x'length` inside an inlined
        // body had nothing to consult when the argument was an expression:
        // `sext(a)` worked and `sext(a + 0)` did not, though both are the same
        // eight bits. A conversion states its width, arithmetic keeps its
        // operands', a branch takes its branches' — the shapes operator
        // dispatch already walks, so ask the same question there.
        self.dispatch_operand_family(a).and_then(|(_, w)| w)
    }

    /// The declared bit width of a vector-family type: `unsigned[8]` -> 8 (and the
    /// element width of an array of one). Mirrors the runner's rule.
    fn declared_width(&self, ty: &ast::Type) -> Option<u32> {
        // A bare derived-vector type (`struct Byte : Logic[8]`) inherits its
        // base array's width.
        if let ast::Type::Path(p) = ty {
            if let Some(seg) = p.segments.last() {
                if let Some(&w) = self.derived_widths.get(&seg.text) {
                    return Some(w);
                }
            }
        }
        if let ast::Type::Indexed {
            base,
            index: Some(i),
            ..
        } = ty
        {
            if matches!(base.as_ref(), ast::Type::Indexed { .. }) {
                return self.declared_width(base);
            }
            let head = match base.as_ref() {
                ast::Type::Path(p) => p.segments.last().map(|s| s.text.as_str())?,
                _ => return None,
            };
            if !self.families.contains(head) {
                return None;
            }
            return u32::try_from(index_values(i, self.const_ranges, self.consts)?.len()).ok();
        }
        None
    }

    /// `signed test_<name>(void) { ... }` — 0 on pass, 1 on the first failed
    /// assert (printing its message first, like a panic).
    fn gen_test_fn(&self, items: &[&ast::ImplItem]) -> Result<String, String> {
        let mut b = String::new();
        b.push_str(&format!(
            "signed test_{}(void) {{\n    g_range_failed = 0;\n    sx_reset();\n    sx_vcd_begin_test();\n",
            self.name
        ));

        // The test's event wheel: sim time + per-clock next-edge state. Arrays
        // are sized >=1 so clock-less tests still compile; `_nclk` grows as
        // `clock()` statements register (source order matches scan order).
        let n = self.clocks.len().max(1);
        let cid: Vec<String> = self.clocks.iter().map(|(c, _)| c.to_string()).collect();
        let half: Vec<String> = self.clocks.iter().map(|(_, h)| format!("{h}ULL")).collect();
        b.push_str(&format!(
            "    uint64_t _now = 0; (void)_now;\n             \x20   uint64_t _next[{n}] = {{{}}}; (void)_next;\n             \x20   static const uint32_t _cid[{n}] = {{{}}};\n             \x20   static const uint64_t _half[{n}] = {{{}}};\n             \x20   signed _nclk = 0; (void)_nclk;\n",
            vec!["0"; n].join(", "),
            if cid.is_empty() { "0".to_string() } else { cid.join(", ") },
            if half.is_empty() { "0".to_string() } else { half.join(", ") },
        ));

        // One pass in source order (sequential `let` semantics, mirroring
        // the runner): connected lets write signals, unconnected scalars
        // become C locals, and a settle precedes the first statement.
        let mut started = false;
        for item in items {
            match item {
                // A DUT instance (any declaration form) is wired by
                // elaboration; the testbench let emits nothing.
                ast::ImplItem::Let(l) if self.instance_names.contains(&l.name.text) => {}
                ast::ImplItem::Let(l) if self.try_declare_fs_read_local(l, &mut b)? => {}
                ast::ImplItem::Let(l) if self.try_declare_string_local(l, &mut b)? => {}
                ast::ImplItem::Let(l) if self.try_declare_array_local(l, &mut b)? => {}
                ast::ImplItem::Let(l) if self.try_declare_struct_local(l, &mut b)? => {}
                ast::ImplItem::Let(l) => match &l.value {
                    Some(ast::Expr::Construct { ty: Some(_), .. }) => {} // instance
                    value => {
                        // Record the vector family for every declared name
                        // (connected ports too): operators dispatch on it.
                        if let Some((fam, _)) = l.ty.as_ref().and_then(|t| self.declared_family(t))
                        {
                            self.local_families
                                .borrow_mut()
                                .insert(l.name.text.clone(), fam);
                        }
                        if let Some((left, right)) = l
                            .ty
                            .as_ref()
                            .and_then(|ty| type_index_bounds(ty, self.const_ranges, self.consts))
                        {
                            self.local_ranges
                                .borrow_mut()
                                .insert(l.name.text.clone(), (left, right));
                        }
                        if let Some(head) = l.ty.as_ref().and_then(type_head_name) {
                            self.local_types
                                .borrow_mut()
                                .insert(l.name.text.clone(), head.to_string());
                        }
                        // A string/struct-literal initializer on a connected
                        // name writes each element/field.
                        if let Some(v) = value {
                            if self.write_composite(&l.name.text, v, &mut b, "    ")? {
                                continue;
                            }
                        }
                        if let Some(&id) = self.map.get(&l.name.text) {
                            if let Some(v) = value {
                                let e = self.value_for(id, v)?;
                                b.push_str(&format!("    sx_set({}, {e});\n", id.0));
                            }
                        } else {
                            let e = match value {
                                // A char literal on an enum-typed local resolves
                                // by position in that enum (data-driven), like
                                // the native harness + hardware paths.
                                Some(v) => self.value_for_local(&l.name.text, v)?,
                                // Uninitialized: the type's `new()` default
                                // (`Logic` -> `'U'`), matching native + hardware.
                                None => {
                                    l.ty.as_ref()
                                        .and_then(type_head_name)
                                        .and_then(|h| self.design.new_defaults.get(h))
                                        .map(|v| format!("{v}ULL"))
                                        .unwrap_or_else(|| "0".to_string())
                                }
                            };
                            // A vector-family local wraps at its declared
                            // width, like the equivalent hardware signal.
                            let declared_width = l.ty.as_ref().and_then(|t| self.declared_width(t));
                            let e = match declared_width {
                                Some(w) => {
                                    self.local_widths
                                        .borrow_mut()
                                        .insert(l.name.text.clone(), w);
                                    mask_c(&e, w)
                                }
                                None => e,
                            };
                            let c_ty = if declared_width.is_some_and(|w| w > 64) {
                                "sx_value"
                            } else {
                                "uint64_t"
                            };
                            b.push_str(&format!("    {c_ty} {} = {e};\n", l.name.text));
                            self.locals.borrow_mut().insert(l.name.text.clone());
                            // Record an enum/Logic local's type so `print!`
                            // renders its symbol, not the raw discriminant.
                            if let Some(h) = l.ty.as_ref().and_then(|t| type_head_name(t)) {
                                if self.enums.contains_key(h) {
                                    self.local_types
                                        .borrow_mut()
                                        .insert(l.name.text.clone(), h.to_string());
                                }
                            }
                        }
                    }
                },
                ast::ImplItem::Stmt(st) => {
                    if !started {
                        b.push_str("    sx_settle();\n");
                        started = true;
                    }
                    self.stmt(st, &mut b, 1)?;
                }
                _ => {}
            }
        }
        if !started {
            b.push_str("    sx_settle();\n");
        }
        self.locals.borrow_mut().clear();

        b.push_str("    if (sx_check_ranges()) return 1;\n    return 0;\n}\n\n");
        // Post-settle range asserts (values persist, so checking at the next
        // settle also catches violations that occur inside await loops).
        let b = b.replace(
            "sx_settle();",
            "sx_run_settle(_now); if (sx_check_ranges()) return 1;",
        );
        Ok(b)
    }

    fn stmt(&self, s: &ast::Stmt, b: &mut String, depth: usize) -> Result<(), String> {
        let ind = "    ".repeat(depth);
        match s {
            ast::Stmt::Assign {
                target,
                value,
                after,
                ..
            } => {
                if after.is_some() {
                    // `clk = !clk after d;` registers on the event wheel; other
                    // delayed writes aren't compiled yet.
                    let (path, _) = after_toggle(target, value, after)?
                        .ok_or("only the `clk = not clk after d` form of `after` is supported in a native test executable")?;
                    if !self.map.contains_key(&path) {
                        return Err(format!("unknown signal `{path}`"));
                    }
                    for id in self.aliases.get(&path).map(|v| v.as_slice()).unwrap_or(&[]) {
                        if let Some(i) = self.clocks.iter().position(|(c, _)| *c == id.0) {
                            b.push_str(&format!(
                                "{ind}_next[{i}] = _now + {}ULL; _nclk = {}; sx_settle();\n",
                                self.clocks[i].1,
                                i + 1
                            ));
                        }
                    }
                    return Ok(());
                }
                let name = expr_path(target).ok_or("unsupported assignment target")?;
                // A string or struct literal writes several element/field
                // signals (`s = "hi"`, `a = { .re = 3 }`).
                if self.write_composite(&name, value, b, &ind)? {
                    b.push_str(&format!("{ind}sx_settle();\n"));
                    return Ok(());
                }
                if self.locals.borrow().contains(&name) {
                    let e = self.value_for_local(&name, value)?;
                    let e = match self.local_widths.borrow().get(&name) {
                        Some(&w) => mask_c(&e, w),
                        None => e,
                    };
                    b.push_str(&format!("{ind}{} = {e};\n", c_local_ident(&name)));
                    return Ok(());
                }
                let id = *self
                    .map
                    .get(&name)
                    .ok_or_else(|| format!("unknown signal `{name}`"))?;
                let e = self.value_for(id, value)?;
                // Drive every port this name connects to (sx_set masks to each
                // signal's width).
                b.push_str(&format!("{ind}{{ sx_value _v = {e};"));
                for a in self.aliases.get(&name).map(|v| v.as_slice()).unwrap_or(&[]) {
                    b.push_str(&format!(" sx_set({}, _v);", a.0));
                }
                b.push_str(&format!(" }}\n{ind}sx_settle();\n"));
                let _ = id;
            }
            ast::Stmt::Expr(ast::Expr::Call {
                callee, args, bang, ..
            }) => {
                self.call(callee, args, *bang, b, depth)?;
            }
            ast::Stmt::For {
                var, range, body, ..
            } => {
                let v = &var.text;
                // `for x in xs`: iterate a DUT-connected array via an id table.
                if let Some((path, n)) =
                    expr_path(range).and_then(|p| self.array_len(&p).map(|n| (p, n)))
                {
                    let k = self.tmp.get();
                    self.tmp.set(k + 1);
                    let indices = self
                        .local_indices
                        .borrow()
                        .get(&path)
                        .cloned()
                        .unwrap_or_else(|| (0..n as i64).collect());
                    let values: Vec<String> = indices
                        .into_iter()
                        .map(|index| {
                            let element = format!("{path}[{index}]");
                            if let Some(id) = self.map.get(&element) {
                                format!("sx_read({})", id.0)
                            } else {
                                c_local_ident(&element)
                            }
                        })
                        .collect();
                    b.push_str(&format!(
                        "{ind}{{ sx_value _a{k}[] = {{{}}};\n\
                         {ind}for (signed _i{k} = 0; _i{k} < {n}; _i{k}++) {{ \
                         sx_value {v} = _a{k}[_i{k}];\n",
                        values.join(", ")
                    ));
                    let fresh = self.locals.borrow_mut().insert(v.clone());
                    let first_element = self
                        .local_indices
                        .borrow()
                        .get(&path)
                        .and_then(|indices| indices.first().copied())
                        .unwrap_or(0);
                    let element = format!("{path}[{first_element}]");
                    let loop_type = self.local_types.borrow().get(&element).cloned();
                    let loop_family = self.local_families.borrow().get(&element).cloned();
                    let loop_width = self.local_widths.borrow().get(&element).copied();
                    let previous_type = self.local_types.borrow_mut().remove(v);
                    let previous_family = self.local_families.borrow_mut().remove(v);
                    let previous_width = self.local_widths.borrow_mut().remove(v);
                    if let Some(ty) = loop_type {
                        self.local_types.borrow_mut().insert(v.clone(), ty);
                    }
                    if let Some(family) = loop_family {
                        self.local_families.borrow_mut().insert(v.clone(), family);
                    }
                    if let Some(width) = loop_width {
                        self.local_widths.borrow_mut().insert(v.clone(), width);
                    }
                    for s in &body.stmts {
                        self.stmt(s, b, depth + 1)?;
                    }
                    self.local_types.borrow_mut().remove(v);
                    self.local_families.borrow_mut().remove(v);
                    self.local_widths.borrow_mut().remove(v);
                    if let Some(previous) = previous_type {
                        self.local_types.borrow_mut().insert(v.clone(), previous);
                    }
                    if let Some(previous) = previous_family {
                        self.local_families.borrow_mut().insert(v.clone(), previous);
                    }
                    if let Some(previous) = previous_width {
                        self.local_widths.borrow_mut().insert(v.clone(), previous);
                    }
                    if fresh {
                        self.locals.borrow_mut().remove(v);
                    }
                    b.push_str(&format!("{ind}}} }}\n"));
                    return Ok(());
                }
                let (lo, hi) = match range {
                    ast::Expr::Range { lo, hi, .. } => (self.expr(lo)?, self.expr(hi)?),
                    _ => return Err("`for` needs a range or an array".into()),
                };
                // Inclusive, directional range (`0..2` -> 0,1,2; `2..0` -> 2,1,0):
                // step by the sign of hi-lo and break *after* running the body at
                // `hi`. The counter is signed so a descending loop to 0 doesn't
                // wrap; `{v}` is exposed as uint64_t to match index/value use.
                let k = self.tmp.get();
                self.tmp.set(k + 1);
                b.push_str(&format!(
                    "{ind}{{ int64_t _lo{k} = (int64_t)({lo}), _hi{k} = (int64_t)({hi});\n\
                     {ind}signed _st{k} = _lo{k} <= _hi{k} ? 1 : -1;\n\
                     {ind}for (int64_t _c{k} = _lo{k}; ; _c{k} += _st{k}) {{\n\
                     {ind}uint64_t {v} = (uint64_t)_c{k};\n"
                ));
                let fresh = self.locals.borrow_mut().insert(v.clone());
                let previous_type = self
                    .local_types
                    .borrow_mut()
                    .insert(v.clone(), "integer".to_string());
                let previous_family = self.local_families.borrow_mut().remove(v);
                let previous_width = self.local_widths.borrow_mut().insert(v.clone(), 64);
                for s in &body.stmts {
                    self.stmt(s, b, depth + 1)?;
                }
                self.local_types.borrow_mut().remove(v);
                self.local_widths.borrow_mut().remove(v);
                if let Some(previous) = previous_type {
                    self.local_types.borrow_mut().insert(v.clone(), previous);
                }
                if let Some(previous) = previous_family {
                    self.local_families.borrow_mut().insert(v.clone(), previous);
                }
                if let Some(previous) = previous_width {
                    self.local_widths.borrow_mut().insert(v.clone(), previous);
                }
                if fresh {
                    self.locals.borrow_mut().remove(v);
                }
                b.push_str(&format!("{ind}if (_c{k} == _hi{k}) break;\n{ind}}} }}\n"));
            }
            ast::Stmt::If(iff) => self.c_if(iff, b, depth)?,
            // A testbench-level match: first arm whose pattern hits, as a C
            // if/else-if chain over the evaluated scrutinee.
            ast::Stmt::Match(m) => {
                let scrut = self.expr(&m.scrutinee)?;
                let k = self.tmp.get();
                self.tmp.set(k + 1);
                b.push_str(&format!("{ind}{{ uint64_t _m{k} = {scrut};\n"));
                let mut first = true;
                for arm in &m.arms {
                    let cond = self.pattern_cond(&arm.pattern, &format!("_m{k}"))?;
                    let kw = if first { "if" } else { "else if" };
                    match cond {
                        Some(c) => b.push_str(&format!("{ind}{kw} ({c}) {{\n")),
                        None => b.push_str(&format!(
                            "{ind}{} {{\n",
                            if first { "if (1)" } else { "else" }
                        )),
                    }
                    for s in &arm.body.stmts {
                        self.stmt(s, b, depth + 1)?;
                    }
                    b.push_str(&format!("{ind}}}\n"));
                    first = false;
                }
                b.push_str(&format!("{ind}}}\n"));
            }
            _ => {}
        }
        Ok(())
    }

    /// `if`/`else if`/`else` chains, recursing through the else branch.
    fn c_if(&self, iff: &ast::IfStmt, b: &mut String, depth: usize) -> Result<(), String> {
        let ind = "    ".repeat(depth);
        let c = self.expr(&iff.cond)?;
        // Binary/comparison lowering already encloses its expression. Avoid
        // producing `if ((a == b))`, which Clang diagnoses as suspicious,
        // while still parenthesizing a bare name or literal as C requires.
        let condition = if c.starts_with('(') && c.ends_with(')') {
            c
        } else {
            format!("({c})")
        };
        b.push_str(&format!("{ind}if {condition} {{\n"));
        for s in &iff.then.stmts {
            self.stmt(s, b, depth + 1)?;
        }
        b.push_str(&format!("{ind}}}\n"));
        match iff.else_.as_deref() {
            Some(ast::ElseBranch::Block(block)) => {
                b.push_str(&format!("{ind}else {{\n"));
                for s in &block.stmts {
                    self.stmt(s, b, depth + 1)?;
                }
                b.push_str(&format!("{ind}}}\n"));
            }
            Some(ast::ElseBranch::If(inner)) => {
                b.push_str(&format!("{ind}else {{\n"));
                self.c_if(inner, b, depth + 1)?;
                b.push_str(&format!("{ind}}}\n"));
            }
            None => {}
        }
        Ok(())
    }

    /// Expand a format string into a printf format plus argument expressions,
    /// honouring `{{`/`}}` escapes and rendering enum/real/integer operands the
    /// way `print!` does. Shared by `print!` and the `assert!`/`warn!` messages.
    /// A stand-in for `e` when deciding how to *render* it. A branch-valued
    /// expression has no name or type of its own, but its branches do, and the
    /// type checker has already made them agree — so the first branch answers
    /// "is this a Char / an enum / a signed vector" for the whole expression.
    /// The value still comes from `e` itself.
    fn type_witness(e: &ast::Expr) -> &ast::Expr {
        match e {
            ast::Expr::IfExpr { then, .. } => Self::type_witness(then),
            ast::Expr::Match { arms, .. } => match arms.iter().find_map(|a| a.value_expr()) {
                Some(v) => Self::type_witness(v),
                None => e,
            },
            // An operator that keeps its operands' family renders as they do,
            // so `a and b` on `Logic` prints a symbol rather than the raw
            // discriminant. A literal operand carries no name to look a type
            // up by, so prefer the side that does: `x and '1'` reads as `x`.
            ast::Expr::Binary { op, lhs, rhs, .. } if op.keeps_operand_family() => {
                let l = Self::type_witness(lhs);
                if expr_path(l).is_some() {
                    l
                } else {
                    Self::type_witness(rhs)
                }
            }
            ast::Expr::Unary {
                op: ast::UnOp::Not,
                rhs,
                ..
            } => Self::type_witness(rhs),
            _ => e,
        }
    }

    fn c_format(&self, text: &str, args: &[ast::Expr]) -> Result<(String, Vec<String>), String> {
        let mut cfmt = String::new();
        let mut cargs = Vec::new();
        let mut vals = args.iter();
        for part in siox::syntax::format::parts(text) {
            let a = match part {
                siox::syntax::format::FormatPart::Text(t) => {
                    cfmt.push_str(&c_escape(&t).replace('%', "%%"));
                    continue;
                }
                siox::syntax::format::FormatPart::Placeholder => vals.next(),
            };
            let Some(a) = a else { continue };
            // Type questions go through a witness so a branch-valued argument
            // is rendered as whatever its branches are.
            let w = Self::type_witness(a);
            let sig = expr_path(w)
                .and_then(|p| self.map.get(&p))
                .map(|id| &self.design.signals[id.0 as usize]);
            let local_type = expr_path(w)
                .and_then(|path| self.local_types.borrow().get(&path).cloned())
                .or_else(|| {
                    // A dynamic index (`s[i]`) has no path, but every element
                    // of an array shares a type, so element 0 answers for the
                    // whole of it. Without this `s[i]` on a string printed the
                    // code point where `s[0]` printed the character.
                    let ast::Expr::Index { base, .. } = w else {
                        return None;
                    };
                    let base = expr_path(base)?;
                    self.local_types
                        .borrow()
                        .get(&format!("{base}[0]"))
                        .cloned()
                });
            let string_elems = self.formatted_string_elems(a);
            let is_real = self.is_real_operand(a);
            let is_char = sig.is_some_and(|signal| signal.char)
                || local_type.as_deref() == Some("Char")
                || self.call_return_head(w).as_deref() == Some("Char");
            let ety: Option<String> = sig
                .and_then(|s| s.enum_type.clone())
                .or(local_type)
                .or_else(|| {
                    // A call returning an enum renders its symbol, like a
                    // signal or local of that type does.
                    let head = self.call_return_head(w)?;
                    self.design.enum_syms.contains_key(&head).then_some(head)
                })
                .or_else(|| {
                    // The variant itself names its enum: `print!("{}",
                    // State::Done)`. Everything reached through a name
                    // rendered as `Done` while the literal variant — the one
                    // form that says its type outright — printed `2`.
                    let ast::Expr::Path(p) = w else { return None };
                    let head = p.segments.first()?.text.clone();
                    (p.segments.len() >= 2 && self.design.enum_syms.contains_key(&head))
                        .then_some(head)
                });
            let enum_syms = ety.as_ref().and_then(|e| self.design.enum_syms.get(e));
            if let ast::Expr::StrLit { text, .. } = a {
                cfmt.push_str("%s");
                cargs.push(format!("\"{}\"", c_escape(text)));
            } else if let Some(elems) = string_elems {
                cfmt.push_str("%s");
                if elems.is_empty() {
                    cargs.push("\"\"".into());
                } else {
                    let capacity = elems.len().saturating_mul(4).saturating_add(1);
                    cargs.push(format!(
                        "sx_chars((sx_value[]){{{}}}, {}, (char[{capacity}]){{0}})",
                        elems.join(", "),
                        elems.len()
                    ));
                }
            } else if let Some(syms) = enum_syms {
                let mut tern = String::from("\"?\"");
                for (disc, sym) in syms {
                    let esc = c_escape(sym);
                    tern = format!("(_v=={disc}?\"{esc}\":{tern})");
                }
                cfmt.push_str("%s");
                cargs.push(format!(
                    "({{ long long _v = (long long)({}); {tern}; }})",
                    self.expr(a)?
                ));
            } else if is_real {
                cfmt.push_str("%g");
                cargs.push(format!("sx_f64({})", self.value_for_print(a)?));
            } else if is_char {
                cfmt.push_str("%s");
                cargs.push(format!("sx_utf8(({}), (char[5]){{0}})", self.expr(a)?));
            } else if self.is_integer_operand(a) {
                cfmt.push_str("%lld");
                let rendered = self.expr(a)?;
                cargs.push(format!(
                    "(long long)({})",
                    self.c_integer_operand(a, &rendered)
                ));
            } else if let Some(width) = self.signed_vector_width(a) {
                cfmt.push_str("%lld");
                let rendered = self.expr(a)?;
                cargs.push(format!("(long long)sx_i64(({rendered}), {width})"));
            } else {
                cfmt.push_str("%s");
                cargs.push(format!(
                    "sx_decimal(({}), (char[SX_DECIMAL_CAP]){{0}})",
                    self.expr(a)?
                ));
            }
        }
        Ok((cfmt, cargs))
    }

    /// The C expression that sets `g_msg` for an `assert!`/`warn!` message at
    /// `args[at]`. With no format arguments this is a plain string literal;
    /// with them it formats into a static buffer first.
    fn c_message(&self, args: &[ast::Expr], at: usize, fallback: &str) -> Result<String, String> {
        let Some(text) = args.get(at).and_then(str_lit) else {
            return Ok(format!("g_msg = \"{}\";", c_escape(fallback)));
        };
        let rest = args.get(at + 1..).unwrap_or(&[]);
        if rest.is_empty() {
            return Ok(format!("g_msg = \"{}\";", c_escape(&text)));
        }
        let (cfmt, cargs) = self.c_format(&text, rest)?;
        let list = if cargs.is_empty() {
            String::new()
        } else {
            format!(", {}", cargs.join(", "))
        };
        let decimal_capacity = u64::from(self.value_bits)
            .saturating_mul(30_103)
            .div_ceil(100_000)
            .saturating_add(2);
        let enum_capacity = self
            .design
            .enum_syms
            .values()
            .flat_map(|symbols| symbols.values().map(|symbol| symbol.len() as u64 + 1))
            .max()
            .unwrap_or(0);
        let string_capacity = rest
            .iter()
            .filter_map(|arg| match arg {
                ast::Expr::StrLit { text, .. } => Some(text.len() as u64 + 1),
                _ => self
                    .formatted_string_elems(arg)
                    .map(|elems| (elems.len() as u64).saturating_mul(4).saturating_add(1)),
            })
            .max()
            .unwrap_or(0);
        let argument_capacity = decimal_capacity
            .max(enum_capacity)
            .max(string_capacity)
            .max(32);
        let capacity = u64::try_from(text.len())
            .unwrap_or(u64::MAX)
            .saturating_add(
                u64::try_from(rest.len())
                    .unwrap_or(u64::MAX)
                    .saturating_mul(argument_capacity),
            )
            .saturating_add(1);
        let id = self.message_id.get();
        self.message_id.set(id.saturating_add(1));
        Ok(format!(
            "static char _sx_msg_{id}[{capacity}]; \
             snprintf(_sx_msg_{id}, sizeof _sx_msg_{id}, \"{cfmt}\"{list}); \
             g_msg = _sx_msg_{id};"
        ))
    }

    fn call(
        &self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        bang: bool,
        b: &mut String,
        depth: usize,
    ) -> Result<(), String> {
        let ind = "    ".repeat(depth);
        let name = match callee {
            ast::Expr::Path(p) => p.segments.first().map(|s| s.text.as_str()).unwrap_or(""),
            _ => "",
        };
        match name {
            // `tick`/`wait` were removed: `await` is the one timing primitive
            // (`wait` errors at parse; tick() returns to std later as source).
            "tick" => {
                return Err(
                    "`tick()` was removed (it returns as a std function later); \
                            write `clk = '1'; await 5ns; clk = '0';` or start a clock \
                            generator (`clk = not clk after 5ns;`)"
                        .into(),
                );
            }
            // clock(clk, period): register a background clock on the wheel
            // (init low; first toggle one half period from now).
            // clock() was sugar; the canonical generator is the after-form.
            "clock" => {
                return Err(
                    "`clock()` was removed; write `clk = not clk after <half-period>;`".into(),
                );
            }
            // await <duration> | <edge> | <condition>.
            "await" => self.emit_await(args, b, depth)?,
            // print!: expand the format at compile time into a printf.
            "print" if bang => {
                let Some(ast::Expr::StrLit { text, .. }) = args.first() else {
                    return Err("print! needs a format string".into());
                };
                let (cfmt, cargs) = self.c_format(text, &args[1..])?;
                let call_args = if cargs.is_empty() {
                    String::new()
                } else {
                    format!(", {}", cargs.join(", "))
                };
                b.push_str(&format!("{ind}printf(\"{cfmt}\\n\"{call_args});\n"));
            }
            // seed!(n): reseed the deterministic RNG.
            "seed" => {
                let n = self.expr(args.first().ok_or("seed! needs a value")?)?;
                b.push_str(&format!("{ind}g_rand = ({n}) ? ({n}) : 1;\n"));
            }
            // stop!/finish!: end the test cleanly (passing).
            "stop" | "finish" => {
                b.push_str(&format!(
                    "{ind}printf(\"{name} at %llu fs\\n\", (unsigned long long)_now); return 0;\n"
                ));
            }
            "assert" if bang => {
                let cond = args.first().ok_or("assert needs a condition")?;
                let c = self.expr(cond)?;
                let set = self.c_message(args, 1, "assertion failed")?;
                // Record the failure message and fail this test; `main` prints
                // the `test <name> ... FAILED` line and the message.
                b.push_str(&format!("{ind}if (!({c})) {{ {set} return 1; }}\n"));
            }
            // warn!(cond, msg): non-fatal — report to stderr, keep running.
            "warn" if bang => {
                let cond = args.first().ok_or("warn needs a condition")?;
                let c = self.expr(cond)?;
                let set = self.c_message(args, 1, "warning")?;
                b.push_str(&format!(
                    "{ind}if (!({c})) {{ {set} fprintf(stderr, \"warning: %s\\n\", g_msg); g_warnings++; }}\n"
                ));
            }
            _ => {}
        }
        Ok(())
    }

    /// Element count of a DUT-connected array in the signal map.
    fn array_len(&self, path: &str) -> Option<u64> {
        if let Some(indices) = self.local_indices.borrow().get(path) {
            return u64::try_from(indices.len())
                .ok()
                .filter(|length| *length > 0);
        }
        let mut n = 0;
        while self.map.contains_key(&format!("{path}[{n}]")) {
            n += 1;
        }
        (n > 0).then_some(n)
    }

    /// Whether an operand reads a `Char` signal (so a `'x'` literal counterpart
    /// is a code point, not a logic code).
    fn is_char_operand(&self, e: &ast::Expr) -> bool {
        // A branch-valued operand is a character if its branches are.
        let e = Self::type_witness(e);
        // A call has no path to look up, so its declared return type is the
        // only thing that says it yields a character.
        if self.call_return_head(e).as_deref() == Some("Char") {
            return true;
        }
        let Some(path) = expr_path(e) else {
            return false;
        };
        self.map
            .get(&path)
            .map(|&id| self.design.signals[id.0 as usize].char)
            .unwrap_or(false)
            || self.local_types.borrow().get(&path).map(String::as_str) == Some("Char")
    }

    /// An operand in a `Char` comparison: a `'x'` literal is its Unicode code
    /// point; anything else translates normally.
    fn c_char_operand(&self, e: &ast::Expr) -> Result<String, String> {
        match e {
            ast::Expr::CharLit { ch, .. } => Ok(format!("{}ULL", *ch as u32)),
            _ => self.expr(e),
        }
    }

    /// Whether an operand is a bare character/logic literal (`'g'`).
    fn is_char_lit(&self, e: &ast::Expr) -> bool {
        matches!(e, ast::Expr::CharLit { .. })
    }

    /// A char literal's position in enum `en` (VHDL `T'pos`), data-driven from
    /// the enum's declaration — `None` if `e` is not a char literal.
    fn enum_char_lit(&self, en: &str, e: &ast::Expr) -> Option<u64> {
        if let ast::Expr::CharLit { ch, .. } = e {
            return self
                .enums
                .get(en)
                .and_then(|m| m.get(&format!("'{ch}'")))
                .copied();
        }
        None
    }

    fn char_value_for_target(&self, name: &str, ch: char) -> u64 {
        if let Some(id) = self.map.get(name) {
            let signal = &self.design.signals[id.0 as usize];
            if signal.char {
                return ch as u32 as u64;
            }
            if let Some(enum_name) = &signal.enum_type {
                if let Some(value) = self
                    .enums
                    .get(enum_name)
                    .and_then(|symbols| symbols.get(&format!("'{ch}'")))
                {
                    return *value;
                }
            }
        }
        if let Some(ty) = self.local_types.borrow().get(name) {
            if ty == "Char" {
                return ch as u32 as u64;
            }
            if let Some(value) = self
                .enums
                .get(ty)
                .and_then(|symbols| symbols.get(&format!("'{ch}'")))
            {
                return *value;
            }
        }
        ch as u32 as u64
    }

    /// The enum type of an operand that is an enum-typed signal or testbench
    /// local — so a char-literal counterpart resolves by position in that enum.
    fn enum_operand_type(&self, e: &ast::Expr) -> Option<String> {
        let p = expr_path(e)?;
        if let Some(&id) = self.map.get(&p) {
            if let Some(en) = &self.design.signals[id.0 as usize].enum_type {
                return Some(en.clone());
            }
        }
        self.local_types
            .borrow()
            .get(&p)
            .filter(|ty| self.enums.contains_key(*ty))
            .cloned()
    }

    /// An operand in an enum comparison: a char literal takes its position in
    /// `en`; anything else translates normally.
    fn enum_operand_c(&self, en: &str, e: &ast::Expr) -> Result<String, String> {
        match self.enum_char_lit(en, e) {
            Some(d) => Ok(format!("{d}ULL")),
            None => self.expr(e),
        }
    }

    /// Whether an operand reads a `real` signal.
    /// The family and width a conversion names: `signed[8](x)` -> ("signed", 8).
    /// A conversion is a `Call` whose callee is the indexed family, so it has
    /// no declared return type to consult like an ordinary call does.
    fn conversion_target(&self, e: &ast::Expr) -> Option<(String, u32)> {
        let ast::Expr::Call { callee, .. } = e else {
            return None;
        };
        let ast::Expr::Index { base, index, .. } = callee.as_ref() else {
            return None;
        };
        let head = expr_path(base)?;
        let w = siox::ir::eval_const_fns(index, &HashMap::new(), self.fns, 0)? as u32;
        (w > 0 && w <= 64).then_some((head, w))
    }

    /// The declared return type of a call, for recovering everything the call
    /// site would otherwise throw away: a vector's width and family, an enum's
    /// symbols. See `call_return_head` for the name alone.
    fn call_return_type(&self, e: &ast::Expr) -> Option<ast::Type> {
        let ast::Expr::Call { callee, .. } = e else {
            return None;
        };
        if let ast::Expr::Field { base, field, .. } = callee.as_ref() {
            let receiver = self.receiver_type(base)?;
            return self
                .methods
                .get(&(receiver, field.text.clone()))
                .and_then(|f| f.ret.clone());
        }
        let key = siox::ir::call_fn_key(callee)?;
        self.fns.get(&key).and_then(|f| f.ret.clone())
    }

    /// The head name of a call's declared return type. `is_real_operand`
    /// already consulted this for `real`; nothing else did, so a function
    /// returning `Char` produced a value that displayed and compared as a
    /// plain number at the call site while the same value bound to a typed
    /// local behaved correctly.
    fn call_return_head(&self, e: &ast::Expr) -> Option<String> {
        let ast::Expr::Call { callee, .. } = e else {
            return None;
        };
        if let ast::Expr::Field { base, field, .. } = callee.as_ref() {
            let receiver = self.receiver_type(base)?;
            return self
                .methods
                .get(&(receiver, field.text.clone()))
                .and_then(|f| f.ret.as_ref())
                .and_then(type_head_name)
                .map(str::to_string);
        }
        let key = siox::ir::call_fn_key(callee)?;
        self.fns
            .get(&key)
            .and_then(|f| f.ret.as_ref())
            .and_then(type_head_name)
            .map(str::to_string)
    }

    fn is_real_operand(&self, e: &ast::Expr) -> bool {
        match e {
            ast::Expr::Int { text, .. } if text.contains('.') => return true,
            ast::Expr::SuffixLit { suffix, .. }
                if suffix.text == "Hz" || suffix.text.ends_with("Hz") =>
            {
                return true;
            }
            ast::Expr::Unary {
                op: ast::UnOp::Neg,
                rhs,
                ..
            } => return self.is_real_operand(rhs),
            ast::Expr::Binary { op, lhs, rhs, .. }
                if !matches!(
                    op,
                    ast::BinOp::Eq
                        | ast::BinOp::Ne
                        | ast::BinOp::Lt
                        | ast::BinOp::Le
                        | ast::BinOp::Gt
                        | ast::BinOp::Ge
                ) =>
            {
                return self.is_real_operand(lhs) || self.is_real_operand(rhs);
            }
            ast::Expr::IfExpr { then, els, .. } => {
                return self.is_real_operand(then) || self.is_real_operand(els);
            }
            ast::Expr::Match { arms, .. } => {
                return arms
                    .iter()
                    .filter_map(ast::MatchArm::value_expr)
                    .any(|value| self.is_real_operand(value));
            }
            ast::Expr::Call { callee, .. } => {
                if let ast::Expr::Field { base, field, .. } = callee.as_ref() {
                    if let Some(receiver) = self.receiver_type(base) {
                        return self
                            .methods
                            .get(&(receiver, field.text.clone()))
                            .and_then(|function| function.ret.as_ref())
                            .and_then(type_head_name)
                            == Some("real");
                    }
                }
                if matches!(callee.as_ref(), ast::Expr::Path(path)
                    if path.segments.len() == 1 && path.segments[0].text == "uniform")
                {
                    return true;
                }
                if let Some(key) = siox::ir::call_fn_key(callee) {
                    return self
                        .fns
                        .get(&key)
                        .and_then(|function| function.ret.as_ref())
                        .and_then(type_head_name)
                        == Some("real");
                }
            }
            _ => {}
        }
        let Some(path) = expr_path(e) else {
            return false;
        };
        if self.real_consts.contains(&path)
            || self
                .fn_type_env
                .borrow()
                .last()
                .and_then(|types| types.get(&path))
                .map(String::as_str)
                == Some("real")
        {
            return true;
        }
        self.map
            .get(&path)
            .map(|&id| self.design.signals[id.0 as usize].real)
            .unwrap_or(false)
            || self.local_types.borrow().get(&path).map(String::as_str) == Some("real")
    }

    /// Whether an operand has the kernel signed `integer` type. Integer
    /// literals alone stay contextual (so `unsigned_value < 5` still dispatches
    /// as unsigned); an explicitly typed path/call/attribute selects signed
    /// comparisons, division, and right shift.
    /// The width of a `signed`-family value whose declared type this stage can
    /// see, for display. The compiler tracks no signedness — `signed` is just
    /// the family whose std impls give it a signed `Ord`, an arithmetic `Shr`
    /// and a signed `Div` — so printing has to recover it from the declared
    /// type, the same way this stage already recovers `Char` and enum symbols.
    ///
    /// Without it a `signed[8]` holding -5 prints as 251 while `s < 0` is
    /// simultaneously true, which reads as a contradiction.
    fn signed_vector_width(&self, e: &ast::Expr) -> Option<u32> {
        // A branch-valued expression is as signed as its branches; the type
        // checker has already made them agree. `is_real_operand` looks through
        // these shapes for `real`, and nothing did for `signed`, so
        // `if c { a } else { b }` on signed values read back as a bit pattern.
        match e {
            ast::Expr::IfExpr { then, els, .. } => {
                return self
                    .signed_vector_width(then)
                    .or_else(|| self.signed_vector_width(els));
            }
            ast::Expr::Match { arms, .. } => {
                return arms
                    .iter()
                    .filter_map(|a| a.value_expr())
                    .find_map(|v| self.signed_vector_width(v));
            }
            // Arithmetic keeps its operands' family and width, so an unbound
            // `a - b` renders as the signed value it is. Binding it first
            // (`let d: signed[8] = a - b;`) printed correctly while the inline
            // form showed the raw pattern — -9 came out as 247.
            ast::Expr::Binary { op, lhs, rhs, .. } if op.keeps_operand_family() => {
                return self
                    .signed_vector_width(lhs)
                    .or_else(|| self.signed_vector_width(rhs));
            }
            _ => {}
        }
        // A conversion names its target directly: `signed[8](x)` is a Call
        // whose callee is the indexed family. Hardware read this as signed and
        // the testbench did not, so the two disagreed on `signed[8](200)`.
        if let Some((fam, w)) = self.conversion_target(e) {
            if self.type_name_is(&fam, "signed") {
                return Some(w);
            }
        }
        // A call carries no name; its declared return type is what says the
        // result is signed and how wide it is.
        if let Some(ret) = self.call_return_type(e) {
            if type_head_name(&ret).is_some_and(|h| self.type_name_is(h, "signed")) {
                if let Some(w) = self.declared_width(&ret) {
                    if w > 0 && w <= 64 {
                        return Some(w);
                    }
                }
            }
        }
        let path = expr_path(e)?;
        // `local_families` is the declared vector family; it is recorded for
        // connected locals and struct leaves too, which `local_types` is not.
        let family = self
            .local_families
            .borrow()
            .get(&path)
            .cloned()
            .or_else(|| self.local_types.borrow().get(&path).cloned())?;
        if !self.type_name_is(&family, "signed") {
            return None;
        }
        // A connected name records no local width — the signal it is wired to
        // carries it — so fall back to that rather than losing the family.
        let width = self.local_widths.borrow().get(&path).copied().or_else(|| {
            self.map
                .get(&path)
                .map(|id| self.design.signals[id.0 as usize].width)
        })?;
        (width > 0 && width <= 64).then_some(width)
    }

    fn is_integer_operand(&self, e: &ast::Expr) -> bool {
        match e {
            ast::Expr::Unary {
                op: ast::UnOp::Neg,
                rhs,
                ..
            } => {
                return matches!(rhs.as_ref(), ast::Expr::Int { text, .. } if !text.contains('.'))
                    || self.is_integer_operand(rhs);
            }
            ast::Expr::Unary { rhs, .. } => return self.is_integer_operand(rhs),
            ast::Expr::Binary { lhs, rhs, .. } => {
                return self.is_integer_operand(lhs) || self.is_integer_operand(rhs);
            }
            ast::Expr::IfExpr { then, els, .. } => {
                return self.is_integer_operand(then) || self.is_integer_operand(els);
            }
            ast::Expr::Match { arms, .. } => {
                return arms
                    .iter()
                    .filter_map(ast::MatchArm::value_expr)
                    .any(|value| self.is_integer_operand(value));
            }
            ast::Expr::SysAttr { attr, .. }
                if matches!(
                    attr.text.as_str(),
                    "left" | "right" | "high" | "low" | "length"
                ) =>
            {
                return true;
            }
            ast::Expr::Call { callee, .. } => {
                if let ast::Expr::Field { base, field, .. } = callee.as_ref() {
                    if let Some(receiver) = self.receiver_type(base) {
                        return self
                            .methods
                            .get(&(receiver, field.text.clone()))
                            .and_then(|function| function.ret.as_ref())
                            .is_some_and(|ty| self.type_is(ty, "integer"));
                    }
                }
                if let Some(key) = siox::ir::call_fn_key(callee) {
                    return self
                        .fns
                        .get(&key)
                        .and_then(|function| function.ret.as_ref())
                        .is_some_and(|ty| self.type_is(ty, "integer"));
                }
            }
            _ => {}
        }
        let Some(path) = expr_path(e) else {
            return false;
        };
        self.fn_type_env
            .borrow()
            .last()
            .and_then(|types| types.get(&path))
            .map(String::as_str)
            == Some("integer")
            || self.integer_consts.contains(&path)
            || self
                .local_types
                .borrow()
                .get(&path)
                .is_some_and(|name| self.type_name_is(name, "integer"))
            || self
                .map
                .get(&path)
                .is_some_and(|id| self.design.signals[id.0 as usize].integer)
    }

    fn c_integer_operand(&self, e: &ast::Expr, rendered: &str) -> String {
        let signed_width = expr_path(e).and_then(|path| {
            if let Some(id) = self
                .map
                .get(&path)
                .filter(|id| self.design.signals[id.0 as usize].integer)
            {
                let signal = &self.design.signals[id.0 as usize];
                return signal
                    .range
                    .map(|(lo, _)| lo < 0)
                    .unwrap_or(true)
                    .then_some(signal.width);
            }
            let integer_local = self
                .local_types
                .borrow()
                .get(&path)
                .is_some_and(|name| self.type_name_is(name, "integer"));
            if !integer_local {
                return None;
            }
            let negative_capable = self
                .local_ranges
                .borrow()
                .get(&path)
                .map(|(lo, _)| *lo < 0)
                .unwrap_or(true);
            negative_capable
                .then(|| self.local_widths.borrow().get(&path).copied())
                .flatten()
        });
        match signed_width {
            Some(width) => format!("sx_i64(({rendered}), {width})"),
            None => format!("((int64_t)({rendered}))"),
        }
    }

    /// An operand in a `real` comparison, as a C `double`: a real signal reads
    /// its bits, an integer/decimal literal is a float constant, anything else
    /// is cast.
    fn c_real_operand(&self, e: &ast::Expr) -> Result<String, String> {
        match e {
            ast::Expr::Int { text, .. } => Ok(format!("((double){})", text.replace('_', ""))),
            ast::Expr::SuffixLit { text, suffix, .. }
                if suffix.text == "Hz" || suffix.text.ends_with("Hz") =>
            {
                let scale = ast::suffix_scale(&suffix.text).unwrap_or(1);
                Ok(format!("((double)({}) * {scale}.0)", text.replace('_', "")))
            }
            _ if self.is_real_operand(e) => {
                if let Some(path) = expr_path(e) {
                    if self
                        .fn_env
                        .borrow()
                        .last()
                        .is_some_and(|environment| environment.contains_key(&path))
                    {
                        return Ok(format!("sx_f64({})", self.expr(e)?));
                    }
                    if self.locals.borrow().contains(&path) {
                        return Ok(format!("sx_f64({})", c_local_ident(&path)));
                    }
                    if let Some(id) = self.map.get(&path) {
                        return Ok(format!("sx_f64(sx_read({}))", id.0));
                    }
                }
                Ok(format!("sx_f64({})", self.expr(e)?))
            }
            _ => Ok(format!("((double)({}))", self.expr(e)?)),
        }
    }

    /// The per-element C read-expressions of a `Char` array (a string) operand:
    /// a connected string reads each element signal, a local reads its C
    /// element locals. `None` if `e` isn't an array-shaped name.
    fn c_string_elems(&self, e: &ast::Expr) -> Option<Vec<String>> {
        let path = expr_path(e)?;
        if let Some(indices) = self.local_indices.borrow().get(&path).cloned() {
            let values = indices
                .into_iter()
                .filter_map(|index| {
                    let element = format!("{path}[{index}]");
                    if let Some(id) = self.map.get(&element) {
                        Some(format!("sx_read({})", id.0))
                    } else if self.locals.borrow().contains(&element) {
                        Some(c_local_ident(&element))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            return Some(values);
        }
        // A connected string: element signals `path[i]` in the map.
        if let Some(n) = self.array_len(&path) {
            return Some(
                (0..n)
                    .map(|i| format!("sx_read({})", self.map[&format!("{path}[{i}]")].0))
                    .collect(),
            );
        }
        // A testbench-local string: one C local per element (`sxl_path_i`).
        let mut elems = Vec::new();
        while self
            .locals
            .borrow()
            .contains(&format!("{path}[{}]", elems.len()))
        {
            elems.push(c_local_ident(&format!("{path}[{}]", elems.len())));
        }
        if !elems.is_empty()
            || self.local_types.borrow().get(&path).map(String::as_str) == Some("string")
        {
            Some(elems)
        } else {
            None
        }
    }

    /// A formatted string argument as its per-character reads. Empty
    /// testbench-local strings deliberately produce an empty vector; they
    /// otherwise have no element local through which `c_string_elems` could
    /// recognize them.
    fn formatted_string_elems(&self, e: &ast::Expr) -> Option<Vec<String>> {
        let path = expr_path(e)?;
        let ty = self.local_types.borrow().get(&path).cloned();
        let elems = self.c_string_elems(e);
        match ty.as_deref() {
            Some("string") => Some(elems.unwrap_or_default()),
            Some("Char") if elems.is_some() => elems,
            _ => None,
        }
    }

    /// Whole-string `==` / `!=` as a C boolean, when one operand is a string
    /// literal (a string is a `Char` array). `None` if neither side is a string
    /// literal (fall through to scalar handling).
    fn c_string_cmp(
        &self,
        op: &ast::BinOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
    ) -> Result<Option<String>, String> {
        let lit = |e: &ast::Expr| match e {
            ast::Expr::StrLit { text, .. } => Some(text.chars().collect::<Vec<char>>()),
            _ => None,
        };
        let eq = matches!(op, ast::BinOp::Eq);
        let left_literal = lit(lhs);
        let right_literal = lit(rhs);
        if let (Some(left), Some(right)) = (&left_literal, &right_literal) {
            let equal = left == right;
            return Ok(Some(if equal == eq { "1".into() } else { "0".into() }));
        }
        if left_literal.is_none() && right_literal.is_none() {
            let (Some(left), Some(right)) = (self.c_string_elems(lhs), self.c_string_elems(rhs))
            else {
                return Ok(None);
            };
            if left.len() != right.len() {
                return Ok(Some(if eq { "0".into() } else { "1".into() }));
            }
            let terms = left
                .iter()
                .zip(&right)
                .map(|(a, b)| format!("({a} == {b})"))
                .collect::<Vec<_>>();
            let all = if terms.is_empty() {
                "1".into()
            } else {
                terms.join(" && ")
            };
            return Ok(Some(if eq {
                format!("({all})")
            } else {
                format!("(!({all}))")
            }));
        }
        let (elems, chars) = match (left_literal, right_literal) {
            (Some(_), Some(_)) => unreachable!(),
            (None, Some(c)) => (self.c_string_elems(lhs), c),
            (Some(c), None) => (self.c_string_elems(rhs), c),
            (None, None) => unreachable!(),
        };
        let Some(elems) = elems else { return Ok(None) };
        // A length mismatch is unequal — a constant either way.
        if elems.len() != chars.len() {
            return Ok(Some(if eq { "0".into() } else { "1".into() }));
        }
        let terms: Vec<String> = elems
            .iter()
            .zip(&chars)
            .map(|(e, &c)| format!("({e} == {}ULL)", c as u32))
            .collect();
        let all = if terms.is_empty() {
            "1".to_string()
        } else {
            terms.join(" && ")
        };
        Ok(Some(if eq {
            format!("({all})")
        } else {
            format!("(!({all}))")
        }))
    }

    /// `await <duration> | <edge> | <condition>` in the native harness, on the
    /// generated event wheel: a duration runs the clocks up to `now + dur`; an
    /// edge or condition steps clock edges until it fires (bounded, mirroring
    /// the runner's scheduler).
    /// Emit the edge-wait loop for `await <clk>::<kind>` / `await clk.kind()`.
    fn emit_await_edge(
        &self,
        base: &ast::Expr,
        kind: &str,
        ind: &str,
        b: &mut String,
    ) -> Result<(), String> {
        let id = expr_path(base)
            .and_then(|p| self.map.get(&p))
            .ok_or("await: unknown edge signal")?
            .0;
        let hit = match kind {
            "rising" => "!_p && _c",
            "falling" => "_p && !_c",
            _ => "_p != _c",
        };
        b.push_str(&format!(
            "{ind}{{ uint64_t _p = sx_read({id}); \
             for (signed _g = 0; _g < 1000000; _g++) {{ \
             if (!sx_step_clock(&_now, _next, _cid, _half, _nclk)) break; \
             uint64_t _c = sx_read({id}); if ({hit}) break; _p = _c; }} }}\n"
        ));
        Ok(())
    }

    fn emit_await(&self, args: &[ast::Expr], b: &mut String, depth: usize) -> Result<(), String> {
        let ind = "    ".repeat(depth);
        match args.first() {
            Some(ast::Expr::SuffixLit { .. }) | Some(ast::Expr::Field { .. }) => {
                let dur = duration_fs(args)?;
                b.push_str(&format!(
                    "{ind}{{ uint64_t _tgt = UINT64_MAX - _now < {dur}ULL \
                     ? UINT64_MAX : _now + {dur}ULL; \
                     while (sx_next_edge(_next, _nclk) != UINT64_MAX && \
                     sx_next_edge(_next, _nclk) <= _tgt) \
                     sx_step_clock(&_now, _next, _cid, _half, _nclk); \
                     _now = _tgt; sx_settle(); }}\n"
                ));
            }
            Some(ast::Expr::SysAttr { base, attr, .. }) => {
                self.emit_await_edge(base, &attr.text, &ind, b)?;
            }
            // `await clk.rising()` — a `ClockLike` edge method waits on the same
            // edge machinery as the `::rising` sysattr.
            Some(ast::Expr::Call { callee, .. })
                if matches!(callee.as_ref(), ast::Expr::Field { field, .. }
                    if matches!(field.text.as_str(), "rising" | "falling" | "edge")) =>
            {
                if let ast::Expr::Field { base, field, .. } = callee.as_ref() {
                    self.emit_await_edge(base, &field.text, &ind, b)?;
                }
            }
            Some(cond) => {
                let c = self.expr(cond)?;
                b.push_str(&format!(
                    "{ind}for (signed _g = 0; _g < 1000000 && !({c}); _g++) {{ \
                     if (!sx_step_clock(&_now, _next, _cid, _half, _nclk)) break; }}\n"
                ));
            }
            None => {}
        }
        Ok(())
    }

    /// The C value for writing `e` to signal `id`: a real-typed target takes
    /// a float literal's f64 bit pattern (matching the runner's eval_for).
    fn value_for(&self, id: SignalId, e: &ast::Expr) -> Result<String, String> {
        let sig = &self.design.signals[id.0 as usize];
        if sig.real {
            if let ast::Expr::Int { text, .. } = e {
                if let Ok(f) = text.replace('_', "").parse::<f64>() {
                    return Ok(format!("{}ULL", f.to_bits()));
                }
            }
        }
        // A char-typed target reads a character literal as its code point.
        if sig.char {
            if let ast::Expr::CharLit { ch, .. } = e {
                return Ok(format!("{}ULL", *ch as u32));
            }
        }
        // A char literal written to an enum signal takes its position in that
        // enum (data-driven), matching the IR's `coerce_to_target`.
        if let Some(en) = &sig.enum_type {
            if let Some(d) = self.enum_char_lit(en, e) {
                return Ok(format!("{d}ULL"));
            }
        }
        self.expr(e)
    }

    /// Translate a value using an unconnected local's declared symbol type.
    /// A character literal is a Unicode code point for `Char`, an ordinal for
    /// an enum, and only otherwise a context-free logic literal.
    fn value_for_local(&self, name: &str, e: &ast::Expr) -> Result<String, String> {
        let ty = self.local_types.borrow().get(name).cloned();
        match (ty.as_deref(), e) {
            (Some("real"), ast::Expr::Int { text, .. }) => text
                .replace('_', "")
                .parse::<f64>()
                .map(|value| format!("{}ULL", value.to_bits()))
                .map_err(|_| format!("invalid real literal `{text}`")),
            (Some("Char"), ast::Expr::CharLit { ch, .. }) => Ok(format!("{}ULL", *ch as u32)),
            (Some(en), _) => match self.enum_char_lit(en, e) {
                Some(discriminant) => Ok(format!("{discriminant}ULL")),
                None => self.expr(e),
            },
            (None, _) => self.expr(e),
        }
    }

    /// A `print!` real argument as a u64-bit-pattern C expression.
    fn value_for_print(&self, a: &ast::Expr) -> Result<String, String> {
        if let ast::Expr::Int { text, .. } = a {
            if let Ok(f) = text.replace('_', "").parse::<f64>() {
                return Ok(format!("{}ULL", f.to_bits()));
            }
        }
        self.expr(a)
    }

    /// The declared type head of a method-call receiver (`p` in `p.sum()`): a
    /// struct/enum local, or a numeric family (`n.cmp(m)`).
    fn receiver_type(&self, recv: &ast::Expr) -> Option<String> {
        let p = expr_path(recv)?;
        if let Some(t) = self.local_types.borrow().get(&p) {
            return Some(t.clone());
        }
        self.local_families.borrow().get(&p).cloned()
    }

    /// A method call `recv.method(args)` as a C expression: substitute `self`
    /// with the receiver and each parameter with its argument into the body,
    /// then flatten it (like a module fn). `self.a` becomes `<recv>.a`, which
    /// reads the receiver's struct-local field.
    fn c_method_call(
        &self,
        recv: &ast::Expr,
        method: &str,
        args: &[ast::Expr],
    ) -> Result<String, String> {
        let ty = self
            .receiver_type(recv)
            .ok_or_else(|| format!("cannot resolve the receiver type of `.{method}()`"))?;
        let f = self
            .methods
            .get(&(ty.clone(), method.to_string()))
            .ok_or_else(|| format!("unknown method `{ty}::{method}`"))?;
        let body = f.body.as_ref().ok_or("method has no body")?;
        let mut map: HashMap<String, ast::Expr> = HashMap::new();
        map.insert("self".to_string(), recv.clone());
        for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(n) = &p.name {
                map.insert(n.text.clone(), a.clone());
            }
        }
        let stmts: Vec<ast::Stmt> = body
            .stmts
            .iter()
            .map(|s| siox::ir::subst_stmt_paths(s, &map))
            .collect();
        self.c_fn_stmts(
            &stmts,
            f.ret.as_ref().is_some_and(|ty| self.type_is(ty, "real")),
        )
    }

    /// A module-fn call as a C expression: bind the arguments, then flatten
    /// the `return`/`if` body into nested conditionals.
    /// The C constant for a zero-argument construction of a named type: the
    /// `impl New` default where one is declared, an enum's first variant, and
    /// zero for a vector family. Returns `None` when the callee is not a type,
    /// so an ordinary call falls through.
    fn c_default_construction(&self, callee: &ast::Expr) -> Option<String> {
        // `unsigned[8]()` — the width is irrelevant to the value.
        let name = match callee {
            ast::Expr::Index { base, .. } => expr_path(base)?,
            _ => match callee {
                ast::Expr::Path(p) if p.segments.len() == 1 => p.segments[0].text.clone(),
                // `T::new()`, the trait spelling of the same thing.
                ast::Expr::Path(p) if p.segments.len() == 2 && p.segments[1].text == "new" => {
                    p.segments[0].text.clone()
                }
                _ => return None,
            },
        };
        if let Some(&v) = self.design.new_defaults.get(&name) {
            return Some(format!("{v}ULL"));
        }
        if let Some(syms) = self.design.enum_syms.get(&name) {
            // The declared-first variant: `enum Phase { Idle, Run }` -> Idle.
            let first = syms.keys().copied().min()?;
            return Some(format!("{first}ULL"));
        }
        (self.structs.contains_key(&name)
            || self.type_name_is(&name, "unsigned")
            || self.type_name_is(&name, "signed"))
        .then(|| "0ULL".to_string())
    }

    fn c_fn_call(&self, callee: &ast::Expr, args: &[ast::Expr]) -> Result<String, String> {
        // `T()` / `unsigned[8]()`: the type's default (spec 3.29). Hardware
        // built these; the testbench had no case for a zero-argument type
        // call, so `Phase()` and `unsigned[8]()` failed as "unsupported call"
        // although `let e: Phase;` — the same default, implicitly — worked.
        if args.is_empty() {
            if let Some(v) = self.c_default_construction(callee) {
                return Ok(v);
            }
        }
        // A bare name, or `Type::name` for a static associated fn.
        let Some(key) = siox::ir::call_fn_key(callee) else {
            return Err("unsupported call in testbench expression".into());
        };
        let name = key.as_str();
        let Some(f) = self.fns.get(name) else {
            // Runtime-provided functions (std::rand).
            return match name {
                "exists" => {
                    let path = match args.first() {
                        Some(ast::Expr::StrLit { text, .. }) => text.clone(),
                        _ => return Err("exists() needs a literal path".into()),
                    };
                    // Resolve relative to the design's source directory, then
                    // escape for the C string literal.
                    let full = self
                        .design
                        .base_dir
                        .join(&path)
                        .to_string_lossy()
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"");
                    Ok(format!(
                        "({{ FILE *_f = fopen(\"{full}\", \"rb\"); signed _e = _f != 0; if (_f) fclose(_f); _e; }})"
                    ))
                }
                "read" | "read_to_string" => Err(format!(
                    "runtime `{name}()` is not compiled into the native binary yet; \
                     use it in initializer position (`let x: T[N] = {name}(..);`) \
                     or compile a test executable with `sioxc --test`"
                )),
                "rand" => Ok("sx_rand()".to_string()),
                "uniform" => Ok("sx_uniform()".to_string()),
                "randint" => {
                    let lo = self.expr(args.first().ok_or("randint needs bounds")?)?;
                    let hi = self.expr(args.get(1).ok_or("randint needs bounds")?)?;
                    Ok(format!("sx_randint(({lo}), ({hi}))"))
                }
                _ => Err(format!("unsupported call `{name}` in testbench expression")),
            };
        };
        if self.extern_fns.contains(name) {
            let mut converted = Vec::new();
            for (param, arg) in f.params.iter().filter(|param| !param.is_self).zip(args) {
                if param.ty.as_ref().is_some_and(|ty| self.type_is(ty, "real")) {
                    converted.push(self.c_real_operand(arg)?);
                } else if param
                    .ty
                    .as_ref()
                    .is_some_and(|ty| self.type_is(ty, "integer"))
                {
                    let rendered = self.expr(arg)?;
                    converted.push(self.c_integer_operand(arg, &rendered));
                } else {
                    converted.push(self.expr(arg)?);
                }
            }
            let call = format!("{name}({})", converted.join(", "));
            return Ok(
                if f.ret.as_ref().is_some_and(|ty| self.type_is(ty, "real")) {
                    format!("sx_b64({call})")
                } else {
                    call
                },
            );
        }
        let body = f.body.as_ref().ok_or("fn has no body")?;
        let real_return = f.ret.as_ref().is_some_and(|ty| self.type_is(ty, "real"));
        // Constant arguments fold statically (also the only way a recursive
        // fn like clog2 compiles here). The evaluator is integer-only, so a
        // declared real result must retain the typed native path below.
        if !real_return {
            let consts: Option<Vec<i64>> = args
                .iter()
                .map(|a| siox::ir::eval_const_fns(a, &HashMap::new(), self.fns, 0))
                .collect();
            if let Some(cs) = consts {
                let mut cenv = HashMap::new();
                for (p, v) in f.params.iter().filter(|p| !p.is_self).zip(cs) {
                    if let Some(n) = &p.name {
                        cenv.insert(n.text.clone(), v);
                    }
                }
                if let Some(v) = siox::ir::eval_const_stmts(&body.stmts, &cenv, self.fns, 0) {
                    return Ok(format!("{}ULL", v as u64));
                }
            }
        }
        if self.tmp.get() > 4096 {
            return Err(format!("fn `{name}` recurses without constant arguments"));
        }
        self.tmp.set(self.tmp.get() + 64);
        let mut env = HashMap::new();
        let mut types = HashMap::new();
        for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(n) = &p.name {
                if let Some(head) = p.ty.as_ref().and_then(type_head_name) {
                    types.insert(n.text.clone(), head.to_string());
                    if head == "real" {
                        env.insert(
                            n.text.clone(),
                            format!("sx_b64({})", self.c_real_operand(a)?),
                        );
                        continue;
                    }
                    // A `signed` vector reaching an `integer` parameter is a
                    // value, not a bit pattern: `compatible` allows the
                    // coercion, so it has to be value-preserving. Passing the
                    // raw bits made `abs(-5)` see 251 and return it unchanged.
                    if head == "integer" {
                        if let Some(w) = self.signed_vector_width(a) {
                            let v = self.expr(a)?;
                            env.insert(n.text.clone(), format!("((sx_value)sx_i64(({v}), {w}))"));
                            continue;
                        }
                    }
                }
                env.insert(n.text.clone(), format!("({})", self.expr(a)?));
                // The argument's width travels with it, as at the operator and
                // method inline sites. A nested inline reads `self'length` off
                // this parameter — signed's Ord tests the sign with
                // `self >> (self'length - 1)` — and without a binding that
                // shifted by 0, so `abs(-5)` came back as 251.
                if let Some(w) = self.arg_width(a) {
                    env.insert(format!("{}::length", n.text), format!("{w}ULL"));
                }
            }
        }
        self.fn_env.borrow_mut().push(env);
        self.fn_type_env.borrow_mut().push(types);
        let out = self.c_fn_stmts(&body.stmts, real_return);
        self.fn_type_env.borrow_mut().pop();
        self.fn_env.borrow_mut().pop();
        self.tmp.set(self.tmp.get() - 64);
        let value = out?;
        // The result is a value of the declared return type, so it wraps to
        // that width — the operator inlines beside this one already do. Left
        // unmasked, `neg(5) -> signed[8]` produced a full-width `0 - 5`, and
        // every consumer then read a 64-bit pattern where 8 bits were
        // declared: the sign test in signed's Ord looked at bit 7 of that.
        if real_return {
            return Ok(value);
        }
        match f.ret.as_ref().and_then(|ty| self.declared_width(ty)) {
            Some(w) if w > 0 && w < 64 => Ok(mask_c(&value, w)),
            _ => Ok(value),
        }
    }

    /// `return e;` / `if c { .. } else { .. }` chains as nested C ternaries.
    fn c_fn_stmts(&self, stmts: &[ast::Stmt], real_return: bool) -> Result<String, String> {
        match stmts.first() {
            Some(ast::Stmt::Return { value: Some(v), .. }) if real_return => {
                Ok(format!("sx_b64({})", self.c_real_operand(v)?))
            }
            Some(ast::Stmt::Return { value: Some(v), .. }) => Ok(format!("({})", self.expr(v)?)),
            Some(ast::Stmt::If(iff)) => {
                let c = self.expr(&iff.cond)?;
                let t = self.c_fn_stmts(&iff.then.stmts, real_return)?;
                let e = match iff.else_.as_deref() {
                    Some(ast::ElseBranch::Block(b)) => self.c_fn_stmts(&b.stmts, real_return)?,
                    _ => self.c_fn_stmts(&stmts[1..], real_return)?,
                };
                Ok(format!("(({c}) ? {t} : {e})"))
            }
            _ => Err("fn bodies compile as return/if chains only".into()),
        }
    }

    /// Translate a testbench expression to a C expression string.
    /// The C condition for a match pattern over `scrut` (a C expression), or
    /// `None` for a wildcard/always-match (spec 3.22). Or-patterns `||` their
    /// alternatives' conditions.
    fn pattern_cond(&self, pattern: &ast::Pattern, scrut: &str) -> Result<Option<String>, String> {
        Ok(match pattern {
            ast::Pattern::Wildcard => None,
            ast::Pattern::Path(p) if p.segments.len() >= 2 => {
                let d = self
                    .enums
                    .get(&p.segments[0].text)
                    .and_then(|m| m.get(&p.segments[1].text))
                    .copied()
                    .ok_or_else(|| format!("unknown variant `{}`", p.segments[1].text))?;
                Some(format!("(({scrut}) == {d}ULL)"))
            }
            ast::Pattern::BitPattern { text, .. } => {
                let (mask, value) = siox::syntax::bit_pattern_mask(text)
                    .ok_or_else(|| format!("bad bit pattern `{text}`"))?;
                Some(format!(
                    "((({scrut}) & {}) == {})",
                    c_word_literal(&mask),
                    c_word_literal(&value)
                ))
            }
            ast::Pattern::Or { alts, .. } => {
                let mut parts = Vec::new();
                for a in alts {
                    match self.pattern_cond(a, scrut)? {
                        None => return Ok(None),
                        Some(c) => parts.push(c),
                    }
                }
                Some(format!("({})", parts.join(" || ")))
            }
            ast::Pattern::Range { lo, hi, .. } if lo == hi => {
                Some(format!("(({scrut}) == {}ULL)", *lo as u64))
            }
            ast::Pattern::Range { lo, hi, .. } => Some(format!(
                "((({scrut}) >= {}ULL) && (({scrut}) <= {}ULL))",
                *lo as u64, *hi as u64
            )),
            _ => Some("0".to_string()),
        })
    }

    fn expr(&self, e: &ast::Expr) -> Result<String, String> {
        Ok(match e {
            ast::Expr::IfExpr {
                cond, then, els, ..
            } => {
                if self.is_real_operand(e) {
                    format!(
                        "sx_b64(({}) ? ({}) : ({}))",
                        self.expr(cond)?,
                        self.c_real_operand(then)?,
                        self.c_real_operand(els)?
                    )
                } else {
                    format!(
                        "(({}) ? ({}) : ({}))",
                        self.expr(cond)?,
                        self.expr(then)?,
                        self.expr(els)?
                    )
                }
            }
            // A match-expression: a first-match C ternary chain over the arms.
            ast::Expr::Match {
                scrutinee, arms, ..
            } => {
                let scrut = self.expr(scrutinee)?;
                let real = self.is_real_operand(e);
                // Build from the last arm backward.
                let mut out = String::from("0");
                for arm in arms.iter().rev() {
                    let val = match arm.value_expr() {
                        Some(v) if real => format!("sx_b64({})", self.c_real_operand(v)?),
                        Some(v) => self.expr(v)?,
                        None => "0".to_string(),
                    };
                    match self.pattern_cond(&arm.pattern, &scrut)? {
                        None => out = format!("({val})"), // wildcard: the default
                        Some(cond) => out = format!("({cond} ? ({val}) : {out})"),
                    }
                }
                out
            }
            ast::Expr::Int { text, .. } => c_word_literal(&parse_word_literal(text)),
            ast::Expr::SuffixLit { text, suffix, .. } => {
                let scale = ast::suffix_scale(&suffix.text).unwrap_or(1);
                if suffix.text == "Hz" || suffix.text.ends_with("Hz") {
                    format!("sx_b64((double)({text}) * {scale}.0)")
                } else {
                    format!("((uint64_t)({text}) * {scale}ULL)")
                }
            }
            ast::Expr::BitStrLit { base, digits, .. } => {
                let radix = match *base {
                    'x' => 16,
                    'o' => 8,
                    _ => 2,
                };
                c_word_literal(&parse_digits_words(digits, radix))
            }
            ast::Expr::CharLit { ch, .. } => logic_lit_value(*ch, self.enums).to_string(),
            // Conversions mask to the target width (testbench side).
            // A method call `recv.method(args)` (possibly nullary) inlines the
            // impl body as a C expression, before the conversion logic below.
            ast::Expr::Call { callee, args, .. }
                if matches!(callee.as_ref(), ast::Expr::Field { .. }) =>
            {
                if let ast::Expr::Field { base, field, .. } = callee.as_ref() {
                    return self.c_method_call(base, &field.text, args);
                }
                unreachable!()
            }
            // A free-function call — a user fn or a runtime one (`exists`,
            // `rand`) — rather than a type conversion.
            ast::Expr::Call { callee, args, .. }
                if matches!(callee.as_ref(), ast::Expr::Path(p)
                if p.segments.len() == 1 && {
                    let n = p.segments[0].text.as_str();
                    self.fns.contains_key(n)
                        || matches!(
                            n,
                            "exists" | "rand" | "randint" | "uniform" | "read" | "read_to_string"
                        )
                }) =>
            {
                return self.c_fn_call(callee, args);
            }
            ast::Expr::Call { callee, args, .. } => {
                // A nullary call is a function, not a conversion — hand it to
                // the call path rather than failing the build.
                let Some(arg) = args.first() else {
                    return self.c_fn_call(callee, args);
                };
                let v = self.expr(arg)?;
                let w = match callee.as_ref() {
                    ast::Expr::Index { base, index, .. }
                        if expr_path(base)
                            .as_deref()
                            .is_some_and(|h| self.families.contains(h)) =>
                    {
                        parse_u64(match index.as_ref() {
                            ast::Expr::Int { text, .. } => text,
                            _ => return Err("conversion width must be a constant here".into()),
                        })
                    }
                    ast::Expr::Path(p)
                        if p.segments.len() == 1 && p.segments[0].text == "resize" =>
                    {
                        match args.get(1) {
                            Some(ast::Expr::Int { text, .. }) => parse_u64(text),
                            // `resize(x, self::length)` inside an inlined
                            // operator impl: the bound width is a C literal
                            // like `8ULL` — recover the number.
                            Some(other) => {
                                let c = self.expr(other)?;
                                c.trim_end_matches("ULL").parse().map_err(|_| {
                                    "resize width must be a constant here".to_string()
                                })?
                            }
                            None => return Err("resize width must be a constant here".into()),
                        }
                    }
                    ast::Expr::Path(p)
                        if p.segments.len() == 1
                            && matches!(p.segments[0].text.as_str(), "integer" | "Char") =>
                    {
                        return Ok(format!("({v})"));
                    }
                    // An enum-derivation conversion (`Logic(u)`, `ULogic(x)`):
                    // representation-identity along the chain — pass through.
                    ast::Expr::Path(p)
                        if p.segments.len() == 1
                            && self.enums.contains_key(&p.segments[0].text) =>
                    {
                        return Ok(format!("({v})"));
                    }
                    _ => return self.c_fn_call(callee, args),
                };
                if w > 64 {
                    format!("((sx_value)({v}))")
                } else if w == 0 || w == 64 {
                    format!("({v})")
                } else {
                    format!("(({v}) & {}ULL)", (1u64 << w) - 1)
                }
            }
            // `::length`: an impl body's bound length (`self::length`), an
            // array's element count, or a name's bit width (they coincide for a
            // flat vector) — VHDL `'length`.
            ast::Expr::SysAttr { base, attr, .. } if attr.text == "length" => {
                let path = expr_path(base).ok_or("`'length` needs a named base")?;
                if let Some(v) = self
                    .fn_env
                    .borrow()
                    .last()
                    .and_then(|m| m.get(&format!("{path}::length")))
                {
                    return Ok(v.clone());
                }
                if let Some(n) = self.array_len(&path) {
                    return Ok(format!("{n}ULL"));
                }
                // Neither a declared width nor an element count. The message
                // named neither the value nor the attribute, and spelled the
                // attribute with the internal `::` key rather than the `'`
                // the language uses.
                format!(
                    "{}ULL",
                    self.name_width(&path).ok_or_else(|| format!(
                        "`{path}'length` is not known here: `{path}` has no declared \
                         width and no element count in the testbench"
                    ))?
                )
            }
            // Range bounds (VHDL `'left`/`'right`/`'high`/`'low`/`'ascending`).
            // A name reads as ascending `0..width-1`; hardware bounds are
            // const-folded in the IR, so this covers only bounds in emitted
            // testbench code.
            ast::Expr::SysAttr { base, attr, .. }
                if matches!(
                    attr.text.as_str(),
                    "left" | "right" | "high" | "low" | "ascending"
                ) =>
            {
                let path = expr_path(base);
                let bounds = path
                    .as_ref()
                    .and_then(|path| self.local_ranges.borrow().get(path).copied());
                let w = path
                    .as_ref()
                    .and_then(|path| self.name_width(path))
                    .unwrap_or(0) as i64;
                let (left, right) = bounds.unwrap_or((0, (w - 1).max(0)));
                let v = match attr.text.as_str() {
                    "left" => left,
                    "right" => right,
                    "low" => left.min(right),
                    "high" => left.max(right),
                    "ascending" => i64::from(left <= right),
                    _ => unreachable!(),
                };
                if attr.text == "ascending" {
                    format!("{v}ULL")
                } else {
                    // Bounds are language `integer` values. Cast through a
                    // signed ABI word so a negative bound sign-extends to the
                    // harness-wide value instead of becoming only 64 one-bits.
                    format!("((sx_value)(int64_t){v}LL)")
                }
            }
            ast::Expr::Path(p)
                if p.segments.len() == 1
                    && self
                        .fn_env
                        .borrow()
                        .last()
                        .is_some_and(|m| m.contains_key(&p.segments[0].text)) =>
            {
                self.fn_env.borrow().last().unwrap()[&p.segments[0].text].clone()
            }
            ast::Expr::Path(p)
                if p.segments.len() == 1 && self.locals.borrow().contains(&p.segments[0].text) =>
            {
                c_local_ident(&p.segments[0].text)
            }
            ast::Expr::Path(p) if p.segments.len() == 1 => {
                if let Some(&id) = self.map.get(&p.segments[0].text) {
                    format!("sx_read({})", id.0)
                } else if let Some(value) = self.const_exprs.get(&p.segments[0].text) {
                    value.clone()
                } else {
                    // Every binder has been consulted by now — a function
                    // parameter, a testbench local, a connected signal, a
                    // constant — so the name simply does not exist. Saying
                    // "cannot translate yet" here blamed the compiler for
                    // the reader's typo.
                    return Err(unknown_value(&p.segments[0].text));
                }
            }
            ast::Expr::Path(p) if p.segments.len() >= 2 => {
                // Enum::Variant -> discriminant.
                let d = self
                    .enums
                    .get(&p.segments[0].text)
                    .and_then(|m| m.get(&p.segments[1].text))
                    .ok_or_else(|| {
                        format!(
                            "`{}` is not a variant of `{}`",
                            p.segments[1].text, p.segments[0].text
                        )
                    })?;
                format!("{d}ULL")
            }
            // An element of a constant lookup table, at a constant index or a
            // runtime one. Constants are stored one scalar per name, so both
            // forms missed the table entirely and were reported as something
            // the emitter cannot translate.
            ast::Expr::Index { base, index, .. }
                if expr_path(base).is_some_and(|b| self.const_array_exprs.contains_key(&b)) =>
            {
                let values = &self.const_array_exprs[&expr_path(base).unwrap()];
                let idx = self.expr(index)?;
                // C folds the chain away when the index is a literal.
                let mut out = String::from("0ULL");
                for (k, value) in values.iter().enumerate().rev() {
                    out = format!("((({idx}) == {k}ULL) ? ({value}) : {out})");
                }
                out
            }
            // A *dynamic* array index — `a[i]` where `i` is not constant, so
            // the whole expression has no path. The elements are separate C
            // locals (or signals), so select between them with a ternary
            // chain: the same shape the hardware path builds as a mux tree,
            // which is why this was supported there and not here.
            ast::Expr::Index { base, index, .. }
                if expr_path(e).is_none()
                    && expr_path(base).is_some_and(|b| !self.array_elements(&b).is_empty()) =>
            {
                let elements = self.array_elements(&expr_path(base).unwrap());
                let idx = self.expr(index)?;
                // An out-of-range index reads 0, matching an undriven signal
                // rather than aliasing some other element.
                let mut out = String::from("0ULL");
                for (k, element) in elements.iter().enumerate().rev() {
                    let read = self.read_path(element)?;
                    out = format!("((({idx}) == {k}ULL) ? ({read}) : {out})");
                }
                out
            }
            // A constant bit slice of a packed value, before the generic
            // field/index path: `w[7..4]` has no `expr_path` to look up.
            ast::Expr::Index { base, index, .. } if self.c_bit_slice(base, index).is_some() => {
                self.c_bit_slice(base, index).unwrap()?
            }
            ast::Expr::Field { .. } | ast::Expr::Index { .. } => {
                let path = expr_path(e).ok_or_else(|| {
                    // Naming the base is the difference between "the tool has
                    // a gap" and a reader hunting a nameless message.
                    match expr_path_base(e) {
                        Some(b) => unsup(&b),
                        None => "unsupported field/index expression".to_string(),
                    }
                })?;
                // A struct-field / array-element of a testbench local reads its
                // mangled C local; otherwise it's a connected signal.
                if self.locals.borrow().contains(&path) {
                    c_local_ident(&path)
                } else {
                    let id = self.map.get(&path).ok_or_else(|| unsup(&path))?;
                    self.check_scalar(*id)?;
                    format!("sx_read({})", id.0)
                }
            }
            ast::Expr::Unary { op, rhs, .. } => {
                if *op == ast::UnOp::Not {
                    if let Some(value) = self.c_dispatch_not(rhs)? {
                        return Ok(value);
                    }
                }
                if *op == ast::UnOp::Neg && self.is_real_operand(rhs) {
                    return Ok(format!("sx_b64(-({}))", self.c_real_operand(rhs)?));
                }
                let r = self.expr(rhs)?;
                match op {
                    ast::UnOp::Not => format!("(!({r}))"),
                    ast::UnOp::Neg => format!("(-({r}))"),
                }
            }
            ast::Expr::Binary { op, lhs, rhs, .. } => {
                // Whole-string equality (`o == "hello"`): a string is a `Char`
                // array, so compare element by element (matches the runner).
                if matches!(op, ast::BinOp::Eq | ast::BinOp::Ne) {
                    if let Some(v) = self.c_string_cmp(op, lhs, rhs)? {
                        return Ok(v);
                    }
                }
                // A `Char` operand reads a code point, so a `'x'` literal
                // counterpart is its Unicode value (not a logic code).
                if self.is_char_operand(lhs) || self.is_char_operand(rhs) {
                    let a = self.c_char_operand(lhs)?;
                    let b = self.c_char_operand(rhs)?;
                    match op {
                        ast::BinOp::Div => return Ok(format!("sx_udiv(({a}), ({b}))")),
                        ast::BinOp::Shl => return Ok(format!("sx_shl(({a}), ({b}))")),
                        ast::BinOp::Shr => return Ok(format!("sx_shr(({a}), ({b}))")),
                        _ => {}
                    }
                    return Ok(format!("({a} {} {b})", c_binop(op)?));
                }
                // A char literal compared for (in)equality against an enum-typed
                // operand reads by its position in that enum (VHDL `T'pos`),
                // data-driven — so `state == 'g'` matches the stored
                // discriminant, not `'g'`'s logic value. Restricted to `==`/`!=`
                // with exactly one char-literal side so custom/arithmetic
                // operators on enums still dispatch normally.
                if matches!(op, ast::BinOp::Eq | ast::BinOp::Ne)
                    && (self.is_char_lit(lhs) ^ self.is_char_lit(rhs))
                {
                    if let Some(en) = self
                        .enum_operand_type(lhs)
                        .or_else(|| self.enum_operand_type(rhs))
                    {
                        let a = self.enum_operand_c(&en, lhs)?;
                        let b = self.enum_operand_c(&en, rhs)?;
                        return Ok(format!("({a} {} {b})", c_binop(op)?));
                    }
                }
                // A `real` operand switches to double semantics: reals read
                // their bits as `double`, integer literals coerce (`z.re == 10`
                // compares 10.0). A comparison yields an signed; arithmetic yields
                // the double's bit pattern (matching the runner).
                if self.is_real_operand(lhs) || self.is_real_operand(rhs) {
                    let a = self.c_real_operand(lhs)?;
                    let b = self.c_real_operand(rhs)?;
                    let e = format!("({a} {} {b})", c_binop(op)?);
                    let is_cmp = matches!(
                        op,
                        ast::BinOp::Eq
                            | ast::BinOp::Ne
                            | ast::BinOp::Lt
                            | ast::BinOp::Le
                            | ast::BinOp::Gt
                            | ast::BinOp::Ge
                    );
                    return Ok(if is_cmp {
                        e
                    } else {
                        format!("sx_b64((double){e})")
                    });
                }
                // A typed operand inlines its family's operator impl (signed's
                // signed Div/Ord), matching the runner.
                if let Some(v) = self.c_dispatch_binop(op, lhs, rhs)? {
                    return Ok(v);
                }
                let (a, o, c) = (self.expr(lhs)?, c_binop(op)?, self.expr(rhs)?);
                if self.is_integer_operand(lhs) || self.is_integer_operand(rhs) {
                    let signed_a = self.c_integer_operand(lhs, &a);
                    let signed_c = self.c_integer_operand(rhs, &c);
                    match op {
                        ast::BinOp::Div => {
                            return Ok(format!("sx_idiv({signed_a}, {signed_c})"));
                        }
                        ast::BinOp::Shr => {
                            return Ok(format!("sx_ishr({signed_a}, ({c}))"));
                        }
                        ast::BinOp::Eq
                        | ast::BinOp::Ne
                        | ast::BinOp::Lt
                        | ast::BinOp::Le
                        | ast::BinOp::Gt
                        | ast::BinOp::Ge => {
                            return Ok(format!("({signed_a} {o} {signed_c})"));
                        }
                        _ => {}
                    }
                }
                match op {
                    ast::BinOp::Div => format!("sx_udiv(({a}), ({c}))"),
                    ast::BinOp::Shl => format!("sx_shl(({a}), ({c}))"),
                    ast::BinOp::Shr => format!("sx_shr(({a}), ({c}))"),
                    _ => format!("({a} {o} {c})"),
                }
            }
            other => {
                // Say WHICH expression, so the report is actionable.
                return Err(format!(
                    "unsupported testbench expression: `{}`",
                    siox::syntax::pretty::expr_string(other)
                ));
            }
        })
    }

    /// The element paths of an array named `base`, in index order, stopping
    /// at the first gap. Elements are registered one by one as locals or
    /// signals, so their presence is what says how long the array is.
    fn array_elements(&self, base: &str) -> Vec<String> {
        let mut out = Vec::new();
        loop {
            let element = format!("{base}[{}]", out.len());
            if !self.locals.borrow().contains(&element) && !self.map.contains_key(&element) {
                return out;
            }
            out.push(element);
        }
    }

    /// Read one already-flattened path: a testbench local reads its mangled C
    /// name, anything else is a connected signal.
    /// A constant bit slice of a packed value (`w[7..4]`, `w[7]`, `w[..4]`).
    /// Hardware has had these since ranges landed and the testbench had none,
    /// so a design could compute a nibble and its test could not check it.
    /// Returns `None` when this is not a slice of a scalar — an array element
    /// resolves through its own signal instead.
    fn c_bit_slice(&self, base: &ast::Expr, index: &ast::Expr) -> Option<Result<String, String>> {
        let path = expr_path(base)?;
        // An array's elements are separate locals; only a packed scalar is
        // sliced by bit position.
        if !self.array_elements(&path).is_empty() {
            return None;
        }
        if !self.locals.borrow().contains(&path) && !self.map.contains_key(&path) {
            return None;
        }
        let konst = |e: &ast::Expr| siox::ir::eval_const_fns(e, &HashMap::new(), self.fns, 0);
        let declared = self.local_ranges.borrow().get(&path).copied();
        let (a, b) = match index {
            ast::Expr::Range { lo, hi, .. } => (konst(lo)?, konst(hi)?),
            ast::Expr::PartialRange { lo, hi, .. } => {
                let (left, right) = declared?;
                (
                    match lo {
                        Some(lo) => konst(lo)?,
                        None => left,
                    },
                    match hi {
                        Some(hi) => konst(hi)?,
                        None => right,
                    },
                )
            }
            _ => {
                let n = konst(index)?;
                (n, n)
            }
        };
        Some(self.c_bit_slice_of(&path, a, b))
    }

    /// The C expression for bits `a..b` of `path`. A descending range is the
    /// natural order and shifts out directly; an ascending one names the same
    /// bits with their significance reversed (spec 3.13), so it is assembled
    /// bit by bit — the width is a constant, so the unrolling is bounded.
    fn c_bit_slice_of(&self, path: &str, a: i64, b: i64) -> Result<String, String> {
        let v = self.read_path(path)?;
        let (hi, lo) = (a.max(b), a.min(b));
        if hi < 0 || lo < 0 {
            return Err(format!("`{path}` sliced with a negative bound"));
        }
        let width = (hi - lo + 1) as u32;
        if a >= b {
            let mask = if width >= 64 {
                u64::MAX
            } else {
                (1u64 << width) - 1
            };
            return Ok(format!("((({v}) >> {lo}) & {mask}ULL)"));
        }
        let mut parts = Vec::new();
        for k in 0..width {
            let from = a as u32 + k;
            let to = width - 1 - k;
            parts.push(format!("(((({v}) >> {from}) & 1ULL) << {to})"));
        }
        Ok(format!("({})", parts.join(" | ")))
    }

    fn read_path(&self, path: &str) -> Result<String, String> {
        if self.locals.borrow().contains(path) {
            return Ok(c_local_ident(path));
        }
        let id = self.map.get(path).ok_or_else(|| unsup(path))?;
        self.check_scalar(*id)?;
        Ok(format!("sx_read({})", id.0))
    }

    /// Reject `real` signals in scalar expressions — native stimulus is
    /// integer-word only for now. A `Char` reads as its code point (a plain
    /// integer), so it is allowed.
    fn check_scalar(&self, id: SignalId) -> Result<(), String> {
        let s = &self.design.signals[id.0 as usize];
        if s.real {
            return Err(format!(
                "signal `{}` is real; siox build does not support real testbenches \
                 yet (compile with `sioxc --test`)",
                s.path
            ));
        }
        Ok(())
    }
}

/// A form the emitter has no translation for — a real gap in the tool.
fn unsup(name: &str) -> String {
    format!("testbench references `{name}`, which siox build cannot translate yet")
}

/// A name nothing declares — a mistake in the source, not a gap in the tool.
/// The two used to share `unsup`'s wording, which told a reader with a typo
/// to go wait for a compiler feature.
fn unknown_value(name: &str) -> String {
    format!(
        "no value named `{name}` is in scope: it has to be a testbench local, \
         a connected signal, a constant, or a parameter"
    )
}

/// Map a siox binary operator to its C spelling. Word-logical ops become
/// boolean C operators (matching the interpreter's semantics).
fn c_binop(op: &ast::BinOp) -> Result<&'static str, String> {
    use ast::BinOp::*;
    Ok(match op {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "/",
        Shl => "<<",
        Shr => ">>",
        Eq => "==",
        Ne => "!=",
        Lt => "<",
        Le => "<=",
        Gt => ">",
        Ge => ">=",
        And => "&",
        Or => "|",
        _ => {
            return Err(format!(
                "unsupported operator `{}` in testbench expression",
                siox::syntax::pretty::bin_op(op)
            ))
        }
    })
}

// --- helpers (small replicas of interpreter internals) ---------------------

/// A `clk = !clk after d;` self-toggle: `Some((clock path, half period))`.
fn after_toggle(
    target: &ast::Expr,
    value: &ast::Expr,
    after: &Option<ast::Expr>,
) -> Result<Option<(String, u64)>, String> {
    let Some(delay) = after.as_ref() else {
        return Ok(None);
    };
    let Some(path) = expr_path(target) else {
        return Ok(None);
    };
    if let ast::Expr::Unary {
        op: ast::UnOp::Not,
        rhs,
        ..
    } = value
    {
        if expr_path(rhs).as_deref() == Some(path.as_str()) {
            let half = duration_fs(std::slice::from_ref(delay))?.max(1);
            return Ok(Some((path, half)));
        }
    }
    Ok(None)
}

/// Collect the background clocks in a test's body — `clock(clk, period)` calls
/// and the VHDL-style `clk = !clk after half;` idiom: (signal id, half fs).
fn scan_clocks(
    items: &[&ast::ImplItem],
    aliases: &HashMap<String, Vec<SignalId>>,
) -> Result<Vec<(u32, u64)>, String> {
    let mut clocks: Vec<(u32, u64)> = Vec::new();
    let mut add = |id: u32, half: u64| {
        if !clocks.iter().any(|(c, _)| *c == id) {
            clocks.push((id, half));
        }
    };
    for item in items {
        if let ast::ImplItem::Stmt(ast::Stmt::Assign {
            target,
            value,
            after,
            ..
        }) = item
        {
            if let Some((path, half)) = after_toggle(target, value, after)? {
                // A clock shared by several DUTs toggles every port.
                for id in aliases.get(&path).map(|v| v.as_slice()).unwrap_or(&[]) {
                    add(id.0, half);
                }
            }
        }
    }
    Ok(clocks)
}

/// The femtosecond duration of `10ns` / `10.ns`; a missing/unknown form
/// defaults to the runner's half period (5 ns).
fn duration_fs(args: &[ast::Expr]) -> Result<u64, String> {
    let scaled = |text: &str, unit: &str| {
        let value = try_parse_u64(text)
            .ok_or_else(|| format!("duration value `{text}` does not fit the native timeline"))?;
        let scale = u64::try_from(ast::suffix_scale(unit).unwrap_or(1_000_000))
            .map_err(|_| format!("time unit `{unit}` is too large for the native timeline"))?;
        value.checked_mul(scale).ok_or_else(|| {
            format!("duration `{text}{unit}` exceeds the native 64-bit femtosecond timeline")
        })
    };
    match args.first() {
        Some(ast::Expr::SuffixLit { text, suffix, .. }) => scaled(text, &suffix.text),
        Some(ast::Expr::Field { base, field, .. }) => {
            if let ast::Expr::Int { text, .. } = base.as_ref() {
                scaled(text, &field.text)
            } else {
                Ok(5_000_000)
            }
        }
        _ => Ok(5_000_000),
    }
}

fn is_test_entity(modules: &[Module], entity: &str) -> bool {
    for m in modules {
        for it in &m.items {
            if let ast::Item::Entity(e) = it {
                if e.name.text == entity {
                    return e
                        .attrs
                        .iter()
                        .any(|a| a.name.segments.last().map(|s| s.text.as_str()) == Some("test"));
                }
            }
        }
    }
    false
}

fn build_map(
    hier: &Hierarchy,
    root: InstanceId,
    design: &Design,
) -> (HashMap<String, SignalId>, HashMap<String, Vec<SignalId>>) {
    // DUTs lower per-instance under the testbench path (`<test>.<inst>.<port>`),
    // so two instances of one entity stay distinct (matches siox-run's map).
    // `aliases` keeps EVERY binding of a name (one clock into many DUTs), so a
    // write drives all connected ports.
    let tb = &hier.instance(root).entity;
    let mut map = HashMap::new();
    let mut aliases: HashMap<String, Vec<SignalId>> = HashMap::new();
    for &child_id in &hier.instance(root).children {
        let child = hier.instance(child_id);
        // Expose the instance's own signals under `<inst>.<rest>` so
        // post-declaration access (`dut.a = x;`, `dut.y`) resolves directly to
        // the DUT's port signal — the third connection form (spec 3.12).
        let iprefix = format!("{tb}.{}.", child.name);
        for (i, sig) in design.signals.iter().enumerate() {
            if let Some(rest) = sig.path.strip_prefix(&iprefix) {
                let key = format!("{}.{}", child.name, rest);
                let id = SignalId(i as u32);
                map.entry(key.clone()).or_insert(id);
                aliases.entry(key).or_default().push(id);
            }
        }
        for c in &child.connections {
            let prefix = format!("{tb}.{}.{}", child.name, c.port);
            for (i, sig) in design.signals.iter().enumerate() {
                let id = SignalId(i as u32);
                if sig.path == prefix {
                    map.insert(c.signal.clone(), id);
                    aliases.entry(c.signal.clone()).or_default().push(id);
                } else if let Some(suffix) = sig.path.strip_prefix(&prefix) {
                    if suffix.starts_with('.') || suffix.starts_with('[') {
                        let key = format!("{}{suffix}", c.signal);
                        map.insert(key.clone(), id);
                        aliases.entry(key).or_default().push(id);
                    }
                }
            }
        }
    }
    (map, aliases)
}

fn test_items<'a>(modules: &'a [Module], entity: &str) -> Vec<&'a ast::ImplItem> {
    let mut items = Vec::new();
    for m in modules {
        for it in &m.items {
            if let ast::Item::Impl(im) = it {
                if im.trait_.is_none() && type_head_name(&im.target) == Some(entity) {
                    items.extend(im.items.iter());
                }
            }
        }
    }
    items
}

fn type_head_name(t: &ast::Type) -> Option<&str> {
    match t {
        ast::Type::Path(p) => p.segments.last().map(|s| s.text.as_str()),
        ast::Type::Generic { base, .. } | ast::Type::Indexed { base, .. } => type_head_name(base),
        ast::Type::View { view, .. } => view.segments.last().map(|i| i.text.as_str()),
    }
}

/// A single string value rather than an outer array of strings.
fn is_single_string_type(ty: &ast::Type) -> bool {
    match ty {
        ast::Type::Path(path) => path.segments.last().map(|id| id.text.as_str()) == Some("string"),
        ast::Type::Indexed { base, .. } => matches!(
            base.as_ref(),
            ast::Type::Path(path)
                if path.segments.last().map(|id| id.text.as_str()) == Some("string")
        ),
        _ => false,
    }
}

fn sized_string_indices(
    ty: &ast::Type,
    const_ranges: &HashMap<String, (i64, i64)>,
    consts: &HashMap<String, u128>,
) -> Option<Vec<i64>> {
    let ast::Type::Indexed {
        base,
        index: Some(index),
        ..
    } = ty
    else {
        return None;
    };
    if !matches!(
        base.as_ref(),
        ast::Type::Path(path)
            if path.segments.last().map(|id| id.text.as_str()) == Some("string")
    ) {
        return None;
    }
    index_values(index, const_ranges, consts)
}

fn index_values(
    index: &ast::Expr,
    const_ranges: &HashMap<String, (i64, i64)>,
    consts: &HashMap<String, u128>,
) -> Option<Vec<i64>> {
    match index {
        ast::Expr::Int { text, .. } => {
            let count = i64::try_from(try_parse_u64(text)?).ok()?;
            Some((0..count).collect())
        }
        ast::Expr::Range { lo, hi, .. } => {
            let left = signed_index_bound(lo)?;
            let right = signed_index_bound(hi)?;
            Some(directional_indices(left, right))
        }
        ast::Expr::Path(path) if path.segments.len() == 1 => {
            let name = &path.segments[0].text;
            if let Some(&(left, right)) = const_ranges.get(name) {
                Some(directional_indices(left, right))
            } else {
                let count = i64::try_from(*consts.get(name)?).ok()?;
                Some((0..count).collect())
            }
        }
        _ => None,
    }
}

fn directional_indices(left: i64, right: i64) -> Vec<i64> {
    if left <= right {
        (left..=right).collect()
    } else {
        (right..=left).rev().collect()
    }
}

fn type_index_bounds(
    ty: &ast::Type,
    const_ranges: &HashMap<String, (i64, i64)>,
    consts: &HashMap<String, u128>,
) -> Option<(i64, i64)> {
    let ast::Type::Indexed {
        index: Some(index), ..
    } = ty
    else {
        return None;
    };
    let indices = index_values(index, const_ranges, consts)?;
    Some((*indices.first()?, *indices.last()?))
}

fn signed_index_bound(expr: &ast::Expr) -> Option<i64> {
    match expr {
        ast::Expr::Int { text, .. } => i64::try_from(try_parse_u64(text)?).ok(),
        ast::Expr::Unary {
            op: ast::UnOp::Neg,
            rhs,
            ..
        } => signed_index_bound(rhs)?.checked_neg(),
        _ => None,
    }
}

/// Phase-1 C ABI mapping for foreign function declarations. `real` is the
/// native C `double`, language `integer` is the signed ABI word, and packed
/// scalar values cross as the corresponding unsigned word.
fn extern_c_type(ty: Option<&ast::Type>, aliases: &HashMap<String, ast::Type>) -> &'static str {
    match ty.and_then(|ty| resolved_type_name(ty, aliases)) {
        Some("real") => "double",
        Some("integer") => "int64_t",
        _ => "uint64_t",
    }
}

fn resolved_type_name<'a>(
    ty: &'a ast::Type,
    aliases: &'a HashMap<String, ast::Type>,
) -> Option<&'a str> {
    let mut ty = ty;
    let mut seen = std::collections::HashSet::new();
    loop {
        let name = type_head_name(ty)?;
        let ast::Type::Path(path) = ty else {
            return Some(name);
        };
        if path.segments.len() != 1 || !seen.insert(name) {
            return Some(name);
        }
        let Some(alias) = aliases.get(name) else {
            return Some(name);
        };
        ty = alias;
    }
}

fn expr_path(e: &ast::Expr) -> Option<String> {
    match e {
        ast::Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
        ast::Expr::Field { base, field, .. } => {
            Some(format!("{}.{}", expr_path(base)?, field.text))
        }
        ast::Expr::Index { base, index, .. } => Some(format!(
            "{}[{}]",
            expr_path(base)?,
            signed_index_bound(index)?
        )),
        _ => None,
    }
}

/// The innermost named base of a field/index chain, for diagnostics: `a[i].x`
/// has no path but is still recognisably about `a`.
fn expr_path_base(e: &ast::Expr) -> Option<String> {
    match e {
        ast::Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
        ast::Expr::Field { base, .. } | ast::Expr::Index { base, .. } => expr_path_base(base),
        _ => None,
    }
}

fn descendant_suffix<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
    name.strip_prefix(prefix)
        .filter(|rest| rest.starts_with('[') || rest.starts_with('.'))
}

fn str_lit(e: &ast::Expr) -> Option<String> {
    match e {
        ast::Expr::StrLit { text, .. } => Some(text.clone()),
        _ => None,
    }
}

fn parse_u64(text: &str) -> u64 {
    try_parse_u64(text).unwrap_or(0)
}

fn try_parse_u64(text: &str) -> Option<u64> {
    let normalized = text.trim().replace('_', "");
    let t = normalized.as_str();
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).ok()
    } else if let Some(bin) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        u64::from_str_radix(bin, 2).ok()
    } else {
        t.parse().ok()
    }
}

/// A logic character's value: its position in std's default logic type
/// (`ULogic`), read from the parsed enum declaration — the emitter holds no
/// value table of its own. `0` if the char is not one of that type's variants.
fn logic_lit_value(c: char, enums: &HashMap<String, HashMap<String, u64>>) -> u64 {
    enums
        .get(siox::ir::DEFAULT_LOGIC_TYPE)
        .and_then(|m| m.get(&format!("'{c}'")))
        .copied()
        .unwrap_or(0)
}
