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

    // Header, one `int test_<name>(void)` per test, then a libtest-style main.
    let mut prog = String::new();
    prog.push_str("#include <stdint.h>\n#include <stdio.h>\n#include <string.h>\n");
    prog.push_str("extern void sx_reset(void);\n");
    prog.push_str("extern void sx_set(uint32_t, uint64_t);\n");
    prog.push_str("extern uint64_t sx_read(uint32_t);\n");
    prog.push_str("extern void sx_settle(void);\n");
    prog.push_str("static const char *g_msg;\n");
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

    let mut names = Vec::new();
    for &root in &tests {
        let name = hier.instance(root).entity.clone();
        let map = build_map(hier, root, design);
        let items = test_items(modules, &name);
        let clocks = scan_clocks(&items, &map);
        let ctx = Ctx { design, map: &map, enums: &enums, name: &name, clocks };
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
        .args([csrc.to_str().unwrap(), obj.to_str().unwrap(), "-O2", "-o", out.to_str().unwrap()])
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
        "    printf(\"\\ntest result: %s. %d passed; %d failed; %d filtered out\\n\",\n\
         \x20          failed ? \"FAILED\" : \"ok\", ran - failed, failed, filtered);\n",
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
    name: &'a str,
    /// `clock(clk, ..)`-registered background clocks: (signal id, half period fs).
    clocks: Vec<(u32, u64)>,
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

        // Initial `let` values, then settle (mirrors the interpreter).
        for item in items {
            if let ast::ImplItem::Let(l) = item {
                match &l.value {
                    Some(ast::Expr::Construct { ty: Some(_), .. }) => {} // instance
                    Some(v) => {
                        if let Some(&id) = self.map.get(&l.name.text) {
                            let e = self.expr(v)?;
                            b.push_str(&format!("    sx_set({}, {e});\n", id.0));
                        }
                    }
                    None => {}
                }
            }
        }
        b.push_str("    sx_settle();\n");

        // Stimulus statements.
        for item in items {
            if let ast::ImplItem::Stmt(s) = item {
                self.stmt(s, &mut b, 1)?;
            }
        }

        b.push_str("    return 0;\n}\n\n");
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
                let id = *self.map.get(&name).ok_or_else(|| format!("unknown signal `{name}`"))?;
                let e = self.expr(value)?;
                b.push_str(&format!("{ind}sx_set({}, {e});\n{ind}sx_settle();\n", id.0));
            }
            ast::Stmt::Expr(ast::Expr::Call { callee, args, bang, .. }) => {
                self.call(callee, args, *bang, b, depth)?;
            }
            ast::Stmt::For { var, range, body, .. } => {
                let (lo, hi) = match range {
                    ast::Expr::Range { lo, hi, .. } => (self.expr(lo)?, self.expr(hi)?),
                    _ => return Err("`for` needs a range".into()),
                };
                let v = &var.text;
                b.push_str(&format!("{ind}for (uint64_t {v} = {lo}; {v} < {hi}; {v}++) {{\n"));
                for s in &body.stmts {
                    self.stmt(s, b, depth + 1)?;
                }
                b.push_str(&format!("{ind}}}\n"));
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
            "tick" => {
                let clk = args.first().and_then(expr_path).ok_or("tick needs a signal")?;
                let id = self.map.get(&clk).ok_or_else(|| format!("unknown clock `{clk}`"))?.0;
                b.push_str(&format!(
                    "{ind}sx_set({id}, 1); sx_settle();\n{ind}sx_set({id}, 0); sx_settle();\n"
                ));
            }
            "wait" => {
                // Advance time without running the clocks (matches the runner).
                b.push_str(&format!("{ind}_now += {}ULL; sx_settle();\n", duration_fs(args)));
            }
            // clock(clk, period): register a background clock on the wheel
            // (init low; first toggle one half period from now).
            "clock" => {
                if let Some(id) = args.first().and_then(expr_path).and_then(|p| self.map.get(&p)) {
                    if let Some(i) = self.clocks.iter().position(|(c, _)| *c == id.0) {
                        b.push_str(&format!(
                            "{ind}sx_set({}, 0); _next[{i}] = _now + {}ULL;                              _nclk = {}; sx_settle();\n",
                            id.0,
                            self.clocks[i].1,
                            i + 1
                        ));
                    }
                }
            }
            // await <duration> | <edge> | <condition>.
            "await" => self.emit_await(args, b, depth)?,
            "assert" if bang => {
                let cond = args.first().ok_or("assert needs a condition")?;
                let c = self.expr(cond)?;
                let msg = args.get(1).and_then(str_lit).unwrap_or_else(|| "assertion failed".into());
                let msg = msg.replace('\\', "\\\\").replace('"', "\\\"");
                // Record the failure message and fail this test; `main` prints
                // the `test <name> ... FAILED` line and the message.
                b.push_str(&format!("{ind}if (!({c})) {{ g_msg = \"{msg}\"; return 1; }}\n"));
            }
            _ => {}
        }
        Ok(())
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

    /// Translate a testbench expression to a C expression string.
    fn expr(&self, e: &ast::Expr) -> Result<String, String> {
        Ok(match e {
            ast::Expr::Int { text, .. } => format!("{}ULL", parse_u64(text)),
            ast::Expr::SuffixLit { text, .. } => format!("{}ULL", parse_u64(text)),
            ast::Expr::Bool { value, .. } => (*value as u64).to_string(),
            ast::Expr::LogicLit { ch, .. } => logic_value(*ch).to_string(),
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
        And => "&&",
        Or => "||",
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
            ast::ImplItem::Stmt(ast::Stmt::Expr(ast::Expr::Call { callee, args, .. })) => {
                let is_clock = matches!(callee.as_ref(),
                    ast::Expr::Path(p) if p.segments.first().map(|s| s.text.as_str()) == Some("clock"));
                if is_clock {
                    if let Some(id) = args.first().and_then(expr_path).and_then(|p| map.get(&p)) {
                        add(id.0, (duration_fs(&args[1..]) / 2).max(1));
                    }
                }
            }
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
