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

use siox_elab::{Hierarchy, InstanceId};
use siox_ir::{Design, SignalId};
use siox_syntax::ast;
use siox_syntax::Module;

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
    let enums = enum_discriminants(modules);
    let families = siox_ir::vector_families(modules);
    let mut fns: HashMap<String, &ast::FnDecl> = HashMap::new();
    for m in modules {
        for item in &m.items {
            if let ast::Item::Fn(f) = item {
                fns.insert(f.name.text.clone(), f);
            }
        }
    }

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
    let ranged: Vec<(u32, &siox_ir::Signal)> = design
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
        let map = build_map(hier, root, design);
        let items = test_items(modules, &name);
        let clocks = scan_clocks(&items, &map);
        let ctx = Ctx {
            design,
            map: &map,
            enums: &enums,
            families: &families,
            name: &name,
            clocks,
            locals: Default::default(),
            tmp: Default::default(),
            fns: &fns,
            fn_env: Default::default(),
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
    std::fs::write(&csrc, prog).map_err(|e| e.to_string())?;
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
    /// Unique-suffix counter for generated C identifiers.
    tmp: std::cell::Cell<usize>,
    /// Module-level functions (testbench-callable; translated to C ternaries).
    fns: &'a HashMap<String, &'a ast::FnDecl>,
    /// Parameter-substitution stack while translating a fn body.
    fn_env: std::cell::RefCell<Vec<HashMap<String, String>>>,
}

impl Ctx<'_> {
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
                ast::ImplItem::Let(l) => match &l.value {
                    Some(ast::Expr::Construct { ty: Some(_), .. }) => {} // instance
                    value => {
                        if let Some(&id) = self.map.get(&l.name.text) {
                            if let Some(v) = value {
                                let e = self.value_for(id, v)?;
                                b.push_str(&format!("    sx_set({}, {e});\n", id.0));
                            }
                        } else {
                            let e = match value {
                                Some(v) => self.expr(v)?,
                                None => "0".to_string(),
                            };
                            b.push_str(&format!("    uint64_t {} = {e};\n", l.name.text));
                            self.locals.borrow_mut().insert(l.name.text.clone());
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
                    let id = self.map.get(&path).ok_or_else(|| format!("unknown signal `{path}`"))?;
                    if let Some(i) = self.clocks.iter().position(|(c, _)| *c == id.0) {
                        b.push_str(&format!(
                            "{ind}_next[{i}] = _now + {}ULL; _nclk = {}; sx_settle();\n",
                            self.clocks[i].1,
                            i + 1
                        ));
                    }
                    return Ok(());
                }
                let name = expr_path(target).ok_or("unsupported assignment target")?;
                if self.locals.borrow().contains(&name) {
                    let e = self.expr(value)?;
                    b.push_str(&format!("{ind}{name} = {e};\n"));
                    return Ok(());
                }
                let id = *self.map.get(&name).ok_or_else(|| format!("unknown signal `{name}`"))?;
                let e = self.value_for(id, value)?;
                b.push_str(&format!("{ind}sx_set({}, {e});\n{ind}sx_settle();\n", id.0));
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
            ast::Stmt::If(iff) => {
                let c = self.expr(&iff.cond)?;
                b.push_str(&format!("{ind}if ({c}) {{\n"));
                for s in &iff.then.stmts {
                    self.stmt(s, b, depth + 1)?;
                }
                b.push_str(&format!("{ind}}}\n"));
                if let Some(ast::ElseBranch::Block(block)) = iff.else_.as_deref() {
                    b.push_str(&format!("{ind}else {{\n"));
                    for s in &block.stmts {
                        self.stmt(s, b, depth + 1)?;
                    }
                    b.push_str(&format!("{ind}}}\n"));
                }
            }
            _ => {}
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
                    cfmt.push_str(&rest[..i].replace('%', "%%").replace('"', "\\\""));
                    if let Some(a) = vals.next() {
                        let is_real = expr_path(a)
                            .and_then(|p| self.map.get(&p))
                            .map(|id| self.design.signals[id.0 as usize].real)
                            .unwrap_or_else(|| {
                                matches!(a, ast::Expr::Int { text, .. } if text.contains('.'))
                            });
                        if is_real {
                            cfmt.push_str("%g");
                            cargs.push(format!("sx_f64({})", self.value_for_print(a)?));
                        } else {
                            cfmt.push_str("%llu");
                            cargs.push(format!("(unsigned long long)({})", self.expr(a)?));
                        }
                    }
                    rest = &rest[i + 2..];
                }
                cfmt.push_str(&rest.replace('%', "%%").replace('"', "\\\""));
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
                let msg = msg.replace('\\', "\\\\").replace('"', "\\\"");
                // Record the failure message and fail this test; `main` prints
                // the `test <name> ... FAILED` line and the message.
                b.push_str(&format!("{ind}if (!({c})) {{ g_msg = \"{msg}\"; return 1; }}\n"));
            }
            // warn!(cond, msg): non-fatal — report to stderr, keep running.
            "warn" if bang => {
                let cond = args.first().ok_or("warn needs a condition")?;
                let c = self.expr(cond)?;
                let msg = args.get(1).and_then(str_lit).unwrap_or_else(|| "warning".into());
                let msg = msg.replace('\\', "\\\\").replace('"', "\\\"");
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

    /// `await <duration> | <edge> | <condition>` in the native harness, on the
    /// generated event wheel: a duration runs the clocks up to `now + dur`; an
    /// edge or condition steps clock edges until it fires (bounded, mirroring
    /// the runner's scheduler).
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
                let id = expr_path(base)
                    .and_then(|p| self.map.get(&p))
                    .ok_or("await: unknown edge signal")?
                    .0;
                let hit = match attr.text.as_str() {
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
            if let ast::Expr::LogicLit { ch, .. } = e {
                return Ok(format!("{}ULL", *ch as u32));
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
                    Ok(format!(
                        "({{ FILE *_f = fopen(\"{path}\", \"rb\"); int _e = _f != 0; if (_f) fclose(_f); _e; }})"
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
            .map(|a| siox_ir::eval_const_fns(a, &HashMap::new(), self.fns, 0))
            .collect();
        if let Some(cs) = consts {
            let mut cenv = HashMap::new();
            for (p, v) in f.params.iter().filter(|p| !p.is_self).zip(cs) {
                if let Some(n) = &p.name {
                    cenv.insert(n.text.clone(), v);
                }
            }
            if let Some(v) = siox_ir::eval_const_stmts(&body.stmts, &cenv, self.fns, 0) {
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
    fn expr(&self, e: &ast::Expr) -> Result<String, String> {
        Ok(match e {
            ast::Expr::IfExpr { cond, then, els, .. } => {
                format!("(({}) ? ({}) : ({}))", self.expr(cond)?, self.expr(then)?, self.expr(els)?)
            }
            ast::Expr::Int { text, .. } => format!("{}ULL", parse_u64(text)),
            ast::Expr::SuffixLit { text, .. } => format!("{}ULL", parse_u64(text)),
            ast::Expr::Bool { value, .. } => (*value as u64).to_string(),
            ast::Expr::LogicLit { ch, .. } => logic_value(*ch).to_string(),
            // Conversions mask to the target width (testbench side).
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
                            _ => return Err("resize width must be a constant here".into()),
                        }
                    }
                    ast::Expr::Path(p)
                        if p.segments.len() == 1
                            && matches!(p.segments[0].text.as_str(), "integer" | "Char") =>
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
            ast::Expr::SysAttr { base, attr, .. } if attr.text == "len" => {
                let n = expr_path(base)
                    .and_then(|p| self.array_len(&p))
                    .ok_or("::len needs a connected array")?;
                format!("{n}ULL")
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
                p.segments[0].text.clone()
            }
            ast::Expr::Path(p) if p.segments.len() == 1 => {
                let id =
                    self.map.get(&p.segments[0].text).ok_or_else(|| unsup(&p.segments[0].text))?;
                format!("sx_read({})", id.0)
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
                let id = self.map.get(&path).ok_or_else(|| unsup(&path))?;
                self.check_scalar(*id)?;
                format!("sx_read({})", id.0)
            }
            ast::Expr::Unary { op, rhs, .. } => {
                let r = self.expr(rhs)?;
                match op {
                    ast::UnOp::Not => format!("(!({r}))"),
                    ast::UnOp::Neg => format!("(-({r}))"),
                }
            }
            ast::Expr::Binary { op, lhs, rhs, .. } => {
                let (a, o, c) = (self.expr(lhs)?, c_binop(*op)?, self.expr(rhs)?);
                format!("({a} {o} {c})")
            }
            _ => return Err("unsupported testbench expression".into()),
        })
    }

    /// Reject real/char signals in expressions — the first cut is integer only.
    fn check_scalar(&self, id: SignalId) -> Result<(), String> {
        let s = &self.design.signals[id.0 as usize];
        if s.real || s.char {
            return Err(format!(
                "signal `{}` is {}; siox build does not support real/char/string \
                 testbenches yet (use `siox test`)",
                s.path,
                if s.real { "real" } else { "Char" }
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
fn c_binop(op: ast::BinOp) -> Result<&'static str, String> {
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
        Xor => "^",
        _ => return Err("unsupported operator in testbench expression".into()),
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
fn scan_clocks(items: &[&ast::ImplItem], map: &HashMap<String, SignalId>) -> Vec<(u32, u64)> {
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
                    if let Some(id) = map.get(&path) {
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

fn build_map(hier: &Hierarchy, root: InstanceId, design: &Design) -> HashMap<String, SignalId> {
    // DUTs lower per-instance under the testbench path (`<test>.<inst>.<port>`),
    // so two instances of one entity stay distinct (matches siox-run's map).
    let tb = &hier.instance(root).entity;
    let mut map = HashMap::new();
    for &child_id in &hier.instance(root).children {
        let child = hier.instance(child_id);
        for c in &child.connections {
            let prefix = format!("{tb}.{}.{}", child.name, c.port);
            for (i, sig) in design.signals.iter().enumerate() {
                let id = SignalId(i as u32);
                if sig.path == prefix {
                    map.insert(c.signal.clone(), id);
                } else if let Some(suffix) = sig.path.strip_prefix(&prefix) {
                    if suffix.starts_with('.') || suffix.starts_with('[') {
                        map.insert(format!("{}{suffix}", c.signal), id);
                    }
                }
            }
        }
    }
    map
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

fn enum_discriminants(modules: &[Module]) -> HashMap<String, HashMap<String, u64>> {
    let mut out = HashMap::new();
    for m in modules {
        for it in &m.items {
            if let ast::Item::Enum(e) = it {
                let mut vars = HashMap::new();
                let mut next = 0u64;
                for v in &e.variants {
                    let d = match &v.value {
                        Some(ast::Expr::Int { text, .. }) => parse_u64(text),
                        _ => next,
                    };
                    vars.insert(v.name.text.clone(), d);
                    next = d + 1;
                }
                out.insert(e.name.text.clone(), vars);
            }
        }
    }
    out
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

fn logic_value(c: char) -> u64 {
    match c {
        '1' | 'H' => 1,
        'Z' => 2,
        'X' | 'U' | 'W' => 3,
        _ => 0,
    }
}
