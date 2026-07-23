//! `siox build` — compile a design + its `#[test]` stimulus into a standalone
//! native simulator binary (stage B5.1).
//!
//! The DUT lowers to a native object (`sx_*` C ABI) via the LLVM backend; the
//! testbench statements are translated to a C `main` that drives it. clang
//! links them into an executable that runs *every* `#[test]` (one C function
//! per test, a libtest-style `main`) and reports results + exit code. All
//! tests share the one lowered Design (one `sx_*` namespace); `sx_reset`
//! zeroes state between them. First cut: integer/logic/bool designs;
//! real/char/string testbenches are a follow-on.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use siox::elab::{Hierarchy, InstanceId};
use siox::ir::{Design, SignalId};
use siox::syntax::ast;
use siox::syntax::Module;

/// Build a native simulator binary that runs *all* `#[test]` entities, like
/// rustc's test harness. Every test's DUT is in the one lowered `Design` (one
/// `sx_*` namespace); `sx_reset` zeroes all state, so tests run sequentially
/// in the same object.
pub fn build(modules: &[Module], hier: &Hierarchy, design: &Design, out: &Path) -> Result<(), String> {
    if let Some(s) = design.signals.iter().find(|s| s.width > 64) {
        return Err(format!("signal `{}` is {} bits; siox build is 64-bit only", s.path, s.width));
    }
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
    let mut op_impls: HashMap<(String, String), Vec<(&ast::FnDecl, Option<String>)>> =
        HashMap::new();
    for m in modules {
        for item in &m.items {
            if let ast::Item::Impl(im) = item {
                let tr = im.trait_.as_ref().and_then(|t| t.segments.last());
                if let (Some(tr), Some(ty)) = (tr, type_head_name(&im.target)) {
                    let operator = if tr.text == "custom" {
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
                    let input_index = usize::from(tr.text == "custom");
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
    let mut fns: HashMap<String, &ast::FnDecl> = HashMap::new();
    for m in modules {
        for item in &m.items {
            if let ast::Item::Fn(f) = item {
                fns.insert(f.name.text.clone(), f);
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

    // Struct field layouts (base-first, inheritance flattened) so a struct-typed
    // testbench local can be materialized as one C local per leaf field. Every
    // impl method by (type head, name), for `recv.method(args)` in stimulus.
    let structs = collect_structs(modules);
    let methods = collect_methods(modules);
    let derived_widths = siox::ir::derived_widths(modules);

    // Header, one `int test_<name>(void)` per test, then a libtest-style main.
    let mut prog = String::new();
    prog.push_str("#include <stdint.h>\n#include <stdio.h>\n#include <string.h>\n");
    prog.push_str("extern void sx_reset(void);\n");
    prog.push_str("extern void sx_set(uint32_t, uint64_t);\n");
    prog.push_str("extern uint64_t sx_read(uint32_t);\n");
    prog.push_str("extern void sx_settle(void);\n");
    prog.push_str("static const char *g_msg;\n");
    prog.push_str("static int g_warnings;\n");
    prog.push_str("static double sx_f64(uint64_t b) { double d; memcpy(&d, &b, 8); return d; }\n");
    prog.push_str("static uint64_t sx_b64(double d) { uint64_t b; memcpy(&b, &d, 8); return b; }\n");
    // xorshift64* with the runner's constants: identical random sequences.
    prog.push_str(
        "static uint64_t g_rand = 0x9E3779B97F4A7C15ULL;\n\
         static uint64_t sx_rand(void) {\n\
         \x20   g_rand ^= g_rand >> 12; g_rand ^= g_rand << 25; g_rand ^= g_rand >> 27;\n\
         \x20   return g_rand * 0x2545F4914F6CDD1DULL;\n}\n",
    );
    // The event wheel: earliest pending clock edge, and one step of the
    // scheduler (advance to that edge, toggle the due clocks, settle).
    prog.push_str(
        "static uint64_t sx_next_edge(const uint64_t *next, int n) {\n\
         \x20   uint64_t t = UINT64_MAX;\n\
         \x20   for (int i = 0; i < n; i++) if (next[i] < t) t = next[i];\n\
         \x20   return t;\n}\n\
         static int sx_step_clock(uint64_t *now, uint64_t *next, const uint32_t *cid,\n\
         \x20                        const uint64_t *half, int n) {\n\
         \x20   uint64_t t = sx_next_edge(next, n);\n\
         \x20   if (t == UINT64_MAX) return 0;\n\
         \x20   if (t > *now) *now = t;\n\
         \x20   for (int i = 0; i < n; i++)\n\
         \x20       if (next[i] == t) { sx_set(cid[i], !sx_read(cid[i])); next[i] += half[i]; }\n\
         \x20   sx_settle();\n\
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
        prog.push_str("static int sx_check_ranges(void) {\n    int64_t v;\n");
        for (id, sig) in &ranged {
            let (lo, hi) = sig.range.unwrap();
            let decode = if lo < 0 && sig.width > 0 && sig.width < 64 {
                format!(
                    "v = (int64_t)sx_read({id}); if (v & {s}LL) v -= {m}LL;",
                    s = 1u64 << (sig.width - 1),
                    m = 1u64 << sig.width
                )
            } else {
                format!("v = (int64_t)sx_read({id});")
            };
            prog.push_str(&format!(
                "    {decode}\n    if (v < {lo}LL || v > {hi}LL) {{ g_msg = \"`{}` left its range {lo}..{hi}\"; return 1; }}\n",
                sig.path
            ));
        }
        prog.push_str("    return 0;\n}\n\n");
    } else {
        prog.push_str("static int sx_check_ranges(void) { return 0; }\n\n");
    }

    let mut names = Vec::new();
    for &root in &tests {
        let name = hier.instance(root).entity.clone();
        let (map, aliases) = build_map(hier, root, design);
        let items = test_items(modules, &name);
        let clocks = scan_clocks(&items, &aliases);
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
            local_families: Default::default(),
            local_types: Default::default(),
            op_impls: &op_impls,
            methods: &methods,
            structs: &structs,
            derived_widths: &derived_widths,
            consts: &consts,
            aliases: &aliases,
            tmp: Default::default(),
            fns: &fns,
            fn_env: Default::default(),
            instance_names,
        };
        prog.push_str(&ctx.gen_test_fn(&items)?);
        names.push(name);
    }
    prog.push_str(&gen_main(&names));

    // Emit the DUT object (all tests' logic) and link with clang.
    let tmp = std::env::temp_dir().join(format!("siox_build_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let obj = tmp.join("design.o");
    let csrc = tmp.join("sim.c");
    siox_llvm::emit_object(design, &obj)?;
    std::fs::write(&csrc, &prog).map_err(|e| e.to_string())?;
    if std::env::var("SIOX_DEBUG_C").is_ok() {
        let _ = std::fs::write("/tmp/siox_debug.c", &prog);
    }
    let status = Command::new("clang")
        .args([csrc.to_str().unwrap(), obj.to_str().unwrap(), "-O2", "-lm", "-o", out.to_str().unwrap()])
        .status()
        .map_err(|e| format!("failed to run clang: {e}"))?;
    let _ = std::fs::remove_dir_all(&tmp);
    if !status.success() {
        return Err("clang failed to link the simulator".into());
    }
    Ok(())
}

/// The libtest-style `main` that runs each `test_<name>` and reports results.
/// Takes an optional name-substring filter as `argv[1]`, like a rustc test
/// binary (`./testbin <filter>`).
fn gen_main(names: &[String]) -> String {
    let mut m = String::new();
    m.push_str("int main(int argc, char **argv) {\n");
    m.push_str("    const char *filter = argc > 1 ? argv[1] : 0;\n");
    m.push_str("    int failed = 0, ran = 0, filtered = 0;\n");
    // Count how many tests match, so the "running N tests" line is post-filter.
    for n in names {
        m.push_str(&format!(
            "    if (!filter || strstr(\"{n}\", filter)) ran++; else filtered++;\n"
        ));
    }
    m.push_str("    printf(\"\\nrunning %d test%s\\n\", ran, ran == 1 ? \"\" : \"s\");\n");
    for n in names {
        m.push_str(&format!(
            "    if (!filter || strstr(\"{n}\", filter)) {{ \
             if (test_{n}()) {{ printf(\"test {n} ... FAILED\\n    %s\\n\", g_msg); failed++; }} \
             else printf(\"test {n} ... ok\\n\"); }}\n"
        ));
    }
    m.push_str(
        "    printf(\"\\ntest result: %s. %d passed; %d failed; %d filtered out\",\n\
         \x20          failed ? \"FAILED\" : \"ok\", ran - failed, failed, filtered);\n\
         \x20   if (g_warnings) printf(\"; %d warning%s\", g_warnings, g_warnings == 1 ? \"\" : \"s\");\n\
         \x20   printf(\"\\n\");\n",
    );
    m.push_str("    return failed ? 1 : 0;\n}\n");
    m
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
    /// Declared bit width of a C local (`let c: uint[8]` -> 8): writes mask to
    /// it so arithmetic wraps exactly like the equivalent hardware signal.
    local_widths: std::cell::RefCell<HashMap<String, u32>>,
    /// Declared vector family of a testbench name (`let a: int[8]` -> "int"),
    /// connected or local — operators on it inline the family's impls.
    local_families: std::cell::RefCell<HashMap<String, String>>,
    /// Operator-trait impls `(trait, type) -> fn`, mirroring the runner.
    op_impls: &'a HashMap<(String, String), Vec<(&'a ast::FnDecl, Option<String>)>>,
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
    consts: &'a HashMap<String, u128>,
    /// Testbench name -> EVERY connected port's signal id (a write drives all).
    aliases: &'a HashMap<String, Vec<SignalId>>,
    /// Unique-suffix counter for generated C identifiers.
    tmp: std::cell::Cell<usize>,
    /// Module-level functions (testbench-callable; translated to C ternaries).
    fns: &'a HashMap<String, &'a ast::FnDecl>,
    /// Parameter-substitution stack while translating a fn body.
    fn_env: std::cell::RefCell<Vec<HashMap<String, String>>>,
    /// Names elaboration turned into DUT instances (`let dut: Sub = {..}` /
    /// `let dut: Sub [= {..}]`) — their `let`s are wired by elaboration and
    /// emit no testbench code.
    instance_names: std::collections::HashSet<String>,
}

/// Struct layouts keyed by name, each a base-first flattened field list
/// (`struct B : A` prepends A's fields), so a struct-typed testbench local can
/// be materialized as one C local per field.
fn collect_structs(modules: &[Module]) -> HashMap<String, Vec<(String, ast::Type)>> {
    let mut raw: HashMap<String, (Option<String>, Vec<(String, ast::Type)>)> = HashMap::new();
    for m in modules {
        for item in &m.items {
            if let ast::Item::Struct(s) = item {
                let base = s.base.as_ref().and_then(type_head_name).map(str::to_string);
                let own = s.fields.iter().map(|f| (f.name.text.clone(), f.ty.clone())).collect();
                raw.insert(s.name.text.clone(), (base, own));
            }
        }
    }
    fn flat(
        name: &str,
        raw: &HashMap<String, (Option<String>, Vec<(String, ast::Type)>)>,
        depth: usize,
    ) -> Vec<(String, ast::Type)> {
        if depth > 32 {
            return Vec::new();
        }
        let Some((base, own)) = raw.get(name) else { return Vec::new() };
        let mut out = match base {
            Some(b) => flat(b, raw, depth + 1),
            None => Vec::new(),
        };
        out.extend(own.iter().cloned());
        out
    }
    raw.keys().map(|k| (k.clone(), flat(k, &raw, 0))).collect()
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
                            out.entry((ty.to_string(), f.name.text.clone())).or_insert(f);
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
    let ast::Expr::Call { callee, args, .. } = e else { return None };
    let ast::Expr::Path(p) = callee.as_ref() else { return None };
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
        ast::Expr::Int { text, .. } => Some(parse_u64(text) as u128),
        ast::Expr::Bool { value, .. } => Some(*value as u128),
        ast::Expr::CharLit { ch, .. } => Some(logic_lit_value(*ch, enums) as u128),
        ast::Expr::Path(p) if p.segments.len() == 1 => consts.get(&p.segments[0].text).copied(),
        ast::Expr::Path(p) if p.segments.len() >= 2 => enums
            .get(&p.segments[0].text)
            .and_then(|m| m.get(&p.segments[1].text))
            .map(|&d| d as u128),
        _ => {
            let env: HashMap<String, i64> =
                consts.iter().map(|(k, &v)| (k.clone(), v as i64)).collect();
            siox::ir::eval_const_fns(e, &env, fns, 0).map(|v| v as u128)
        }
    }
}

/// Wrap a C expression so it masks to `w` bits (wrap at 2^w).
fn mask_c(e: &str, w: u32) -> String {
    if w > 0 && w < 64 {
        format!("(({e}) & {:#x}ULL)", (1u64 << w) - 1)
    } else {
        e.to_string()
    }
}

impl Ctx<'_> {
    /// A testbench local initialized from a `std::fs` file read
    /// (`let s: string = read_to_string("path")`, `let m: uint[8][N] = read(..)`).
    /// The file is read at **build time** (matching the corpus's stable fixtures)
    /// to size and fill the local: one `Char`/byte element per index. Returns
    /// `true` when handled. `read`/`read_to_string` in *initializer* position of
    /// a DUT signal is baked by the IR; this covers the testbench-local case.
    fn try_declare_fs_read_local(&self, l: &ast::LetDecl, b: &mut String) -> Result<bool, String> {
        let Some(value) = &l.value else { return Ok(false) };
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
            self.local_types.borrow_mut().insert(name.clone(), head.to_string());
        }
        for (i, &code) in codes.iter().enumerate() {
            let key = format!("{name}[{i}]");
            // A connected element writes its signal; an unconnected local gets
            // its own C variable, registered so `name[i]` reads resolve to it.
            if let Some(&id) = self.map.get(&key) {
                b.push_str(&format!("    sx_set({}, {code}ULL);\n", id.0));
            } else {
                b.push_str(&format!("    uint64_t {} = {code}ULL;\n", c_local_ident(&key)));
                self.locals.borrow_mut().insert(key);
            }
        }
        Ok(true)
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
        let Some(head) = l.ty.as_ref().and_then(|t| type_head_name(t)) else { return Ok(false) };
        // Only a genuine field-aggregate is expanded into per-field locals. A
        // type that *inherits from an array* — `uint`/`int` (`struct uint :
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
            return Ok(false);
        }
        self.local_types.borrow_mut().insert(l.name.text.clone(), head.to_string());
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
            Some(ast::Expr::Construct { spread: Some(base), .. }) => expr_path(base),
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
            // A nested *field-aggregate* field expands to its own leaves; a
            // field that inherits from an array (a `uint`/`int`/enum vector,
            // which has no fields) is a scalar leaf.
            let fhead = type_head_name(fty);
            if let Some(sub) = fhead.and_then(|h| self.structs.get(h)).filter(|f| !f.is_empty()) {
                self.local_types.borrow_mut().insert(key.clone(), fhead.unwrap().to_string());
                let sub_base = spread_base.map(|s| format!("{s}.{fname}"));
                self.declare_struct_fields(&key, sub, &HashMap::new(), sub_base.as_deref(), b)?;
                continue;
            }
            if let Some((fam, w)) = self.declared_family(fty) {
                self.local_families.borrow_mut().insert(key.clone(), fam);
                self.local_widths.borrow_mut().insert(key.clone(), w);
            } else if let Some(w) = self.declared_width(fty) {
                self.local_widths.borrow_mut().insert(key.clone(), w);
            }
            let init_e = match init.get(fname.as_str()) {
                Some(v) => {
                    let e = self.expr(v)?;
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
            b.push_str(&format!("    uint64_t {} = {init_e};\n", c_local_ident(&key)));
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
        match value {
            ast::Expr::StrLit { text, .. } => {
                for (i, ch) in text.chars().enumerate() {
                    if let Some(&id) = self.map.get(&format!("{name}[{i}]")) {
                        b.push_str(&format!("{ind}sx_set({}, {}ULL);\n", id.0, ch as u32));
                    }
                }
                Ok(true)
            }
            ast::Expr::Construct { args, .. } => {
                for arg in args {
                    // Named `.field = v` only; positional needs struct field
                    // order (a follow-up). A value-less arg is parser recovery.
                    let Some(f) = &arg.field else { continue };
                    let field = format!("{name}.{}", f.text);
                    let Some(&id) = self.map.get(&field) else { continue };
                    let e = match &arg.value {
                        Some(v) => self.value_for(id, v)?,
                        None => "0".to_string(),
                    };
                    b.push_str(&format!("{ind}sx_set({}, {e});\n", id.0));
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// The declared `(family, width)` of a vector-family type (`int[8]` ->
    /// ("int", 8)). Mirrors the runner's rule.
    fn declared_family(&self, ty: &ast::Type) -> Option<(String, u32)> {
        if let ast::Type::Indexed { base, index: Some(i), .. } = ty {
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
            if let ast::Expr::Int { text, .. } = i.as_ref() {
                return Some((head.to_string(), text.parse().ok()?));
            }
        }
        None
    }

    /// The bit width a testbench name carries: a local's declared width or the
    /// connected signal's.
    fn name_width(&self, name: &str) -> Option<u32> {
        if let Some(&w) = self.local_widths.borrow().get(name) {
            return Some(w);
        }
        self.map.get(name).map(|&id| self.design.signals[id.0 as usize].width)
    }

    /// Translate `lhs op rhs` through the lhs family's operator impl as an
    /// inline C expression, when one exists — the native mirror of the
    /// runner's `dispatch_binop`. Comparisons derive from `Ord::cmp`.
    fn c_dispatch_binop(
        &self,
        op: &ast::BinOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
    ) -> Result<Option<String>, String> {
        let (fam, lname) = match lhs {
            ast::Expr::Path(p) if p.segments.len() == 1 => {
                let name = p.segments[0].text.clone();
                let family = self
                    .local_families
                    .borrow()
                    .get(&name)
                    .cloned()
                    .or_else(|| self.local_types.borrow().get(&name).cloned());
                match family {
                    Some(f) => (f, name),
                    None => return Ok(None),
                }
            }
            _ => return Ok(None),
        };
        let op_str = siox::syntax::pretty::bin_op(op);
        // `==`/`!=`: bit equality at the type's width (mask both sides).
        if matches!(op_str, "==" | "!=") {
            let Some(w) = self.name_width(&lname) else { return Ok(None) };
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
        let cmp = match op_str {
            "<" => Some((0u64, false)),
            ">" => Some((2, false)),
            ">=" => Some((0, true)),
            "<=" => Some((2, true)),
            _ => None,
        };
        let tr = match cmp {
            Some(_) => "Ord",
            None => siox::syntax::ast::op_trait_name(op_str).unwrap_or(op_str),
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
        let Some((f, _)) = selected else { return Ok(None) };
        let Some(body) = f.body.as_ref() else { return Ok(None) };

        let w = self.name_width(&lname).unwrap_or(0);
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
        let out = self.c_fn_stmts(&body.stmts);
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
        let ast::Expr::Path(p) = rhs else { return Ok(None) };
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
        let Some(family) = family else { return Ok(None) };
        let Some((f, _)) = self
            .op_impls
            .get(&("Not".to_string(), family))
            .and_then(|candidates| candidates.first())
        else {
            return Ok(None);
        };
        let Some(body) = f.body.as_ref() else { return Ok(None) };
        let width = self.name_width(&name).unwrap_or(0);
        let mut env = HashMap::new();
        env.insert("self".to_string(), format!("({})", self.expr(rhs)?));
        env.insert("self::length".to_string(), format!("{width}ULL"));
        self.fn_env.borrow_mut().push(env);
        let out = self.c_fn_stmts(&body.stmts);
        self.fn_env.borrow_mut().pop();
        let value = out?;
        Ok(Some(if width > 0 { mask_c(&value, width) } else { value }))
    }

    /// The declared bit width of a vector-family type: `uint[8]` -> 8 (and the
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
        if let ast::Type::Indexed { base, index: Some(i), .. } = ty {
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
            if let ast::Expr::Int { text, .. } = i.as_ref() {
                return text.parse().ok();
            }
        }
        None
    }

    /// `int test_<name>(void) { ... }` — 0 on pass, 1 on the first failed
    /// assert (printing its message first, like a panic).
    fn gen_test_fn(&self, items: &[&ast::ImplItem]) -> Result<String, String> {
        let mut b = String::new();
        b.push_str(&format!("int test_{}(void) {{\n    sx_reset();\n", self.name));

        // The test's event wheel: sim time + per-clock next-edge state. Arrays
        // are sized >=1 so clock-less tests still compile; `_nclk` grows as
        // `clock()` statements register (source order matches scan order).
        let n = self.clocks.len().max(1);
        let cid: Vec<String> = self.clocks.iter().map(|(c, _)| c.to_string()).collect();
        let half: Vec<String> = self.clocks.iter().map(|(_, h)| format!("{h}ULL")).collect();
        b.push_str(&format!(
            "    uint64_t _now = 0; (void)_now;\n             \x20   uint64_t _next[{n}] = {{{}}}; (void)_next;\n             \x20   static const uint32_t _cid[{n}] = {{{}}};\n             \x20   static const uint64_t _half[{n}] = {{{}}};\n             \x20   int _nclk = 0; (void)_nclk;\n",
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
                ast::ImplItem::Let(l) if self.try_declare_struct_local(l, &mut b)? => {}
                ast::ImplItem::Let(l) => match &l.value {
                    Some(ast::Expr::Construct { ty: Some(_), .. }) => {} // instance
                    value => {
                        // Record the vector family for every declared name
                        // (connected ports too): operators dispatch on it.
                        if let Some((fam, _)) =
                            l.ty.as_ref().and_then(|t| self.declared_family(t))
                        {
                            self.local_families.borrow_mut().insert(l.name.text.clone(), fam);
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
                                // the JIT + hardware paths.
                                Some(v) => match l
                                    .ty
                                    .as_ref()
                                    .and_then(type_head_name)
                                    .and_then(|h| self.enum_char_lit(h, v))
                                {
                                    Some(d) => format!("{d}ULL"),
                                    None => self.expr(v)?,
                                },
                                // Uninitialized: the type's `new()` default
                                // (`Logic` -> `'U'`), matching JIT + hardware.
                                None => l
                                    .ty
                                    .as_ref()
                                    .and_then(type_head_name)
                                    .and_then(|h| self.design.new_defaults.get(h))
                                    .map(|v| format!("{v}ULL"))
                                    .unwrap_or_else(|| "0".to_string()),
                            };
                            // A vector-family local wraps at its declared
                            // width, like the equivalent hardware signal.
                            let e = match l.ty.as_ref().and_then(|t| self.declared_width(t)) {
                                Some(w) => {
                                    self.local_widths.borrow_mut().insert(l.name.text.clone(), w);
                                    mask_c(&e, w)
                                }
                                None => e,
                            };
                            b.push_str(&format!("    uint64_t {} = {e};\n", l.name.text));
                            self.locals.borrow_mut().insert(l.name.text.clone());
                            // Record an enum/Logic local's type so `print!`
                            // renders its symbol, not the raw discriminant.
                            if let Some(h) = l.ty.as_ref().and_then(|t| type_head_name(t)) {
                                if self.enums.contains_key(h) {
                                    self.local_types.borrow_mut().insert(l.name.text.clone(), h.to_string());
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

        b.push_str("    return 0;\n}\n\n");
        // Post-settle range asserts (values persist, so checking at the next
        // settle also catches violations that occur inside await loops).
        let b = b.replace("sx_settle();", "sx_settle(); if (sx_check_ranges()) return 1;");
        Ok(b)
    }

    fn stmt(&self, s: &ast::Stmt, b: &mut String, depth: usize) -> Result<(), String> {
        let ind = "    ".repeat(depth);
        match s {
            ast::Stmt::Assign { target, value, after, .. } => {
                if after.is_some() {
                    // `clk = !clk after d;` registers on the event wheel; other
                    // delayed writes aren't compiled yet.
                    let (path, _) = after_toggle(target, value, after)
                        .ok_or("only the `clk = not clk after d` form of `after` is supported in the native binary yet (use `sioxc test`)")?;
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
                    let e = self.expr(value)?;
                    let e = match self.local_widths.borrow().get(&name) {
                        Some(&w) => mask_c(&e, w),
                        None => e,
                    };
                    b.push_str(&format!("{ind}{} = {e};\n", c_local_ident(&name)));
                    return Ok(());
                }
                let id = *self.map.get(&name).ok_or_else(|| format!("unknown signal `{name}`"))?;
                let e = self.value_for(id, value)?;
                // Drive every port this name connects to (sx_set masks to each
                // signal's width).
                b.push_str(&format!("{ind}{{ uint64_t _v = {e};"));
                for a in self.aliases.get(&name).map(|v| v.as_slice()).unwrap_or(&[]) {
                    b.push_str(&format!(" sx_set({}, _v);", a.0));
                }
                b.push_str(&format!(" }}\n{ind}sx_settle();\n"));
                let _ = id;
            }
            ast::Stmt::Expr(ast::Expr::Call { callee, args, bang, .. }) => {
                self.call(callee, args, *bang, b, depth)?;
            }
            ast::Stmt::For { var, range, body, .. } => {
                let v = &var.text;
                // `for x in xs`: iterate a DUT-connected array via an id table.
                if let Some((path, n)) =
                    expr_path(range).and_then(|p| self.array_len(&p).map(|n| (p, n)))
                {
                    let k = self.tmp.get();
                    self.tmp.set(k + 1);
                    let ids: Vec<String> =
                        (0..n).map(|i| self.map[&format!("{path}[{i}]")].0.to_string()).collect();
                    b.push_str(&format!(
                        "{ind}{{ static const uint32_t _a{k}[] = {{{}}};\n\
                         {ind}for (int _i{k} = 0; _i{k} < {n}; _i{k}++) {{ \
                         uint64_t {v} = sx_read(_a{k}[_i{k}]);\n",
                        ids.join(", ")
                    ));
                    let fresh = self.locals.borrow_mut().insert(v.clone());
                    for s in &body.stmts {
                        self.stmt(s, b, depth + 1)?;
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
                     {ind}int _st{k} = _lo{k} <= _hi{k} ? 1 : -1;\n\
                     {ind}for (int64_t _c{k} = _lo{k}; ; _c{k} += _st{k}) {{\n\
                     {ind}uint64_t {v} = (uint64_t)_c{k};\n"
                ));
                let fresh = self.locals.borrow_mut().insert(v.clone());
                for s in &body.stmts {
                    self.stmt(s, b, depth + 1)?;
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
        b.push_str(&format!("{ind}if ({c}) {{\n"));
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
                return Err("`tick()` was removed (it returns as a std function later); \
                            write `clk = '1'; await 5ns; clk = '0';` or start a clock \
                            generator (`clk = not clk after 5ns;`)"
                    .into());
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
                let mut cfmt = String::new();
                let mut cargs = Vec::new();
                let mut vals = args[1..].iter();
                let mut rest = text.as_str();
                while let Some(i) = rest.find("{}") {
                    cfmt.push_str(&c_escape(&rest[..i]).replace('%', "%%"));
                    if let Some(a) = vals.next() {
                        let sig = expr_path(a)
                            .and_then(|p| self.map.get(&p))
                            .map(|id| &self.design.signals[id.0 as usize]);
                        let is_real = sig.map(|s| s.real).unwrap_or_else(|| {
                            matches!(a, ast::Expr::Int { text, .. } if text.contains('.'))
                        });
                        // An enum-typed signal prints its variant symbol via a
                        // ternary over the value (a clang statement-expression
                        // evaluates the operand once).
                        // A signal's enum type, or an enum/Logic testbench
                        // local's declared type, selects symbol rendering.
                        let ety: Option<String> = sig
                            .and_then(|s| s.enum_type.clone())
                            .or_else(|| expr_path(a).and_then(|p| self.local_types.borrow().get(&p).cloned()));
                        let enum_syms = ety.as_ref().and_then(|e| self.design.enum_syms.get(e));
                        if let Some(syms) = enum_syms {
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
                        } else {
                            cfmt.push_str("%llu");
                            cargs.push(format!("(unsigned long long)({})", self.expr(a)?));
                        }
                    }
                    rest = &rest[i + 2..];
                }
                cfmt.push_str(&c_escape(rest).replace('%', "%%"));
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
                let msg = args.get(1).and_then(str_lit).unwrap_or_else(|| "assertion failed".into());
                let msg = c_escape(&msg);
                // Record the failure message and fail this test; `main` prints
                // the `test <name> ... FAILED` line and the message.
                b.push_str(&format!("{ind}if (!({c})) {{ g_msg = \"{msg}\"; return 1; }}\n"));
            }
            // warn!(cond, msg): non-fatal — report to stderr, keep running.
            "warn" if bang => {
                let cond = args.first().ok_or("warn needs a condition")?;
                let c = self.expr(cond)?;
                let msg = args.get(1).and_then(str_lit).unwrap_or_else(|| "warning".into());
                let msg = c_escape(&msg);
                b.push_str(&format!(
                    "{ind}if (!({c})) {{ fprintf(stderr, \"warning: {msg}\\n\"); g_warnings++; }}\n"
                ));
            }
            _ => {}
        }
        Ok(())
    }

    /// Element count of a DUT-connected array in the signal map.
    fn array_len(&self, path: &str) -> Option<u64> {
        let mut n = 0;
        while self.map.contains_key(&format!("{path}[{n}]")) {
            n += 1;
        }
        (n > 0).then_some(n)
    }

    /// Whether an operand reads a `Char` signal (so a `'x'` literal counterpart
    /// is a code point, not a logic code).
    fn is_char_operand(&self, e: &ast::Expr) -> bool {
        expr_path(e)
            .and_then(|p| self.map.get(&p))
            .map(|&id| self.design.signals[id.0 as usize].char)
            .unwrap_or(false)
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
            return self.enums.get(en).and_then(|m| m.get(&format!("'{ch}'"))).copied();
        }
        None
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
    fn is_real_operand(&self, e: &ast::Expr) -> bool {
        expr_path(e)
            .and_then(|p| self.map.get(&p))
            .map(|&id| self.design.signals[id.0 as usize].real)
            .unwrap_or(false)
    }

    /// An operand in a `real` comparison, as a C `double`: a real signal reads
    /// its bits, an integer/decimal literal is a float constant, anything else
    /// is cast.
    fn c_real_operand(&self, e: &ast::Expr) -> Result<String, String> {
        match e {
            ast::Expr::Int { text, .. } => Ok(format!("((double){text})")),
            _ if self.is_real_operand(e) => {
                let path = expr_path(e).ok_or("real operand must be a signal")?;
                let id = self.map.get(&path).ok_or_else(|| unsup(&path))?;
                Ok(format!("sx_f64(sx_read({}))", id.0))
            }
            _ => Ok(format!("((double)({}))", self.expr(e)?)),
        }
    }

    /// The per-element C read-expressions of a `Char` array (a string) operand:
    /// a connected string reads each element signal, a local reads its C
    /// element locals. `None` if `e` isn't an array-shaped name.
    fn c_string_elems(&self, e: &ast::Expr) -> Option<Vec<String>> {
        let path = expr_path(e)?;
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
        while self.locals.borrow().contains(&format!("{path}[{}]", elems.len())) {
            elems.push(c_local_ident(&format!("{path}[{}]", elems.len())));
        }
        (!elems.is_empty()).then_some(elems)
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
        let (elems, chars) = match (lit(lhs), lit(rhs)) {
            (Some(_), Some(_)) | (None, None) => return Ok(None), // two literals / no literal
            (None, Some(c)) => (self.c_string_elems(lhs), c),
            (Some(c), None) => (self.c_string_elems(rhs), c),
        };
        let Some(elems) = elems else { return Ok(None) };
        let eq = matches!(op, ast::BinOp::Eq);
        // A length mismatch is unequal — a constant either way.
        if elems.len() != chars.len() {
            return Ok(Some(if eq { "0".into() } else { "1".into() }));
        }
        let terms: Vec<String> = elems
            .iter()
            .zip(&chars)
            .map(|(e, &c)| format!("({e} == {}ULL)", c as u32))
            .collect();
        let all = if terms.is_empty() { "1".to_string() } else { terms.join(" && ") };
        Ok(Some(if eq { format!("({all})") } else { format!("(!({all}))") }))
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
             for (int _g = 0; _g < 1000000; _g++) {{ \
             if (!sx_step_clock(&_now, _next, _cid, _half, _nclk)) break; \
             uint64_t _c = sx_read({id}); if ({hit}) break; _p = _c; }} }}\n"
        ));
        Ok(())
    }

    fn emit_await(&self, args: &[ast::Expr], b: &mut String, depth: usize) -> Result<(), String> {
        let ind = "    ".repeat(depth);
        match args.first() {
            Some(ast::Expr::SuffixLit { .. }) | Some(ast::Expr::Field { .. }) => {
                let dur = duration_fs(args);
                b.push_str(&format!(
                    "{ind}{{ uint64_t _tgt = _now + {dur}ULL; \
                     while (sx_next_edge(_next, _nclk) <= _tgt) \
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
                    "{ind}for (int _g = 0; _g < 1000000 && !({c}); _g++) {{ \
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
                if let Ok(f) = text.parse::<f64>() {
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

    /// A `print!` real argument as a u64-bit-pattern C expression.
    fn value_for_print(&self, a: &ast::Expr) -> Result<String, String> {
        if let ast::Expr::Int { text, .. } = a {
            if let Ok(f) = text.parse::<f64>() {
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
        let stmts: Vec<ast::Stmt> =
            body.stmts.iter().map(|s| siox::ir::subst_stmt_paths(s, &map)).collect();
        self.c_fn_stmts(&stmts)
    }

    /// A module-fn call as a C expression: bind the arguments, then flatten
    /// the `return`/`if` body into nested conditionals.
    fn c_fn_call(&self, callee: &ast::Expr, args: &[ast::Expr]) -> Result<String, String> {
        let name = match callee {
            ast::Expr::Path(p) if p.segments.len() == 1 => p.segments[0].text.as_str(),
            _ => return Err("unsupported call in testbench expression".into()),
        };
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
                        "({{ FILE *_f = fopen(\"{full}\", \"rb\"); int _e = _f != 0; if (_f) fclose(_f); _e; }})"
                    ))
                }
                "read" | "read_to_string" => Err(format!(
                    "runtime `{name}()` is not compiled into the native binary yet; \
                     use it in initializer position (`let x: T[N] = {name}(..);`) \
                     or run with `sioxc test`"
                )),
                "rand" => Ok("sx_rand()".to_string()),
                "randint" => {
                    let lo = self.expr(args.first().ok_or("randint needs bounds")?)?;
                    let hi = self.expr(args.get(1).ok_or("randint needs bounds")?)?;
                    Ok(format!("(({lo}) + sx_rand() % ((({hi}) - ({lo})) + 1))"))
                }
                _ => Err(format!("unsupported call `{name}` in testbench expression")),
            };
        };
        let f = f;
        let body = f.body.as_ref().ok_or("fn has no body")?;
        // Constant arguments fold statically (also the only way a recursive
        // fn like clog2 compiles here).
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
        if self.tmp.get() > 4096 {
            return Err(format!("fn `{name}` recurses without constant arguments"));
        }
        self.tmp.set(self.tmp.get() + 64);
        let mut env = HashMap::new();
        for (p, a) in f.params.iter().filter(|p| !p.is_self).zip(args) {
            if let Some(n) = &p.name {
                env.insert(n.text.clone(), format!("({})", self.expr(a)?));
            }
        }
        self.fn_env.borrow_mut().push(env);
        let out = self.c_fn_stmts(&body.stmts);
        self.fn_env.borrow_mut().pop();
        self.tmp.set(self.tmp.get() - 64);
        out
    }

    /// `return e;` / `if c { .. } else { .. }` chains as nested C ternaries.
    fn c_fn_stmts(&self, stmts: &[ast::Stmt]) -> Result<String, String> {
        match stmts.first() {
            Some(ast::Stmt::Return { value: Some(v), .. }) => {
                Ok(format!("({})", self.expr(v)?))
            }
            Some(ast::Stmt::If(iff)) => {
                let c = self.expr(&iff.cond)?;
                let t = self.c_fn_stmts(&iff.then.stmts)?;
                let e = match iff.else_.as_deref() {
                    Some(ast::ElseBranch::Block(b)) => self.c_fn_stmts(&b.stmts)?,
                    _ => self.c_fn_stmts(&stmts[1..])?,
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
                let (mask, value) =
                    siox::ir::bit_pattern_mask(text).ok_or_else(|| format!("bad bit pattern `{text}`"))?;
                Some(format!("((({scrut}) & {mask}ULL) == {value}ULL)"))
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
            ast::Expr::IfExpr { cond, then, els, .. } => {
                format!("(({}) ? ({}) : ({}))", self.expr(cond)?, self.expr(then)?, self.expr(els)?)
            }
            // A match-expression: a first-match C ternary chain over the arms.
            ast::Expr::Match { scrutinee, arms, .. } => {
                let scrut = self.expr(scrutinee)?;
                // Build from the last arm backward.
                let mut out = String::from("0");
                for arm in arms.iter().rev() {
                    let val = match arm.value_expr() {
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
            ast::Expr::Int { text, .. } => format!("{}ULL", parse_u64(text)),
            ast::Expr::SuffixLit { text, .. } => format!("{}ULL", parse_u64(text)),
            ast::Expr::Bool { value, .. } => (*value as u64).to_string(),
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
                            || matches!(n, "exists" | "rand" | "randint" | "read" | "read_to_string")
                    }) =>
            {
                return self.c_fn_call(callee, args);
            }
            ast::Expr::Call { callee, args, .. } => {
                let arg = args.first().ok_or("conversion needs an argument")?;
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
                                c.trim_end_matches("ULL")
                                    .parse()
                                    .map_err(|_| "resize width must be a constant here".to_string())?
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
                if w == 0 || w >= 64 {
                    format!("({v})")
                } else {
                    format!("(({v}) & {}ULL)", (1u64 << w) - 1)
                }
            }
            // `::length`: an impl body's bound length (`self::length`), an
            // array's element count, or a name's bit width (they coincide for a
            // flat vector) — VHDL `'length`.
            ast::Expr::SysAttr { base, attr, .. } if attr.text == "length" => {
                let path = expr_path(base).ok_or("::length needs a named base")?;
                if let Some(v) =
                    self.fn_env.borrow().last().and_then(|m| m.get(&format!("{path}::length")))
                {
                    return Ok(v.clone());
                }
                if let Some(n) = self.array_len(&path) {
                    return Ok(format!("{n}ULL"));
                }
                format!("{}ULL", self.name_width(&path).ok_or("unknown ::length")?)
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
                let w = expr_path(base).and_then(|p| self.name_width(&p)).unwrap_or(0) as i64;
                let v = match attr.text.as_str() {
                    "left" | "low" => 0,
                    "right" | "high" => (w - 1).max(0),
                    "ascending" => 1,
                    _ => unreachable!(),
                };
                format!("{v}ULL")
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
                } else if let Some(&v) = self.consts.get(&p.segments[0].text) {
                    format!("{}ULL", v as u64)
                } else {
                    return Err(unsup(&p.segments[0].text));
                }
            }
            ast::Expr::Path(p) if p.segments.len() >= 2 => {
                // Enum::Variant -> discriminant.
                let d = self
                    .enums
                    .get(&p.segments[0].text)
                    .and_then(|m| m.get(&p.segments[1].text))
                    .ok_or_else(|| unsup(&p.segments[1].text))?;
                format!("{d}ULL")
            }
            ast::Expr::Field { .. } | ast::Expr::Index { .. } => {
                let path = expr_path(e).ok_or("unsupported field/index")?;
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
                    if let Some(en) = self.enum_operand_type(lhs).or_else(|| self.enum_operand_type(rhs)) {
                        let a = self.enum_operand_c(&en, lhs)?;
                        let b = self.enum_operand_c(&en, rhs)?;
                        return Ok(format!("({a} {} {b})", c_binop(op)?));
                    }
                }
                // A `real` operand switches to double semantics: reals read
                // their bits as `double`, integer literals coerce (`z.re == 10`
                // compares 10.0). A comparison yields an int; arithmetic yields
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
                    return Ok(if is_cmp { e } else { format!("sx_b64((double){e})") });
                }
                // A typed operand inlines its family's operator impl (int's
                // signed Div/Ord), matching the runner.
                if let Some(v) = self.c_dispatch_binop(op, lhs, rhs)? {
                    return Ok(v);
                }
                let (a, o, c) = (self.expr(lhs)?, c_binop(op)?, self.expr(rhs)?);
                format!("({a} {o} {c})")
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

    /// Reject `real` signals in scalar expressions — native stimulus is
    /// integer-word only for now. A `Char` reads as its code point (a plain
    /// integer), so it is allowed.
    fn check_scalar(&self, id: SignalId) -> Result<(), String> {
        let s = &self.design.signals[id.0 as usize];
        if s.real {
            return Err(format!(
                "signal `{}` is real; siox build does not support real testbenches \
                 yet (use `siox test`)",
                s.path
            ));
        }
        Ok(())
    }
}

fn unsup(name: &str) -> String {
    format!("testbench references `{name}`, which siox build cannot translate yet")
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
fn after_toggle(target: &ast::Expr, value: &ast::Expr, after: &Option<ast::Expr>) -> Option<(String, u64)> {
    let delay = after.as_ref()?;
    let path = expr_path(target)?;
    if let ast::Expr::Unary { op: ast::UnOp::Not, rhs, .. } = value {
        if expr_path(rhs).as_deref() == Some(path.as_str()) {
            let half = duration_fs(std::slice::from_ref(delay)).max(1);
            return Some((path, half));
        }
    }
    None
}

/// Collect the background clocks in a test's body — `clock(clk, period)` calls
/// and the VHDL-style `clk = !clk after half;` idiom: (signal id, half fs).
fn scan_clocks(
    items: &[&ast::ImplItem],
    aliases: &HashMap<String, Vec<SignalId>>,
) -> Vec<(u32, u64)> {
    let mut clocks: Vec<(u32, u64)> = Vec::new();
    let mut add = |id: u32, half: u64| {
        if !clocks.iter().any(|(c, _)| *c == id) {
            clocks.push((id, half));
        }
    };
    for item in items {
        match item {
            ast::ImplItem::Stmt(ast::Stmt::Assign { target, value, after, .. }) => {
                if let Some((path, half)) = after_toggle(target, value, after) {
                    // A clock shared by several DUTs toggles every port.
                    for id in aliases.get(&path).map(|v| v.as_slice()).unwrap_or(&[]) {
                        add(id.0, half);
                    }
                }
            }
            _ => {}
        }
    }
    clocks
}

/// The femtosecond duration of `10ns` / `10.ns`; a missing/unknown form
/// defaults to the runner's half period (5 ns).
fn duration_fs(args: &[ast::Expr]) -> u64 {
    match args.first() {
        Some(ast::Expr::SuffixLit { text, suffix, .. }) => {
            parse_u64(text) * ast::suffix_scale(&suffix.text).unwrap_or(1_000_000) as u64
        }
        Some(ast::Expr::Field { base, field, .. }) => {
            if let ast::Expr::Int { text, .. } = base.as_ref() {
                parse_u64(text) * ast::suffix_scale(&field.text).unwrap_or(1_000_000) as u64
            } else {
                5_000_000
            }
        }
        _ => 5_000_000,
    }
}

fn is_test_entity(modules: &[Module], entity: &str) -> bool {
    for m in modules {
        for it in &m.items {
            if let ast::Item::Entity(e) = it {
                if e.name.text == entity {
                    return e.attrs.iter().any(|a| {
                        a.name.segments.last().map(|s| s.text.as_str()) == Some("test")
                    });
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
        ast::Type::Mode { inner, .. } => type_head_name(inner),
    }
}

fn expr_path(e: &ast::Expr) -> Option<String> {
    match e {
        ast::Expr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].text.clone()),
        ast::Expr::Field { base, field, .. } => Some(format!("{}.{}", expr_path(base)?, field.text)),
        ast::Expr::Index { base, index, .. } => match index.as_ref() {
            ast::Expr::Int { text, .. } => Some(format!("{}[{}]", expr_path(base)?, parse_u64(text))),
            _ => None,
        },
        _ => None,
    }
}

fn str_lit(e: &ast::Expr) -> Option<String> {
    match e {
        ast::Expr::StrLit { text, .. } => Some(text.clone()),
        _ => None,
    }
}

fn parse_u64(text: &str) -> u64 {
    let t = text.trim();
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).unwrap_or(0)
    } else if let Some(bin) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        u64::from_str_radix(bin, 2).unwrap_or(0)
    } else {
        t.parse().unwrap_or(0)
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
